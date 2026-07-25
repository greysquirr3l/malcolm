//! Linux `tc`/`netem` real network-fault adapter.
//!
//! The adapter turns the in-process T07 network faults into real
//! Linux traffic-control impairments on a named interface. It
//! shells out to the `tc` binary from iproute2 — no `unsafe`,
//! no new dependency, and the qdisc snapshot/restore helpers in
//! [`netem_cmd`] guarantee the interface returns to its original
//! state on revert.
//!
//! # Feature gating
//!
//! Compiled only on Linux with the `netem` feature enabled. The
//! default build of `malcolm-agent` cannot touch network
//! interfaces.
//!
//! # Safety contract
//!
//! Every action goes through [`SafetyGuard::check_target`] for
//! the interface. The guard refuses the host's default-route
//! interface by construction unless the operator has explicitly
//! added it to the iface allowlist. The arming state is also
//! checked: an unarmed guard returns a `dry_run: true`
//! `AppliedFault` carrying the exact `tc` command lines that
//! would have been run, with no execution.
//!
//! # Reversibility
//!
//! `apply` snapshots the existing root qdisc before any change.
//! `revert` removes the netem qdisc and replays the snapshot.
//! The [`crate::cleanup::Cleanup`] registry guarantees revert on
//! `Drop` and on `SIGINT`/`SIGTERM`. A crashed run cannot leave
//! the interface impaired.
//!
//! # Watchdog
//!
//! `apply` accepts an optional `watchdog_ms` field. When set, the
//! adapter spawns a background thread that sleeps for the
//! duration and then calls `revert` directly. This is a safety
//! net for a wedged test process that has not yet dropped its
//! `Cleanup` registry.
//!
//! # Privilege requirements
//!
//! `tc` requires `CAP_NET_ADMIN`. The adapter treats absence of
//! the binary or the privilege as a clean `PlatformUnsupported`
//! error; the integration tests skip cleanly on unprivileged
//! runners.

use std::collections::HashMap;
use std::process::Command;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

use super::netem_cmd::{NetemParams, QdiscSnapshot, default_route_interface, tc_available};

/// Actions the netem adapter understands. Decoded from the
/// `FaultPlan::payload` JSON object.
#[derive(Debug, Clone, PartialEq)]
pub enum NetemAction {
    /// Add latency, optionally with jitter and correlation.
    Latency {
        /// Mean delay.
        mean: Duration,
        /// Jitter range (uniform).
        jitter: Option<Duration>,
        /// Correlation between consecutive packets' delays.
        correlation: Option<f32>,
    },
    /// Random packet loss.
    Loss {
        /// Loss percentage in `[0.0, 100.0]`.
        percent: f32,
        /// Loss correlation in `[0.0, 100.0]`.
        correlation: Option<f32>,
    },
    /// Random packet corruption.
    Corrupt {
        /// Corruption percentage in `[0.0, 100.0]`.
        percent: f32,
    },
    /// Packet reordering.
    Reorder {
        /// Reorder percentage in `[0.0, 100.0]`.
        percent: f32,
        /// Reorder correlation in `[0.0, 100.0]`.
        correlation: Option<f32>,
    },
    /// Bandwidth cap (modelled as a `tbf` qdisc layered under
    /// netem).
    Rate {
        /// Bytes per second.
        bps: u64,
    },
    /// Full partition — 100% loss for the duration.
    Partition,
}

impl NetemAction {
    /// Short, stable identifier for the action. Used in tracing
    /// events and the `AppliedFault::description`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Latency { .. } => "netem_latency",
            Self::Loss { .. } => "netem_loss",
            Self::Corrupt { .. } => "netem_corrupt",
            Self::Reorder { .. } => "netem_reorder",
            Self::Rate { .. } => "netem_rate",
            Self::Partition => "netem_partition",
        }
    }

    /// Decode a `NetemAction` from the JSON payload of a
    /// `FaultPlan`. Returns [`AgentError::InvalidPlan`] on any
    /// shape mismatch rather than guessing. Validates ranges so
    /// a malformed plan never reaches `tc`.
    ///
    /// # Errors
    ///
    /// - `InvalidPlan` if the payload is not a JSON object.
    /// - `InvalidPlan` if a required field is missing or has
    ///   the wrong type.
    /// - `InvalidPlan` if a percentage is out of `[0.0, 100.0]`.
    /// - `InvalidPlan` if a correlation is out of `[0.0, 100.0]`.
    /// - `InvalidPlan` if the `kind` discriminator is unknown.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, AgentError> {
        let obj = payload.as_object().ok_or_else(|| AgentError::InvalidPlan {
            adapter: NetemAdapter::KIND,
            reason: "payload must be a JSON object".to_owned(),
        })?;
        let kind = obj
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: NetemAdapter::KIND,
                reason: "missing or non-string field `kind`".to_owned(),
            })?;
        match kind {
            "latency" => {
                let mean_ms = obj
                    .get("mean_ms")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: NetemAdapter::KIND,
                        reason: "missing or non-u64 field `mean_ms`".to_owned(),
                    })?;
                let jitter = match obj.get("jitter_ms") {
                    Some(v) if v.is_number() => {
                        let j = v.as_u64().ok_or_else(|| AgentError::InvalidPlan {
                            adapter: NetemAdapter::KIND,
                            reason: "`jitter_ms` must be a non-negative u64".to_owned(),
                        })?;
                        Some(Duration::from_millis(j))
                    }
                    _ => None,
                };
                let correlation = match obj.get("correlation") {
                    Some(v) if v.is_number() => {
                        let c = v.as_f64().ok_or_else(|| AgentError::InvalidPlan {
                            adapter: NetemAdapter::KIND,
                            reason: "`correlation` must be a number".to_owned(),
                        })?;
                        // f64 -> f32 conversion; we accept the
                        // precision loss because correlations
                        // are percentages in [0, 100] and f32
                        // preserves 7 decimal digits of
                        // precision in that range.
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "value is a percentage in [0, 100]; precision is preserved"
                        )]
                        let c_f32 = c as f32;
                        validate_pct(NetemAdapter::KIND, "correlation", c_f32)?;
                        Some(c_f32)
                    }
                    _ => None,
                };
                Ok(Self::Latency {
                    mean: Duration::from_millis(mean_ms),
                    jitter,
                    correlation,
                })
            }
            "loss" => {
                let percent = percent_field(obj, "percent", NetemAdapter::KIND)?;
                let correlation = correlation_field(obj, "correlation", NetemAdapter::KIND)?;
                Ok(Self::Loss {
                    percent,
                    correlation,
                })
            }
            "corrupt" => {
                let percent = percent_field(obj, "percent", NetemAdapter::KIND)?;
                Ok(Self::Corrupt { percent })
            }
            "reorder" => {
                let percent = percent_field(obj, "percent", NetemAdapter::KIND)?;
                let correlation = correlation_field(obj, "correlation", NetemAdapter::KIND)?;
                Ok(Self::Reorder {
                    percent,
                    correlation,
                })
            }
            "rate" => {
                let bps = obj
                    .get("bps")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: NetemAdapter::KIND,
                        reason: "missing or non-u64 field `bps`".to_owned(),
                    })?;
                Ok(Self::Rate { bps })
            }
            "partition" => Ok(Self::Partition),
            other => Err(AgentError::InvalidPlan {
                adapter: NetemAdapter::KIND,
                reason: format!("unknown action kind `{other}`"),
            }),
        }
    }

    /// Translate the action into the netem parameter set the
    /// command builder consumes.
    fn to_params(&self) -> NetemParams {
        match self {
            Self::Latency {
                mean,
                jitter,
                correlation,
            } => NetemParams {
                delay: Some(*mean),
                delay_jitter: *jitter,
                delay_correlation: *correlation,
                loss_pct: None,
                loss_correlation: None,
                corrupt_pct: None,
                reorder_pct: None,
                reorder_correlation: None,
                rate_bps: None,
            },
            Self::Loss {
                percent,
                correlation,
            } => NetemParams {
                delay: None,
                delay_jitter: None,
                delay_correlation: None,
                loss_pct: Some(*percent),
                loss_correlation: *correlation,
                corrupt_pct: None,
                reorder_pct: None,
                reorder_correlation: None,
                rate_bps: None,
            },
            Self::Corrupt { percent } => NetemParams {
                delay: None,
                delay_jitter: None,
                delay_correlation: None,
                loss_pct: None,
                loss_correlation: None,
                corrupt_pct: Some(*percent),
                reorder_pct: None,
                reorder_correlation: None,
                rate_bps: None,
            },
            Self::Reorder {
                percent,
                correlation,
            } => NetemParams {
                delay: None,
                delay_jitter: None,
                delay_correlation: None,
                loss_pct: None,
                loss_correlation: None,
                corrupt_pct: None,
                reorder_pct: Some(*percent),
                reorder_correlation: *correlation,
                rate_bps: None,
            },
            Self::Rate { bps } => NetemParams {
                delay: None,
                delay_jitter: None,
                delay_correlation: None,
                loss_pct: None,
                loss_correlation: None,
                corrupt_pct: None,
                reorder_pct: None,
                reorder_correlation: None,
                rate_bps: Some(*bps),
            },
            Self::Partition => NetemParams {
                delay: None,
                delay_jitter: None,
                delay_correlation: None,
                loss_pct: Some(100.0),
                loss_correlation: None,
                corrupt_pct: None,
                reorder_pct: None,
                reorder_correlation: None,
                rate_bps: None,
            },
        }
    }
}

/// Internal state of the adapter, shared with the watchdog
/// thread. Held in an `Arc<Mutex<…>>` so a single `&self` clone
/// can outlive the `apply` call.
#[derive(Debug, Default)]
struct NetemState {
    /// Map from adapter id → bookkeeping the runtime needs to
    /// revert (interface, snapshot, dry-run flag).
    applied: HashMap<u64, AppliedRecord>,
}

#[derive(Debug)]
struct AppliedRecord {
    /// The interface the adapter touched. `None` for dry-runs.
    interface: Option<String>,
    /// Snapshot of the original root qdisc (if any). `None`
    /// when the interface had no root qdisc at apply time.
    snapshot: Option<QdiscSnapshot>,
}

/// The tc/netem adapter. Each `apply` call snapshots the
/// existing root qdisc, builds a `tc qdisc add` command for the
/// decoded action, and runs it. `revert` removes the netem qdisc
/// and replays the snapshot.
#[derive(Debug, Default, Clone)]
pub struct NetemAdapter {
    /// Monotonic counter for the dry-run / applied ids the
    /// adapter hands out. Distinct from the cleanup registry's
    /// id.
    next_id: Arc<AtomicU64>,
    /// Shared state behind an `Arc<Mutex<…>>` so the watchdog
    /// thread can outlive the `apply` call without borrowing
    /// `&self`.
    state: Arc<Mutex<NetemState>>,
}

impl NetemAdapter {
    /// Adapter kind string. Exposed as a constant so tests and
    /// adapters can compare against it without hard-coding.
    pub const KIND: &'static str = "netem";

    /// Construct a new `NetemAdapter` with its id counter at
    /// zero.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: Arc::new(AtomicU64::new(0)),
            state: Arc::new(Mutex::new(NetemState::default())),
        }
    }

    /// Number of applied faults currently tracked.
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .applied
            .len()
    }

    /// Internal: execute the `tc qdisc add` command for the
    /// action. Returns the new qdisc's handle. Caller must have
    /// already snapshotted the previous state.
    fn tc_add_qdisc(iface: &str, params: &NetemParams) -> Result<String, AgentError> {
        if !tc_available() {
            return Err(AgentError::PlatformUnsupported {
                adapter: Self::KIND,
                action: "tc".to_owned(),
                platform: "tc binary not available".to_owned(),
            });
        }
        let mut cmd = Command::new("tc");
        cmd.args(["qdisc", "add", "dev", iface, "root", "netem"]);
        for arg in params.tc_argv() {
            cmd.arg(arg);
        }
        let output = cmd.output().map_err(|e| AgentError::AdapterFailure {
            adapter: Self::KIND,
            reason: format!("tc qdisc add {iface} failed: {e}"),
        })?;
        if !output.status.success() {
            return Err(AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!(
                    "tc qdisc add {iface} failed: stderr={}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            });
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// Internal: remove the netem qdisc from the interface.
    /// Idempotent: missing qdisc is not an error so the
    /// `Cleanup` registry can call revert twice without harm.
    fn tc_del_qdisc(iface: &str) -> Result<(), AgentError> {
        if !tc_available() {
            return Ok(()); // Already gone, nothing to do.
        }
        let output = Command::new("tc")
            .args(["qdisc", "del", "dev", iface, "root"])
            .output()
            .map_err(|e| AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("tc qdisc del {iface} failed: {e}"),
            })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // `RTNETLINK answers: No such file or directory`
            // is the kernel's response when the qdisc is
            // already gone; treat that as success.
            if stderr.contains("No such file or directory") {
                return Ok(());
            }
            return Err(AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("tc qdisc del {iface} failed: stderr={stderr}"),
            });
        }
        Ok(())
    }

    /// Internal: replay a snapshot by adding the original qdisc
    /// back. Used by `revert` after the netem qdisc is gone.
    fn tc_restore_snapshot(iface: &str, snapshot: &QdiscSnapshot) -> Result<(), AgentError> {
        snapshot.restore(iface)
    }

    /// Internal: revert logic shared by the public `revert` and
    /// the watchdog thread. Returns `()` because the underlying
    /// `tc` calls are best-effort: failures are logged via
    /// `tracing::warn!` so a wedged teardown does not propagate
    /// an error and prevent the rest of the cleanup from
    /// running.
    fn revert_internal(record: &AppliedRecord) {
        if let Some(iface) = &record.interface {
            if let Err(e) = Self::tc_del_qdisc(iface) {
                tracing::warn!(
                    target: "malcolm_agent::netem",
                    iface = %iface,
                    error = %e,
                    "netem adapter: tc qdisc del failed during revert"
                );
            }
            if let Some(snapshot) = &record.snapshot {
                if let Err(e) = Self::tc_restore_snapshot(iface, snapshot) {
                    tracing::warn!(
                        target: "malcolm_agent::netem",
                        iface = %iface,
                        error = %e,
                        "netem adapter: snapshot restore failed during revert"
                    );
                }
            }
        }
    }
}

impl TargetAdapter for NetemAdapter {
    #[expect(
        clippy::too_many_lines,
        reason = "apply threads every NetemAction variant through one path"
    )]
    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        // The interface is carried in `payload.interface`. We
        // need it for the safety check and for the dry-run
        // description.
        let iface = plan
            .payload
            .get("interface")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: Self::KIND,
                reason: "missing or non-string field `interface`".to_owned(),
            })?;

        // Decode the action. InvalidPlan paths fire before any
        // safety check, so the InvalidPlan errors surface
        // cleanly even when the interface is not allowlisted.
        let action = NetemAction::from_payload(&plan.payload)?;
        let params = action.to_params();

        // Dry-run-first: if the guard is not armed, build the
        // command line(s) for the description and return.
        if !guard.is_armed() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let argv = params.tc_argv();
            let argv_joined = argv
                .iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ");
            let description = format!(
                "{} (dry-run; guard unarmed): tc qdisc add dev {iface} root netem {argv_joined}",
                Self::KIND
            );
            tracing::info!(
                target: "malcolm_agent::netem",
                applied_id = id,
                iface = %iface,
                kind = action.kind(),
                plan = %plan,
                "netem adapter: dry-run (guard unarmed)"
            );
            return Ok(AppliedFault {
                id,
                adapter: Self::KIND,
                dry_run: true,
                description,
            });
        }

        // Safety check on the interface. The allowlist is the
        // only thing standing between the adapter and the
        // default-route interface, so the check is mandatory.
        guard.check_target(&Target::Iface(iface)).map_err(|e| {
            tracing::warn!(
                target: "malcolm_agent::netem",
                iface = %iface,
                error = %e,
                "netem adapter: interface rejected by safety guard"
            );
            e
        })?;

        // Verify the host has `tc` available. Treat absence as
        // a clean platform error rather than panicking in
        // tests.
        if !tc_available() {
            return Err(AgentError::PlatformUnsupported {
                adapter: Self::KIND,
                action: action.kind().to_owned(),
                platform: "tc binary not available".to_owned(),
            });
        }

        // Log if the operator is applying impairment to the
        // default-route interface. The allowlist check above
        // is the actual protection; this is informational.
        if let Some(default_iface) = default_route_interface() {
            if default_iface == iface {
                tracing::info!(
                    target: "malcolm_agent::netem",
                    iface = %iface,
                    "netem adapter: applying impairment to the default-route interface (allowlisted)"
                );
            }
        }

        // Snapshot the existing root qdisc BEFORE we touch
        // anything, so revert can replay it.
        let snapshot = QdiscSnapshot::capture(iface)?;

        // Apply the qdisc.
        Self::tc_add_qdisc(iface, &params)?;

        // Record the bookkeeping for revert.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let record = AppliedRecord {
            interface: Some(iface.to_owned()),
            snapshot,
        };
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.applied.insert(id, record);
        }

        // Optional watchdog: spawn a thread that sleeps for
        // `watchdog_ms` then reverts. The watchdog reads from
        // the same `Arc<Mutex<NetemState>>` as the adapter
        // itself, so a clone of the adapter is sufficient.
        if let Some(watchdog_ms) = plan
            .payload
            .get("watchdog_ms")
            .and_then(serde_json::Value::as_u64)
        {
            let adapter = self.clone();
            std::thread::Builder::new()
                .name(format!("malcolm-netem-watchdog-{id}"))
                .spawn(move || {
                    std::thread::sleep(Duration::from_millis(watchdog_ms));
                    let entry = {
                        let mut state = adapter
                            .state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.applied.remove(&id)
                    };
                    if let Some(record) = entry {
                        NetemAdapter::revert_internal(&record);
                    }
                })
                .map_err(|e| AgentError::AdapterFailure {
                    adapter: Self::KIND,
                    reason: format!("failed to spawn watchdog thread: {e}"),
                })?;
        }

        let description = format!("{}: {} on {iface}", Self::KIND, action.kind());
        tracing::info!(
            target: "malcolm_agent::netem",
            applied_id = id,
            iface = %iface,
            kind = action.kind(),
            "netem adapter: applied"
        );
        Ok(AppliedFault {
            id,
            adapter: Self::KIND,
            dry_run: false,
            description,
        })
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        let entry = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.applied.remove(&applied.id)
        };
        let Some(record) = entry else {
            // Dry-run or unknown id. Nothing to revert.
            return Ok(());
        };
        Self::revert_internal(&record);
        Ok(())
    }

    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }
}

/// Validate a percentage field is in `[0.0, 100.0]`.
fn validate_pct(adapter: &'static str, field: &str, value: f32) -> Result<(), AgentError> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(AgentError::InvalidPlan {
            adapter,
            reason: format!("`{field}` must be in [0.0, 100.0]; got {value}"),
        });
    }
    Ok(())
}

fn percent_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    adapter: &'static str,
) -> Result<f32, AgentError> {
    let value_f64 = obj
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| AgentError::InvalidPlan {
            adapter,
            reason: format!("missing or non-number field `{field}`"),
        })?;
    #[expect(
        clippy::cast_possible_truncation,
        reason = "value is a percentage in [0, 100]; precision is preserved"
    )]
    let value = value_f64 as f32;
    validate_pct(adapter, field, value)?;
    Ok(value)
}

fn correlation_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    adapter: &'static str,
) -> Result<Option<f32>, AgentError> {
    match obj.get(field) {
        Some(v) if v.is_number() => {
            let c_f64 = v.as_f64().ok_or_else(|| AgentError::InvalidPlan {
                adapter,
                reason: format!("`{field}` must be a number"),
            })?;
            #[expect(
                clippy::cast_possible_truncation,
                reason = "value is a percentage in [0, 100]; precision is preserved"
            )]
            let c = c_f64 as f32;
            validate_pct(adapter, field, c)?;
            Ok(Some(c))
        }
        _ => Ok(None),
    }
}

/// Single-quote escape for the dry-run description so spaces
/// and shell metacharacters in argv values render unambiguously.
fn shell_escape(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '%' | ','))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}
