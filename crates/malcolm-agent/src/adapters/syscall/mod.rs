//! Syscall interception / fault injection adapter (feature `syscall`).
//!
//! **Experimental. The highest blast-radius adapter in this crate.**
//! Intercepts a target process's syscalls via `ptrace(2)` and injects
//! failures — force a chosen syscall to return a chosen errno, or delay
//! it — to exercise error-handling paths that are otherwise nearly
//! impossible to trigger on demand (real `ENOSPC`, real
//! `ECONNREFUSED`, ...).
//!
//! # Feature gating
//!
//! Compiled only on `target_os = "linux"`, `target_arch = "x86_64"`,
//! with the `syscall` feature enabled. `x86_64`-only because the register
//! manipulation in [`ptrace`] operates on the `x86_64` `user_regs_struct`
//! layout (`orig_rax` / `rax`); other Linux architectures are a
//! documented follow-up.
//!
//! # Two modes
//!
//! - **Spawn-under-supervision** (preferred, default): the adapter
//!   launches the target itself via [`ptrace::Supervisor::spawn_under_supervision`],
//!   so the blast radius is a single child this process already owns.
//!   No `SafetyGuard` target check is needed — there is no pre-existing
//!   pid to allowlist — but the guard must still be armed.
//! - **Attach** (fallback, explicit opt-in required): attaches to an
//!   already-running, allowlisted pid via
//!   [`ptrace::Supervisor::attach`]. Requires *both* the pid on
//!   `SafetyGuard`'s allowlist *and* [`SyscallAdapter::with_attach_enabled`]
//!   — a plan alone cannot switch the adapter into the more invasive
//!   mode.
//!
//! # Safety contract
//!
//! An unarmed guard makes `apply` a dry-run: the plan is decoded and
//! validated (so a malformed payload still surfaces `InvalidPlan`), but
//! no process is spawned, attached to, or traced. See
//! [`crate::safety::SafetyGuard`].
//!
//! # Determinism
//!
//! `probability < 1.0` is sampled from a `StdRng` seeded from the
//! plan's `seed` field, so a fixed seed reproduces the same
//! inject/skip sequence of matching syscall occurrences across runs.
//! Real OS scheduling is not deterministic — this preserves malcolm's
//! replay guarantee for *which occurrences* are injected, not *when*
//! they occur in wall-clock time.

mod ptrace;
mod table;

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Map, Value};

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

pub use table::SyscallSelector;

/// Where the fault-injecting `ptrace` supervisor attaches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyscallTarget {
    /// Preferred mode: the adapter spawns `command` itself
    /// (`command[0]` is the program, the rest are arguments).
    Spawn {
        /// Program and arguments; `command[0]` is the executable.
        command: Vec<String>,
    },
    /// Fallback mode: attach to an already-running pid. Requires the pid
    /// on the `SafetyGuard` allowlist and
    /// [`SyscallAdapter::with_attach_enabled`].
    Attach {
        /// Target pid.
        pid: u32,
    },
}

/// What to do to a matching syscall occurrence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SyscallEffect {
    /// Skip the real syscall and report `-errno` to the caller.
    FailWith {
        /// Positive errno value, e.g. `28` for `ENOSPC`.
        errno: i32,
    },
    /// Let the real syscall run, but delay its entry by `delay`.
    Delay {
        /// How long to hold the syscall at its entry stop.
        delay: Duration,
    },
}

/// A fully-decoded syscall fault instruction: the output of
/// [`SyscallAction::from_payload`], consumed by [`SyscallAdapter::apply`].
#[derive(Debug, Clone, PartialEq)]
pub struct SyscallAction {
    /// Where to attach.
    pub target: SyscallTarget,
    /// Which syscall to match.
    pub syscall: SyscallSelector,
    /// What to do on a match.
    pub effect: SyscallEffect,
    /// Injection probability per matching occurrence, in `[0.0, 1.0]`.
    pub probability: f32,
    /// Seed for the deterministic per-occurrence RNG.
    pub seed: u64,
}

impl SyscallAction {
    /// Stable identifier for the effect, used in tracing events and the
    /// `AppliedFault::description`. Matches the T37 `fault_type` naming
    /// convention (`syscall_fail` / `syscall_delay`).
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self.effect {
            SyscallEffect::FailWith { .. } => "syscall_fail",
            SyscallEffect::Delay { .. } => "syscall_delay",
        }
    }

    /// Human-readable summary for `AppliedFault::description` and the
    /// dry-run path, e.g. `` `syscall_fail write(1) errno=28 probability=1 mode=spawn` ``.
    #[must_use]
    pub fn describe(&self) -> String {
        let mode = match &self.target {
            SyscallTarget::Spawn { command } => format!("spawn({})", command.join(" ")),
            SyscallTarget::Attach { pid } => format!("attach(pid={pid})"),
        };
        let effect = match self.effect {
            SyscallEffect::FailWith { errno } => format!("errno={errno}"),
            SyscallEffect::Delay { delay } => format!("delay_ms={}", delay.as_millis()),
        };
        format!(
            "{} {} {effect} probability={} {mode}",
            self.kind(),
            self.syscall.label(),
            self.probability
        )
    }

    /// Decode a `SyscallAction` from the JSON payload of a `FaultPlan`.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::InvalidPlan`] on any shape mismatch: not a
    /// JSON object, a missing or wrongly-typed required field, an
    /// unknown `kind`/`mode` discriminator, an out-of-range
    /// `probability`, or an unresolvable syscall name.
    pub fn from_payload(payload: &Value) -> Result<Self, AgentError> {
        let obj = payload.as_object().ok_or_else(|| AgentError::InvalidPlan {
            adapter: SyscallAdapter::KIND,
            reason: "payload must be a JSON object".to_owned(),
        })?;
        let kind =
            obj.get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: SyscallAdapter::KIND,
                    reason: "missing or non-string field `kind`".to_owned(),
                })?;
        let target = decode_target(obj)?;
        let syscall = SyscallSelector::from_json(obj.get("syscall"))?;
        let probability = decode_probability(obj)?;
        let seed = obj.get("seed").and_then(Value::as_u64).unwrap_or(0);

        let effect = match kind {
            "fail_with" => {
                let errno = obj.get("errno").and_then(Value::as_i64).ok_or_else(|| {
                    AgentError::InvalidPlan {
                        adapter: SyscallAdapter::KIND,
                        reason: "missing or non-integer field `errno`".to_owned(),
                    }
                })?;
                let errno = i32::try_from(errno).map_err(|_| AgentError::InvalidPlan {
                    adapter: SyscallAdapter::KIND,
                    reason: format!("errno {errno} does not fit in i32"),
                })?;
                SyscallEffect::FailWith { errno }
            }
            "delay" => {
                let delay_ms = obj.get("delay_ms").and_then(Value::as_u64).ok_or_else(|| {
                    AgentError::InvalidPlan {
                        adapter: SyscallAdapter::KIND,
                        reason: "missing or non-u64 field `delay_ms`".to_owned(),
                    }
                })?;
                SyscallEffect::Delay {
                    delay: Duration::from_millis(delay_ms),
                }
            }
            other => {
                return Err(AgentError::InvalidPlan {
                    adapter: SyscallAdapter::KIND,
                    reason: format!("unknown action kind `{other}`"),
                });
            }
        };

        Ok(Self {
            target,
            syscall,
            effect,
            probability,
            seed,
        })
    }
}

fn decode_target(obj: &Map<String, Value>) -> Result<SyscallTarget, AgentError> {
    let mode = obj
        .get("mode")
        .and_then(Value::as_str)
        .ok_or_else(|| AgentError::InvalidPlan {
            adapter: SyscallAdapter::KIND,
            reason: "missing or non-string field `mode` (\"spawn\" or \"attach\")".to_owned(),
        })?;
    match mode {
        "spawn" => {
            let raw = obj
                .get("command")
                .and_then(Value::as_array)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: SyscallAdapter::KIND,
                    reason: "mode `spawn` requires array field `command`".to_owned(),
                })?;
            let command: Option<Vec<String>> =
                raw.iter().map(|v| v.as_str().map(str::to_owned)).collect();
            let command = command.ok_or_else(|| AgentError::InvalidPlan {
                adapter: SyscallAdapter::KIND,
                reason: "field `command` must be an array of strings".to_owned(),
            })?;
            if command.is_empty() {
                return Err(AgentError::InvalidPlan {
                    adapter: SyscallAdapter::KIND,
                    reason: "field `command` must not be empty".to_owned(),
                });
            }
            Ok(SyscallTarget::Spawn { command })
        }
        "attach" => {
            let pid =
                obj.get("pid")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: SyscallAdapter::KIND,
                        reason: "mode `attach` requires u64 field `pid`".to_owned(),
                    })?;
            let pid = u32::try_from(pid).map_err(|_| AgentError::InvalidPlan {
                adapter: SyscallAdapter::KIND,
                reason: format!("pid {pid} does not fit in u32"),
            })?;
            Ok(SyscallTarget::Attach { pid })
        }
        other => Err(AgentError::InvalidPlan {
            adapter: SyscallAdapter::KIND,
            reason: format!("unknown mode `{other}` (expected \"spawn\" or \"attach\")"),
        }),
    }
}

fn decode_probability(obj: &Map<String, Value>) -> Result<f32, AgentError> {
    let Some(raw) = obj.get("probability") else {
        return Ok(1.0);
    };
    let raw = raw.as_f64().ok_or_else(|| AgentError::InvalidPlan {
        adapter: SyscallAdapter::KIND,
        reason: "field `probability` must be a number".to_owned(),
    })?;
    if !(0.0..=1.0).contains(&raw) {
        return Err(AgentError::InvalidPlan {
            adapter: SyscallAdapter::KIND,
            reason: format!("field `probability` must be in [0.0, 1.0], got {raw}"),
        });
    }
    #[allow(
        clippy::cast_possible_truncation,
        reason = "probability is validated to [0.0, 1.0]; f64->f32 narrowing loses only precision \
                   far beyond what a probability needs, never range"
    )]
    let narrowed = raw as f32;
    Ok(narrowed)
}

/// The syscall interception adapter. Each applied fault owns a
/// dedicated `ptrace` supervisor thread for its lifetime; `revert` stops
/// that thread and detaches, leaving the target running unimpeded —
/// see [`ptrace::Supervisor::stop`].
#[derive(Debug, Default)]
pub struct SyscallAdapter {
    next_id: AtomicU64,
    /// Explicit opt-in for attach mode. `false` by default: a
    /// `FaultPlan` alone cannot switch this adapter into the more
    /// invasive attach path, matching the process/cgroups adapters'
    /// pattern of requiring an explicit, code-reviewable toggle for the
    /// most dangerous behavior.
    attach_enabled: AtomicBool,
    /// Map from adapter id to its live supervisor. A `Mutex` so `revert`
    /// (which needs `&self`, not `&mut self`, per the `TargetAdapter`
    /// contract) can remove entries.
    applied: Mutex<HashMap<u64, ptrace::Supervisor>>,
}

impl SyscallAdapter {
    /// Adapter kind string used in `adapter_kind()` and
    /// `AgentError::{InvalidPlan,AdapterFailure}` reasons.
    pub const KIND: &'static str = "syscall";

    /// Construct a new `SyscallAdapter` with attach mode disabled (only
    /// spawn-under-supervision plans will succeed).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Explicitly enable or disable attach-mode plans. Disabled by
    /// default; the caller must opt in with `true` after weighing that
    /// attach mode traces a process it did not create.
    #[must_use]
    pub fn with_attach_enabled(self, enabled: bool) -> Self {
        self.attach_enabled.store(enabled, Ordering::Relaxed);
        self
    }

    /// Number of applied faults currently tracked. Used by tests to
    /// confirm the adapter's internal bookkeeping matches the `Cleanup`
    /// registry's count.
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

impl TargetAdapter for SyscallAdapter {
    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        let action = SyscallAction::from_payload(&plan.payload)?;
        let syscall_nr = action.syscall.resolve()?;
        let description = action.describe();

        if !guard.is_armed() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            tracing::info!(
                target: "malcolm_agent::syscall",
                applied_id = id,
                plan = %plan,
                description = %description,
                "syscall adapter: dry-run (guard unarmed)"
            );
            return Ok(AppliedFault {
                id,
                adapter: Self::KIND,
                dry_run: true,
                description: format!("{description} (dry-run; guard unarmed)"),
            });
        }

        if let SyscallTarget::Attach { pid } = &action.target {
            guard.check_target(&Target::Pid(*pid)).map_err(|e| {
                tracing::warn!(
                    target: "malcolm_agent::syscall",
                    pid,
                    error = %e,
                    "syscall adapter: attach target rejected by safety guard"
                );
                e
            })?;
            if !self.attach_enabled.load(Ordering::Relaxed) {
                return Err(AgentError::AdapterFailure {
                    adapter: Self::KIND,
                    reason:
                        "attach mode is disabled; call SyscallAdapter::with_attach_enabled(true) \
                              to allow tracing a pre-existing process, or use mode \"spawn\""
                            .to_owned(),
                });
            }
        }

        let effect = match action.effect {
            SyscallEffect::FailWith { errno } => ptrace::InjectKind::FailWith { errno },
            SyscallEffect::Delay { delay } => ptrace::InjectKind::Delay { duration: delay },
        };
        let spec = ptrace::InjectSpec {
            syscall_nr,
            syscall_label: action.syscall.label(),
            effect,
            probability: action.probability,
            seed: action.seed,
        };

        let supervisor = match &action.target {
            SyscallTarget::Spawn { command } => {
                ptrace::Supervisor::spawn_under_supervision(command, spec)?
            }
            SyscallTarget::Attach { pid } => ptrace::Supervisor::attach(*pid, spec)?,
        };
        let pid = supervisor.pid();

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut applied = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            applied.insert(id, supervisor);
        }
        tracing::info!(
            target: "malcolm_agent::syscall",
            applied_id = id,
            pid,
            kind = action.kind(),
            syscall = %action.syscall.label(),
            "syscall adapter: supervisor attached"
        );
        Ok(AppliedFault {
            id,
            adapter: Self::KIND,
            dry_run: false,
            description,
        })
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        let supervisor = {
            let mut map = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.remove(&applied.id)
        };
        let Some(supervisor) = supervisor else {
            // Dry-run or unknown id. Nothing to revert.
            return Ok(());
        };
        supervisor.stop()
    }

    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }
}

/// Probe whether this host actually allows `ptrace`: spawn a trivial,
/// immediately-exiting child under `PTRACE_TRACEME` supervision and see
/// whether the handshake succeeds, then tear it down.
///
/// This is a real end-to-end check rather than a sysctl read: it also
/// catches the container-seccomp-denies-ptrace case, which
/// `/proc/sys/kernel/yama/ptrace_scope` alone would miss. Returns
/// `false` (never panics) on hosts where `ptrace` is unavailable —
/// unprivileged CI runners, containers without `CAP_SYS_PTRACE` or with
/// a seccomp profile that denies `ptrace(2)`, or non-Linux platforms
/// reaching this function via a manual feature/cfg override.
#[must_use]
pub fn probe_ptrace_available() -> bool {
    let spec = ptrace::InjectSpec {
        syscall_nr: -1,
        syscall_label: "probe".to_owned(),
        effect: ptrace::InjectKind::FailWith { errno: 0 },
        probability: 0.0,
        seed: 0,
    };
    let command = ["/bin/true".to_owned()];
    match ptrace::Supervisor::spawn_under_supervision(&command, spec) {
        Ok(supervisor) => supervisor.stop().is_ok(),
        Err(_) => false,
    }
}
