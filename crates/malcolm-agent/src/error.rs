//! Error type for the `malcolm-agent` crate.
//!
//! Every variant renders a stable, **non-secret-leaking** message. The
//! blast-radius of an `AgentError` is bounded: the message names the
//! rule that fired and (where relevant) the rejected target, but never
//! embeds user payloads, environment values, or adapter internals.

use thiserror::Error;

/// All failure modes that can surface from the agent port and its
/// safety interlocks.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The [`crate::SafetyGuard`] was not fully armed.
    ///
    /// The agent refuses to touch the host unless *both* the
    /// `MALCOLM_AGENT_ARM=1` environment flag is set **and** the caller
    /// passed the explicit opt-in boolean to [`crate::SafetyGuard::arm`].
    #[error("agent is not armed: set MALCOLM_AGENT_ARM=1 and pass the explicit opt-in boolean")]
    NotArmed,

    /// The `SafetyGuard` was constructed but the `MALCOLM_AGENT_ARM` env
    /// flag was missing.
    #[error("MALCOLM_AGENT_ARM environment flag is not set")]
    ArmFlagMissing,

    /// The caller did not pass the explicit opt-in boolean to
    /// [`crate::SafetyGuard::arm`].
    #[error("explicit opt-in boolean was not provided")]
    ExplicitOptInMissing,

    /// The requested target is not on the `SafetyGuard` allowlist.
    #[error("target {target} is not on the allowlist (rule: {rule})")]
    TargetNotAllowed {
        /// Stable identifier of the rule that rejected the target
        /// (e.g. `"self_pid"`, `"pid_1"`, `"default_route_iface"`).
        rule: &'static str,
        /// The rejected target, as a short, safe-to-log label.
        target: String,
    },

    /// The adapter asked the `SafetyGuard` to apply a `FaultPlan` that
    /// did not pass the dry-run contract.
    #[error("fault plan did not pass dry-run contract")]
    DryRunRequired,

    /// The underlying adapter failed to apply or revert a fault.
    #[error("adapter {adapter} failed: {reason}")]
    AdapterFailure {
        /// Adapter identifier (matches [`crate::TargetAdapter::adapter_kind`]).
        adapter: &'static str,
        /// Adapter-supplied reason; intentionally free-form but never
        /// expected to embed secrets.
        reason: String,
    },

    /// The Cleanup registry was asked to revert a fault it did not
    /// register — usually a sign that two agents are sharing a host
    /// or that a prior run leaked.
    #[error("applied fault {id} is not registered with the cleanup registry")]
    UnknownAppliedFault {
        /// Stable identifier of the applied fault.
        id: u64,
    },
}
