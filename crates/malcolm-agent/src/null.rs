//! `NullAdapter` — the no-op adapter that ships with the default build.
//!
//! `NullAdapter::apply` accepts every plan and returns a `dry_run: true`
//! `AppliedFault`. The agent runtime and tests can use it to validate
//! the wiring (allowlist, cleanup, apply/revert lifecycle) without
//! touching the host. The real adapters (T34–T38) live behind
//! feature flags and replace `NullAdapter` in the production wiring.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::SafetyGuard;

/// A no-op adapter. Always reports `dry_run: true`; never mutates
/// host state. Use it for unit tests and for the default build of
/// the agent runtime.
#[derive(Debug, Default)]
pub struct NullAdapter {
    /// Monotonic counter for the dry-run ids the adapter hands out.
    /// Atomic so multiple invocations from different threads produce
    /// distinct ids.
    next_id: AtomicU64,
}

impl NullAdapter {
    /// Construct a new `NullAdapter` with its id counter at zero.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
        }
    }
}

impl TargetAdapter for NullAdapter {
    fn apply(&self, plan: &FaultPlan, _guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        // The null adapter never touches the host. It accepts any
        // plan and records a description that mirrors the plan's
        // human-readable reason. The arming state of the guard is
        // intentionally not consulted — the contract for the null
        // adapter is "always dry-run, never side-effect".
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let description = format!("null:{plan}");
        let applied = AppliedFault {
            id,
            adapter: self.adapter_kind(),
            dry_run: true,
            description,
        };
        tracing::info!(
            target: "malcolm_agent::null",
            applied_id = applied.id,
            plan = %plan,
            "null adapter: dry-run only (no side effect)"
        );
        Ok(applied)
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        // Reversal of a dry-run is a no-op. The runtime may still
        // call this during cleanup; treat it as success.
        tracing::debug!(
            target: "malcolm_agent::null",
            applied_id = applied.id,
            "null adapter: revert of dry-run is a no-op"
        );
        Ok(())
    }

    fn adapter_kind(&self) -> &'static str {
        "null"
    }
}
