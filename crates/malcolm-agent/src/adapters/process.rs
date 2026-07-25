//! Process-control adapter — the first real-world adapter in
//! `malcolm-agent`. Implements process termination, arbitrary signal
//! delivery, and pause/resume via `SIGSTOP`/`SIGCONT`.
//!
//! # Feature gating
//!
//! This module is compiled only on Unix with the `process` feature
//! enabled. The default build of `malcolm-agent` cannot terminate
//! anything.
//!
//! # Safety contract
//!
//! Every action goes through [`SafetyGuard::check_target`] first.
//! The guard refuses pid 1, the current process, and the parent
//! process by construction; the caller must additionally have
//! added the target pid to the pid allowlist. The guard's arming
//! state is also checked: an unarmed guard returns a `dry_run: true`
//! `AppliedFault` and performs no signal delivery.
//!
//! # Reversibility
//!
//! - `Signal`, `Terminate` — irreversible. The `AppliedFault` records
//!   the action for auditing; `revert` is a documented no-op (the
//!   signal cannot be un-delivered).
//! - `Pause` — reversible. The `AppliedFault` records that the pid
//!   was stopped; `revert` sends `SIGCONT`, so a paused process is
//!   always resumed by [`crate::cleanup::Cleanup`] on `Drop` and on
//!   `SIGINT`/`SIGTERM`.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use nix::sys::signal::{self, Signal as NixSignal};
use nix::unistd::Pid;

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

/// Actions the process adapter understands. The adapter's `apply`
/// method decodes the `FaultPlan::payload` (a JSON object) into one
/// of these variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessAction {
    /// Send an arbitrary signal to a pid. The signal must be one of
    /// the well-known values exposed by [`nix::sys::signal::Signal`]
    /// (encoded as a string, e.g. `"SIGUSR1"`, `"SIGHUP"`).
    Signal {
        /// Target pid.
        pid: u32,
        /// Signal name (e.g. `"SIGUSR1"`).
        signal: String,
    },
    /// Graceful terminate — send `SIGTERM`, wait up to `grace` for
    /// the process to exit, escalate to `SIGKILL` if still alive.
    /// The total wait is capped at `grace`; the adapter does not
    /// busy-spin.
    Terminate {
        /// Target pid.
        pid: u32,
        /// Time to wait between `SIGTERM` and `SIGKILL`.
        grace: Duration,
    },
    /// Pause a process via `SIGSTOP`. Reversible: the adapter's
    /// `revert` sends `SIGCONT`.
    Pause {
        /// Target pid to stop.
        pid: u32,
    },
    /// Resume a previously-paused process via `SIGCONT`.
    Resume {
        /// Target pid to continue.
        pid: u32,
    },
}

impl ProcessAction {
    /// Stable identifier for the action, used in tracing events and
    /// the `AppliedFault::description`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Signal { .. } => "process_signal",
            Self::Terminate { .. } => "process_terminate",
            Self::Pause { .. } => "process_pause",
            Self::Resume { .. } => "process_resume",
        }
    }

    /// Decode a `ProcessAction` from the JSON payload of a
    /// `FaultPlan`. Returns [`AgentError::InvalidPlan`] on any
    /// shape mismatch rather than guessing.
    ///
    /// # Errors
    ///
    /// - `InvalidPlan` if the payload is not a JSON object.
    /// - `InvalidPlan` if a required field is missing or has the
    ///   wrong type.
    /// - `InvalidPlan` if the `kind` discriminator is unknown.
    /// - `InvalidPlan` if the signal name in `Signal` is not one
    ///   of `nix`'s recognised signal names.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, AgentError> {
        let obj = payload.as_object().ok_or_else(|| AgentError::InvalidPlan {
            adapter: ProcessAdapter::KIND,
            reason: "payload must be a JSON object".to_owned(),
        })?;
        let kind = obj
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: ProcessAdapter::KIND,
                reason: "missing or non-string field `kind`".to_owned(),
            })?;
        let pid = obj
            .get("pid")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: ProcessAdapter::KIND,
                reason: "missing or non-u64 field `pid`".to_owned(),
            })?;
        let pid = u32::try_from(pid).map_err(|_| AgentError::InvalidPlan {
            adapter: ProcessAdapter::KIND,
            reason: format!("pid {pid} does not fit in u32"),
        })?;
        match kind {
            "signal" => {
                let signal_name = obj
                    .get("signal")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: ProcessAdapter::KIND,
                        reason: "missing or non-string field `signal`".to_owned(),
                    })?;
                // Validate the signal name up front so the liveness
                // probe does not short-circuit an invalid plan with
                // an AdapterFailure. The adapter-level dispatch
                // resolves the string to a NixSignal again — this
                // is a pure validation step.
                ProcessAdapter::parse_signal(signal_name)?;
                Ok(Self::Signal {
                    pid,
                    signal: signal_name.to_owned(),
                })
            }
            "terminate" => {
                let grace_ms = obj
                    .get("grace_ms")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: ProcessAdapter::KIND,
                        reason: "missing or non-u64 field `grace_ms`".to_owned(),
                    })?;
                Ok(Self::Terminate {
                    pid,
                    grace: Duration::from_millis(grace_ms),
                })
            }
            "pause" => Ok(Self::Pause { pid }),
            "resume" => Ok(Self::Resume { pid }),
            other => Err(AgentError::InvalidPlan {
                adapter: ProcessAdapter::KIND,
                reason: format!("unknown action kind `{other}`"),
            }),
        }
    }
}

/// The process-control adapter. Each `apply` call sends at most one
/// signal to one pid; batch operations are the caller's
/// responsibility.
#[derive(Debug, Default)]
pub struct ProcessAdapter {
    /// Monotonic counter for the dry-run / applied ids the adapter
    /// hands out. Distinct from the cleanup registry's id so an
    /// adapter-internal id never collides with a registry id.
    next_id: AtomicU64,
    /// Map from adapter id → `(pid, action)` for every applied
    /// fault. `revert` consults this map to decide which signal (if
    /// any) to send on cleanup. The map is wrapped in a `Mutex` so
    /// `revert` can satisfy the `Send + Sync` bound on
    /// `TargetAdapter`.
    applied: Mutex<HashMap<u64, (u32, ProcessAction)>>,
}

impl ProcessAdapter {
    /// Adapter kind string used in `adapter_kind()` and the
    /// `AgentError::InvalidPlan` reason. Exposed as a constant so
    /// tests and adapters can compare against it without hard-coding.
    pub const KIND: &'static str = "process";

    /// Construct a new `ProcessAdapter` with its id counter at zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            applied: Mutex::new(HashMap::new()),
        }
    }

    /// Number of applied faults currently tracked. Used by tests to
    /// confirm the adapter's internal bookkeeping matches the
    /// `Cleanup` registry's count.
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Probe whether a pid is currently runnable on the host
    /// (`kill(pid, None)`). Returns `Ok(true)` when the process
    /// exists (even if we lack permission to signal it), `Ok(false)`
    /// when no such process exists, and `Err` for any other
    /// failure.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::AdapterFailure`] if the probe itself
    /// fails for a reason other than ESRCH (no such process).
    fn probe_live(pid: u32) -> Result<bool, AgentError> {
        let npid = pid_to_nix(pid)?;
        match signal::kill(npid, None) {
            Ok(()) => Ok(true),
            Err(nix::errno::Errno::ESRCH) => Ok(false),
            Err(e) => Err(AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("kill probe failed: {e}"),
            }),
        }
    }

    /// Translate a string signal name (e.g. `"SIGUSR1"`) into a
    /// `NixSignal`. Returns `AgentError::InvalidPlan` on an unknown
    /// name rather than guessing a default.
    fn parse_signal(name: &str) -> Result<NixSignal, AgentError> {
        // Nix's `Signal::from_str` accepts the canonical names. We
        // route through a small lookup to surface a stable error
        // message that includes the rejected name.
        const NAMES: &[(&str, NixSignal)] = &[
            ("SIGHUP", NixSignal::SIGHUP),
            ("SIGINT", NixSignal::SIGINT),
            ("SIGQUIT", NixSignal::SIGQUIT),
            ("SIGILL", NixSignal::SIGILL),
            ("SIGTRAP", NixSignal::SIGTRAP),
            ("SIGABRT", NixSignal::SIGABRT),
            ("SIGBUS", NixSignal::SIGBUS),
            ("SIGFPE", NixSignal::SIGFPE),
            ("SIGKILL", NixSignal::SIGKILL),
            ("SIGUSR1", NixSignal::SIGUSR1),
            ("SIGUSR2", NixSignal::SIGUSR2),
            ("SIGSEGV", NixSignal::SIGSEGV),
            ("SIGPIPE", NixSignal::SIGPIPE),
            ("SIGALRM", NixSignal::SIGALRM),
            ("SIGTERM", NixSignal::SIGTERM),
            ("SIGCHLD", NixSignal::SIGCHLD),
            ("SIGCONT", NixSignal::SIGCONT),
            ("SIGSTOP", NixSignal::SIGSTOP),
            ("SIGTSTP", NixSignal::SIGTSTP),
            ("SIGTTIN", NixSignal::SIGTTIN),
            ("SIGTTOU", NixSignal::SIGTTOU),
            ("SIGURG", NixSignal::SIGURG),
            ("SIGXCPU", NixSignal::SIGXCPU),
            ("SIGXFSZ", NixSignal::SIGXFSZ),
            ("SIGVTALRM", NixSignal::SIGVTALRM),
            ("SIGPROF", NixSignal::SIGPROF),
            ("SIGWINCH", NixSignal::SIGWINCH),
            ("SIGIO", NixSignal::SIGIO),
            ("SIGSYS", NixSignal::SIGSYS),
        ];
        // Note: `SIGPWR` is not present on every nix target — Linux
        // exposes it on some architectures, macOS does not. We omit
        // it from the lookup and return InvalidPlan for that name.
        NAMES
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, s)| *s)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: Self::KIND,
                reason: format!("unknown signal name `{name}`"),
            })
    }
}

/// Convert a u32 pid to a `nix::unistd::Pid`. Returns
/// `AgentError::AdapterFailure` on values that don't fit in the
/// platform's `pid_t` (`i32` on every Unix `nix` target).
fn pid_to_nix(pid: u32) -> Result<Pid, AgentError> {
    let raw = i32::try_from(pid).map_err(|_| AgentError::AdapterFailure {
        adapter: ProcessAdapter::KIND,
        reason: format!("pid {pid} does not fit in i32 on this platform"),
    })?;
    Ok(Pid::from_raw(raw))
}

impl TargetAdapter for ProcessAdapter {
    #[expect(
        clippy::too_many_lines,
        reason = "apply threads every ProcessAction variant through one path"
    )]
    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        // Dry-run-first: if the guard is not armed, record the
        // would-have action and return without touching the host.
        if !guard.is_armed() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let description = format!("{} (dry-run; guard unarmed): {}", Self::KIND, plan.reason);
            tracing::info!(
                target: "malcolm_agent::process",
                applied_id = id,
                plan = %plan,
                "process adapter: dry-run (guard unarmed)"
            );
            return Ok(AppliedFault {
                id,
                adapter: Self::KIND,
                dry_run: true,
                description,
            });
        }

        // Decode the action so we can route to the right check.
        let action = ProcessAction::from_payload(&plan.payload)?;

        // Safety check first — before any signal. The guard rejects
        // pid 1, self, parent, and any pid not on the allowlist.
        guard
            .check_target(&Target::Pid(action.pid()))
            .map_err(|e| {
                tracing::warn!(
                    target: "malcolm_agent::process",
                    pid = action.pid(),
                    error = %e,
                    "process adapter: target rejected by safety guard"
                );
                e
            })?;

        // Probe the pid exists before we attempt a signal. This is
        // a better error than racing a kill(2) that may return
        // ESRCH after we already started a graceful terminate loop.
        if !Self::probe_live(action.pid())? {
            return Err(AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("pid {} does not exist", action.pid()),
            });
        }

        // Dispatch.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let result = match &action {
            ProcessAction::Signal { pid, signal } => {
                let sig = Self::parse_signal(signal)?;
                signal::kill(pid_to_nix(*pid)?, sig).map_err(|e| AgentError::AdapterFailure {
                    adapter: Self::KIND,
                    reason: format!("kill({pid}, {sig:?}) failed: {e}"),
                })?;
                Ok(())
            }
            ProcessAction::Terminate { pid, grace } => {
                let nix_pid = pid_to_nix(*pid)?;
                // SIGTERM first.
                if let Err(e) = signal::kill(nix_pid, NixSignal::SIGTERM) {
                    return Err(AgentError::AdapterFailure {
                        adapter: Self::KIND,
                        reason: format!("SIGTERM to {pid} failed: {e}"),
                    });
                }
                // Wait up to `grace` for the process to exit. We
                // probe liveness with `kill(pid, None)` (which
                // doesn't deliver a signal) rather than `waitpid`
                // so the runtime/parent that spawned the child can
                // still reap it after we return. Polling with a
                // 10 ms backoff keeps the loop non-busy and the
                // total wait bounded by `grace`.
                let deadline = Instant::now() + *grace;
                let mut exited = false;
                loop {
                    match signal::kill(nix_pid, None) {
                        Ok(()) => {
                            // Still alive. Back off.
                            if Instant::now() >= deadline {
                                break;
                            }
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(nix::errno::Errno::ESRCH) => {
                            exited = true;
                            break;
                        }
                        Err(e) => {
                            return Err(AgentError::AdapterFailure {
                                adapter: Self::KIND,
                                reason: format!("liveness probe failed: {e}"),
                            });
                        }
                    }
                }
                if !exited {
                    // Escalate to SIGKILL. This is irreversible.
                    if let Err(e) = signal::kill(nix_pid, NixSignal::SIGKILL) {
                        return Err(AgentError::AdapterFailure {
                            adapter: Self::KIND,
                            reason: format!("escalation to SIGKILL of {pid} failed: {e}"),
                        });
                    }
                    tracing::warn!(
                        target: "malcolm_agent::process",
                        pid,
                        grace_ms = u64::try_from(grace.as_millis()).unwrap_or(u64::MAX),
                        "process adapter: SIGTERM did not terminate within grace; escalated to SIGKILL"
                    );
                }
                Ok(())
            }
            ProcessAction::Pause { pid } => {
                signal::kill(pid_to_nix(*pid)?, NixSignal::SIGSTOP).map_err(|e| {
                    AgentError::AdapterFailure {
                        adapter: Self::KIND,
                        reason: format!("SIGSTOP to {pid} failed: {e}"),
                    }
                })?;
                Ok(())
            }
            ProcessAction::Resume { pid } => {
                signal::kill(pid_to_nix(*pid)?, NixSignal::SIGCONT).map_err(|e| {
                    AgentError::AdapterFailure {
                        adapter: Self::KIND,
                        reason: format!("SIGCONT to {pid} failed: {e}"),
                    }
                })?;
                Ok(())
            }
        };

        result?;

        // Record the (id, pid, action) tuple so `revert` can decide
        // what to do. The map is consumed by `revert`; the runtime
        // is expected to call `revert` for every applied fault.
        let pid = action.pid();
        // Compute the description and tracing payload BEFORE moving
        // `action` into the bookkeeping map — once it's in the map
        // we cannot borrow it.
        let kind = kind_str(&action);
        let description = format!("{}: {}", Self::KIND, kind);
        {
            let mut applied = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            applied.insert(id, (pid, action));
        }
        tracing::info!(
            target: "malcolm_agent::process",
            applied_id = id,
            kind,
            pid,
            "process adapter: applied"
        );
        Ok(AppliedFault {
            id,
            adapter: Self::KIND,
            dry_run: false,
            description,
        })
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        // Look up the original action by id. If the id was never
        // recorded (e.g. the dry-run path returns ids that are not
        // in the map) the revert is a no-op. The Cleanup registry
        // also calls revert for dry-run applied faults; in that
        // case there is nothing to do.
        let entry = {
            let mut map = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.remove(&applied.id)
        };
        let Some((pid, action)) = entry else {
            // Dry-run or unknown id. Nothing to revert; treat as
            // success so cleanup does not loop on a no-op.
            return Ok(());
        };
        match action {
            ProcessAction::Pause { .. } => {
                // The applied fault was a Pause; revert by sending
                // SIGCONT. Errors here are reported but the cleanup
                // loop continues with the next entry.
                if let Err(e) = signal::kill(pid_to_nix(pid)?, NixSignal::SIGCONT) {
                    tracing::warn!(
                        target: "malcolm_agent::process",
                        pid,
                        error = %e,
                        "process adapter: SIGCONT during revert failed; process may remain stopped"
                    );
                    return Err(AgentError::AdapterFailure {
                        adapter: Self::KIND,
                        reason: format!("SIGCONT to {pid} during revert failed: {e}"),
                    });
                }
                tracing::info!(
                    target: "malcolm_agent::process",
                    pid,
                    "process adapter: SIGCONT sent during revert"
                );
                Ok(())
            }
            ProcessAction::Signal { .. }
            | ProcessAction::Terminate { .. }
            | ProcessAction::Resume { .. } => {
                // Irreversible (or self-reverting). No-op.
                Ok(())
            }
        }
    }

    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }
}

/// Short, stable action name used in tracing events and
/// `AppliedFault::description`. Matches the T14 `fault_type` naming
/// convention.
const fn kind_str(action: &ProcessAction) -> &'static str {
    match action {
        ProcessAction::Signal { .. } => "process_signal",
        ProcessAction::Terminate { .. } => "process_kill",
        ProcessAction::Pause { .. } => "process_pause",
        ProcessAction::Resume { .. } => "process_resume",
    }
}

impl ProcessAction {
    /// Pid targeted by this action. Used by the safety interlock to
    /// route to the pid allowlist.
    #[must_use]
    pub const fn pid(&self) -> u32 {
        match self {
            Self::Signal { pid, .. }
            | Self::Terminate { pid, .. }
            | Self::Pause { pid }
            | Self::Resume { pid } => *pid,
        }
    }
}
