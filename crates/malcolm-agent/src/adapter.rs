//! `TargetAdapter` — the port trait that bridges in-process fault
//! primitives to real, out-of-process side effects.
//!
//! Adapters in this crate are the *only* place where side effects land.
//! The math core (`malcolm-core`) and the simulation library (`malcolm`)
//! stay untouched and side-effect-free. Every adapter MUST go through
//! [`crate::SafetyGuard`] before touching the host; see
//! [`crate::safety`] for the interlock contract.
//!
//! # Apply → revert lifecycle
//!
//! ```text
//!  ┌──────────────┐ apply(plan) ┌─────────────────┐ revert(applied) ┌────────────┐
//!  │ TargetAdapter├────────────►│ AppliedFault    ├────────────────►│ host state │
//!  └──────────────┘             │ registered with │                 │ restored   │
//!                               │ Cleanup registry│                 └────────────┘
//!                               └─────────────────┘
//! ```
//!
//! `apply` returns an [`AppliedFault`] that the [`crate::cleanup::Cleanup`]
//! registry holds until `revert` is called — automatically on `Drop`,
//! on `SIGINT`/`SIGTERM`, or explicitly by the caller.

use std::fmt;

use crate::error::AgentError;
use crate::safety::SafetyGuard;

/// Resolved, concrete instruction for a real-world side effect.
///
/// A `FaultPlan` is the output of translating a malcolm in-process
/// `Fault` decision (which was sampled under the in-process simulation)
/// into a real action against a real target. Adapters consume the
/// plan, not the original malcolm fault — so a plan can be inspected,
/// rejected, or recorded without coupling adapter code to the
/// simulation library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultPlan {
    /// Stable identifier of the adapter that should execute the plan
    /// (matches [`TargetAdapter::adapter_kind`]).
    pub adapter: String,
    /// Free-form, adapter-specific payload. Each adapter defines its
    /// own JSON schema; consumers MUST route the plan to the adapter
    /// whose `adapter_kind()` matches `adapter` or reject it.
    pub payload: serde_json::Value,
    /// Short, human-readable reason that surfaces in `tracing` events.
    pub reason: String,
}

impl fmt::Display for FaultPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "FaultPlan(adapter={}, reason={})",
            self.adapter, self.reason
        )
    }
}

/// Records what a `TargetAdapter::apply` call actually did so the
/// effect can be undone. The same struct is used for *dry-run* results
/// (with `dry_run: true`) and for live applied faults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedFault {
    /// Stable identifier returned by the adapter; the cleanup registry
    /// uses it as the lookup key.
    pub id: u64,
    /// Adapter that produced the fault; matches
    /// [`TargetAdapter::adapter_kind`].
    pub adapter: &'static str,
    /// When `true`, no side effect was performed. The plan was
    /// validated and *would have* executed, but the host was left
    /// untouched. Mirrors the in-process `Fault::dry_run` contract.
    pub dry_run: bool,
    /// Human-readable description of what was (or would have been)
    /// changed on the host. Safe to log.
    pub description: String,
}

impl fmt::Display for AppliedFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mode = if self.dry_run { "dry-run" } else { "applied" };
        write!(
            f,
            "AppliedFault(id={}, adapter={}, mode={}, desc={})",
            self.id, self.adapter, mode, self.description
        )
    }
}

/// The port trait every real-world adapter implements.
///
/// Adapters are constructed with whatever state they need (system
/// handles, capability tokens, network interface names) and then
/// submitted to the agent runtime. The runtime calls
/// [`apply`](Self::apply) after consulting [`SafetyGuard`], and
/// [`revert`](Self::revert) on cleanup.
///
/// # Safety contract
///
/// Every concrete implementation MUST:
/// 1. Call `guard.check_plan(self, plan)?` at the top of `apply`.
/// 2. Refuse to mutate host state unless `guard.is_armed()` is `true`
///    *and* the plan is on the allowlist.
/// 3. Return a `dry_run: true` `AppliedFault` when the guard is
///    unarmed, and perform no side effect.
/// 4. Make `revert` idempotent — the cleanup registry may call it more
///    than once during shutdown.
pub trait TargetAdapter: Send + Sync {
    /// Apply a real side effect derived from the sampled fault decision.
    ///
    /// # Errors
    ///
    /// - [`AgentError::NotArmed`] if the guard is not fully armed.
    /// - [`AgentError::TargetNotAllowed`] if the plan's target is not
    ///   on the allowlist.
    /// - [`AgentError::AdapterFailure`] for adapter-specific failures.
    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError>;

    /// Revert a previously applied fault. Adapters must be reversible
    /// where physically possible (process kill is not, but throttled
    /// CPU is). Non-reversible adapters MUST return
    /// `Err(AdapterFailure { .. })` here so the caller can record the
    /// fact in the run report.
    ///
    /// # Errors
    ///
    /// - [`AgentError::AdapterFailure`] if the revert itself failed.
    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError>;

    /// Stable identifier for the adapter (e.g. `"process"`, `"netem"`,
    /// `"cgroups"`). Used in tracing events and the run report.
    fn adapter_kind(&self) -> &'static str;
}
