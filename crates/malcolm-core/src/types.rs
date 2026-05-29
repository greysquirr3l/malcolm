//! Shared domain value objects for fault injection outcomes.
//!
//! These types are `no_std`-compatible (requiring only `alloc`) and travel
//! across crate boundaries as the results of fault operations.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::types::{FaultEvent, FaultResult, SkipReason};
//!
//! let event = FaultEvent {
//!     fault_type: "network_partition".to_owned(),
//!     node_id: "node-0".to_owned(),
//!     seed: 42,
//!     intensity: 0.8,
//!     dry_run: false,
//!     timestamp_ms: 1_000,
//! };
//! let result = FaultResult::Injected(event);
//! assert!(matches!(result, FaultResult::Injected(_)));
//! ```

use alloc::string::String;

/// Crate version string, re-exported for runtime inspection.
pub const MALCOLM_CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

// ── FaultEvent ────────────────────────────────────────────────────────────────

/// A record of a single fault injection event.
///
/// `FaultEvent` is produced when a fault is successfully injected. It carries
/// all context needed to reproduce or replay the injection deterministically.
///
/// The `timestamp_ms` field is provided by the caller rather than read from
/// the system clock, preserving `no_std` compatibility and deterministic replay.
///
/// # Example
///
/// ```rust
/// use malcolm_core::types::FaultEvent;
///
/// let event = FaultEvent {
///     fault_type: "latency_spike".to_owned(),
///     node_id: "gateway".to_owned(),
///     seed: 1337,
///     intensity: 0.65,
///     dry_run: false,
///     timestamp_ms: 5_000,
/// };
/// assert!(!event.dry_run);
/// ```
#[derive(Debug, Clone, PartialEq)]
pub struct FaultEvent {
    /// Identifier for the kind of fault (e.g. `"network_partition"`).
    pub fault_type: String,
    /// Identifier of the topology node that received the fault.
    pub node_id: String,
    /// Seed used to produce this injection, enabling deterministic replay.
    pub seed: u64,
    /// Normalised fault intensity in `[0.0, 1.0]`.
    pub intensity: f64,
    /// `true` when the fault was emitted in dry-run mode (no real side effects).
    pub dry_run: bool,
    /// Wall-clock milliseconds at injection time, supplied by the caller.
    pub timestamp_ms: u64,
}

// ── SkipReason ────────────────────────────────────────────────────────────────

/// Reason a fault was skipped rather than injected.
///
/// # Example
///
/// ```rust
/// use malcolm_core::types::{FaultResult, SkipReason};
///
/// let result = FaultResult::Skipped(SkipReason::DryRun);
/// assert!(matches!(result, FaultResult::Skipped(SkipReason::DryRun)));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// The intensity was below the fault's activation threshold.
    BelowThreshold,
    /// The fault is operating in dry-run mode and intentionally took no action.
    DryRun,
    /// The fault was cancelled via its handle before injection could occur.
    Cancelled,
}

// ── FaultResult ───────────────────────────────────────────────────────────────

/// The outcome of a single fault injection attempt.
///
/// # Example
///
/// ```rust
/// use malcolm_core::types::{FaultResult, SkipReason};
///
/// let skipped = FaultResult::Skipped(SkipReason::BelowThreshold);
/// assert!(matches!(skipped, FaultResult::Skipped(_)));
/// ```
#[derive(Debug, Clone, PartialEq)]
pub enum FaultResult {
    /// The fault was injected; the event record is attached.
    Injected(FaultEvent),
    /// The fault was not injected; the skip reason is attached.
    Skipped(SkipReason),
}

// ── DryRunReport ──────────────────────────────────────────────────────────────

/// Description of what a fault *would* do if executed without dry-run mode.
///
/// Returned by `Fault::dry_run` and always emitted as a `tracing::debug!`
/// event by conforming implementations.
///
/// # Example
///
/// ```rust
/// use malcolm_core::types::DryRunReport;
///
/// let report = DryRunReport {
///     fault_type: "clock_skew".to_owned(),
///     node_id: "replica-1".to_owned(),
///     would_inject: true,
///     reason: "intensity 0.7 exceeds threshold 0.5".to_owned(),
/// };
/// assert!(report.would_inject);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunReport {
    /// Identifier for the kind of fault.
    pub fault_type: String,
    /// Identifier of the topology node that would receive the fault.
    pub node_id: String,
    /// `true` if the fault would have been injected under normal conditions.
    pub would_inject: bool,
    /// Human-readable explanation of the dry-run outcome.
    pub reason: String,
}
