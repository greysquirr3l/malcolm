//! Core `Fault` port trait, `FaultContext`, `FaultHandle`, and `FaultRegistry`.
//!
//! This module owns all consumer-facing port traits for fault injection.
//! Concrete fault implementations (network, resource, clock, Byzantine) live in
//! separate modules and implement these traits — they do **not** re-export them.
//!
//! # Tracing contract
//!
//! Every conforming `Fault::inject` implementation **must** emit a
//! `tracing::info!` event with structured fields `fault_type`, `node_id`,
//! `seed`, and `intensity`. Every `Fault::dry_run` implementation **must**
//! emit a `tracing::debug!` event.
//!
//! # Example
//!
//! ```rust
//! use malcolm::fault::{FaultHandle, FaultRegistry};
//!
//! let mut reg = FaultRegistry::new();
//! reg.register("node-0", FaultHandle::new());
//! reg.register("node-0", FaultHandle::new());
//! assert_eq!(reg.active_count("node-0"), 2);
//!
//! reg.cancel_node("node-0");
//! assert_eq!(reg.active_count("node-0"), 0);
//! ```

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::types::{DryRunReport, FaultResult};

// ── FaultContext ──────────────────────────────────────────────────────────────

/// All context a [`Fault`] implementation needs at injection time.
///
/// `timestamp_ms` is supplied by the caller rather than read from the system
/// clock, keeping fault replay deterministic.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::FaultContext;
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let ctx = FaultContext {
///     seed: 42,
///     timestamp_ms: 1_000,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::network_partition(),
/// };
/// assert_eq!(ctx.node_id, "node-0");
/// ```
#[derive(Debug, Clone)]
pub struct FaultContext {
    /// Seed for deterministic RNG within the fault implementation.
    pub seed: u64,
    /// Wall-clock time at injection, in milliseconds, supplied by the caller.
    pub timestamp_ms: u64,
    /// Identifier of the topology node being targeted.
    pub node_id: String,
    /// Bifurcation profile describing the stability regime of the target system.
    pub profile: BifurcationProfile,
}

// ── Fault ─────────────────────────────────────────────────────────────────────

/// Port trait for a chaos fault that can be injected or dry-run.
///
/// Implementors **must** emit structured `tracing` events on every call:
/// - `inject` → `tracing::info!` with fields `fault_type`, `node_id`, `seed`, `intensity`
/// - `dry_run` → `tracing::debug!` describing the outcome
///
/// The trait is object-safe: all methods take `&self` and return owned values.
/// There are no default implementations.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::{DryRunReport, FaultResult, SkipReason};
///
/// struct AlwaysSkip;
///
/// impl Fault for AlwaysSkip {
///     fn inject(&self, ctx: &FaultContext) -> FaultResult {
///         FaultResult::Skipped(SkipReason::BelowThreshold)
///     }
///     fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
///         DryRunReport {
///             fault_type: self.fault_type().to_owned(),
///             node_id: ctx.node_id.clone(),
///             would_inject: false,
///             reason: "below threshold".to_owned(),
///         }
///     }
///     fn fault_type(&self) -> &'static str { "always_skip" }
/// }
///
/// let ctx = FaultContext {
///     seed: 1,
///     timestamp_ms: 0,
///     node_id: "n0".to_owned(),
///     profile: BifurcationProfile::network_partition(),
/// };
/// assert!(matches!(AlwaysSkip.inject(&ctx), FaultResult::Skipped(_)));
/// ```
pub trait Fault {
    /// Inject the fault using the provided context, returning the outcome.
    fn inject(&self, ctx: &FaultContext) -> FaultResult;

    /// Describe what [`inject`](Fault::inject) would do without applying side effects.
    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport;

    /// Return a short, stable identifier for this fault kind.
    ///
    /// The string must match the `fault_type` field in emitted
    /// [`FaultEvent`](malcolm_core::types::FaultEvent) records.
    fn fault_type(&self) -> &'static str;
}

// ── FaultHandle ───────────────────────────────────────────────────────────────

/// A cloneable cancellation token tied to an active fault.
///
/// `FaultHandle` wraps an `Arc<AtomicBool>` so that clones all share the same
/// underlying flag. Calling [`cancel`](FaultHandle::cancel) on any clone
/// cancels the fault for all holders.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::FaultHandle;
///
/// let h1 = FaultHandle::new();
/// let h2 = h1.clone();
///
/// assert!(!h1.is_cancelled());
/// h2.cancel();
/// assert!(h1.is_cancelled()); // shared flag
/// ```
#[derive(Clone, Debug, Default)]
pub struct FaultHandle {
    cancelled: Arc<AtomicBool>,
}

impl FaultHandle {
    /// Create a new, uncancelled handle.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel the fault. All clones of this handle observe the cancellation.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    /// Returns `true` if this handle has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

// ── FaultRegistry ─────────────────────────────────────────────────────────────

/// Runtime collection of active [`FaultHandle`]s, keyed by node identifier.
///
/// The registry holds lightweight cancellation tokens, not the faults
/// themselves. This lets the injection path retain its fault reference while
/// the registry provides a control plane for cancellation queries.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::{FaultHandle, FaultRegistry};
///
/// let mut reg = FaultRegistry::new();
/// reg.register("node-a", FaultHandle::new());
/// reg.register("node-a", FaultHandle::new());
/// assert_eq!(reg.active_count("node-a"), 2);
///
/// reg.cancel_node("node-a");
/// assert_eq!(reg.active_count("node-a"), 0);
/// ```
#[derive(Debug, Default)]
pub struct FaultRegistry {
    inner: HashMap<String, Vec<FaultHandle>>,
}

impl FaultRegistry {
    /// Create an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handle under the given node identifier.
    pub fn register(&mut self, node_id: &str, handle: FaultHandle) {
        self.inner
            .entry(node_id.to_owned())
            .or_default()
            .push(handle);
    }

    /// Cancel all handles registered under `node_id`.
    ///
    /// Cancellation uses the `AtomicBool` interior mutability of each handle,
    /// so only a shared reference to `self` is required.
    pub fn cancel_node(&self, node_id: &str) {
        if let Some(handles) = self.inner.get(node_id) {
            for handle in handles {
                handle.cancel();
            }
        }
    }

    /// Count handles under `node_id` that have **not** been cancelled.
    #[must_use]
    pub fn active_count(&self, node_id: &str) -> usize {
        self.inner.get(node_id).map_or(0, |handles| {
            handles.iter().filter(|h| !h.is_cancelled()).count()
        })
    }

    /// Cancel all handles in the registry, then clear all entries.
    pub fn clear(&mut self) {
        for handles in self.inner.values() {
            for handle in handles {
                handle.cancel();
            }
        }
        self.inner.clear();
    }
}

// ── MalcolmClock ──────────────────────────────────────────────────────────────

/// A clock abstraction for use in testable code.
///
/// Code that relies on wall-clock time should accept a `&dyn MalcolmClock` (or
/// `Arc<dyn MalcolmClock>`) rather than calling `SystemTime::now()` directly.
/// This allows tests to substitute a [`MockClock`] and control time
/// deterministically without touching the system clock.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::{MalcolmClock, RealClock};
///
/// let clock = RealClock;
/// let now = clock.now_ms();
/// assert!(now > 0);
/// ```
pub trait MalcolmClock: Send + Sync {
    /// Returns the current time as milliseconds since the UNIX epoch.
    fn now_ms(&self) -> u64;
}

// ── RealClock ─────────────────────────────────────────────────────────────────

/// The production [`MalcolmClock`] implementation backed by `SystemTime`.
///
/// Returns `0` on the (extremely unlikely) event that the system clock reports
/// a time before the UNIX epoch, and saturates at [`u64::MAX`] if the timestamp
/// exceeds ~584 million years from 1970.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::{MalcolmClock, RealClock};
///
/// let now = RealClock.now_ms();
/// assert!(now > 0);
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct RealClock;

impl MalcolmClock for RealClock {
    fn now_ms(&self) -> u64 {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        u64::try_from(millis).unwrap_or(u64::MAX)
    }
}

// ── MockClock ─────────────────────────────────────────────────────────────────

/// A fully controllable clock for use in tests and simulations.
///
/// All mutating methods take `&self` and use atomic operations so that
/// `MockClock` can be shared across threads via `Arc<MockClock>`.
///
/// # Note
///
/// These methods only affect code that reads time through this clock instance.
/// They do **not** modify the system clock.
///
/// # Example
///
/// ```rust
/// use malcolm::fault::{MalcolmClock, MockClock};
///
/// let clock = MockClock::default();
/// let t0 = clock.now_ms();
/// clock.advance(100);
/// assert_eq!(clock.now_ms(), t0 + 100);
/// ```
#[derive(Clone, Debug, Default)]
pub struct MockClock {
    time_ms: Arc<AtomicU64>,
    frozen: Arc<AtomicBool>,
}

impl MockClock {
    /// Create a new `MockClock` with its counter initialised to `start_ms`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::fault::{MalcolmClock, MockClock};
    ///
    /// let clock = MockClock::at(1_000);
    /// assert_eq!(clock.now_ms(), 1_000);
    /// ```
    #[must_use]
    pub fn at(start_ms: u64) -> Self {
        let clock = Self::default();
        clock.set(start_ms);
        clock
    }

    /// Advance the clock by `ms` milliseconds (saturating).
    ///
    /// No-op while the clock is frozen.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::fault::{MalcolmClock, MockClock};
    ///
    /// let clock = MockClock::default();
    /// clock.advance(50);
    /// assert_eq!(clock.now_ms(), 50);
    /// ```
    pub fn advance(&self, ms: u64) {
        if !self.frozen.load(Ordering::Acquire) {
            self.time_ms.fetch_add(ms, Ordering::AcqRel);
        }
    }

    /// Freeze the clock at its current value.
    ///
    /// While frozen, [`advance`](Self::advance) and [`jump`](Self::jump) are
    /// silently ignored. Call [`set`](Self::set) to unfreeze.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::fault::{MalcolmClock, MockClock};
    ///
    /// let clock = MockClock::at(500);
    /// clock.freeze();
    /// clock.advance(100);
    /// assert_eq!(clock.now_ms(), 500); // still frozen
    /// ```
    pub fn freeze(&self) {
        self.frozen.store(true, Ordering::Release);
    }

    /// Jump the clock forward (`ms > 0`) or backward (`ms < 0`), saturating at `0`
    /// on underflow and at [`u64::MAX`] on overflow.
    ///
    /// No-op while the clock is frozen.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::fault::{MalcolmClock, MockClock};
    ///
    /// let clock = MockClock::at(1_000);
    /// clock.jump(500);
    /// assert_eq!(clock.now_ms(), 1_500);
    /// clock.jump(-200);
    /// assert_eq!(clock.now_ms(), 1_300);
    /// ```
    pub fn jump(&self, ms: i64) {
        if self.frozen.load(Ordering::Acquire) {
            return;
        }
        if ms >= 0 {
            #[expect(
                clippy::cast_sign_loss,
                reason = "ms is non-negative after the if guard"
            )]
            let add = ms as u64;
            self.time_ms.fetch_add(add, Ordering::AcqRel);
        } else {
            let abs_ms = ms.unsigned_abs();
            let mut current = self.time_ms.load(Ordering::Acquire);
            loop {
                let new_val = current.saturating_sub(abs_ms);
                match self.time_ms.compare_exchange_weak(
                    current,
                    new_val,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => break,
                    Err(actual) => current = actual,
                }
            }
        }
    }

    /// Set the clock to an exact millisecond value and unfreeze it.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::fault::{MalcolmClock, MockClock};
    ///
    /// let clock = MockClock::default();
    /// clock.freeze();
    /// clock.set(9_000);
    /// clock.advance(1_000);
    /// assert_eq!(clock.now_ms(), 10_000); // unfrozen by set()
    /// ```
    pub fn set(&self, ms: u64) {
        self.time_ms.store(ms, Ordering::Release);
        self.frozen.store(false, Ordering::Release);
    }
}

impl MalcolmClock for MockClock {
    fn now_ms(&self) -> u64 {
        self.time_ms.load(Ordering::Acquire)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use malcolm_core::types::{FaultEvent, SkipReason};
    use tracing_test::traced_test;

    // ── MockFault ────────────────────────────────────────────────────────────

    struct MockFault;

    impl Fault for MockFault {
        fn inject(&self, ctx: &FaultContext) -> FaultResult {
            let event = FaultEvent {
                fault_type: self.fault_type().to_owned(),
                node_id: ctx.node_id.clone(),
                seed: ctx.seed,
                intensity: 0.8,
                dry_run: false,
                timestamp_ms: ctx.timestamp_ms,
            };
            tracing::info!(
                fault_type = %event.fault_type,
                node_id = %event.node_id,
                seed = event.seed,
                intensity = event.intensity,
                "fault injected",
            );
            FaultResult::Injected(event)
        }

        fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
            let report = DryRunReport {
                fault_type: self.fault_type().to_owned(),
                node_id: ctx.node_id.clone(),
                would_inject: true,
                reason: "mock fault always injects".to_owned(),
            };
            tracing::debug!(
                fault_type = %report.fault_type,
                node_id = %report.node_id,
                would_inject = report.would_inject,
                "dry run completed",
            );
            report
        }

        fn fault_type(&self) -> &'static str {
            "mock"
        }
    }

    fn make_ctx() -> FaultContext {
        FaultContext {
            seed: 42,
            timestamp_ms: 1_000,
            node_id: "node-0".to_owned(),
            profile: malcolm_core::bifurcation::BifurcationProfile::network_partition(),
        }
    }

    // ── FaultHandle tests ────────────────────────────────────────────────────

    #[test]
    fn fault_handle_starts_uncancelled() {
        let handle = FaultHandle::new();
        assert!(!handle.is_cancelled());
    }

    #[test]
    fn fault_handle_cancel_sets_flag() {
        let handle = FaultHandle::new();
        handle.cancel();
        assert!(handle.is_cancelled());
    }

    #[test]
    fn fault_handle_clone_shares_flag() {
        let h1 = FaultHandle::new();
        let h2 = h1.clone();
        h1.cancel();
        assert!(h2.is_cancelled());
    }

    // ── FaultRegistry tests ──────────────────────────────────────────────────

    #[test]
    fn fault_registry_cancel_node() {
        let mut registry = FaultRegistry::new();
        let h1 = FaultHandle::new();
        let h2 = FaultHandle::new();
        registry.register("node-a", h1);
        registry.register("node-a", h2);
        assert_eq!(registry.active_count("node-a"), 2);
        registry.cancel_node("node-a");
        assert_eq!(registry.active_count("node-a"), 0);
    }

    #[test]
    fn fault_registry_active_count_unknown_node() {
        let registry = FaultRegistry::new();
        assert_eq!(registry.active_count("unknown"), 0);
    }

    #[test]
    fn fault_registry_clear_cancels_all() {
        let mut registry = FaultRegistry::new();
        let h = FaultHandle::new();
        let h_clone = h.clone();
        registry.register("node-b", h);
        registry.clear();
        assert!(h_clone.is_cancelled());
        assert_eq!(registry.active_count("node-b"), 0);
    }

    // ── MockFault tests ──────────────────────────────────────────────────────

    #[test]
    fn mock_fault_dry_run_returns_would_inject_true() {
        let fault = MockFault;
        let ctx = make_ctx();
        let report = fault.dry_run(&ctx);
        assert!(report.would_inject);
        assert_eq!(report.fault_type, "mock");
    }

    #[test]
    fn mock_fault_inject_returns_injected() {
        let fault = MockFault;
        let ctx = make_ctx();
        let result = fault.inject(&ctx);
        assert!(matches!(result, FaultResult::Injected(_)));
    }

    #[test]
    fn fault_result_skipped_variant() {
        let result = FaultResult::Skipped(SkipReason::BelowThreshold);
        assert!(matches!(
            result,
            FaultResult::Skipped(SkipReason::BelowThreshold)
        ));
    }

    // ── Tracing tests ────────────────────────────────────────────────────────

    #[traced_test]
    #[test]
    fn inject_emits_info_event() {
        let fault = MockFault;
        let ctx = make_ctx();
        let _ = fault.inject(&ctx);
        assert!(logs_contain("fault injected"));
    }

    #[traced_test]
    #[test]
    fn dry_run_emits_debug_event() {
        let fault = MockFault;
        let ctx = make_ctx();
        let _ = fault.dry_run(&ctx);
        assert!(logs_contain("dry run completed"));
    }
}
