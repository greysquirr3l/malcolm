//! Cleanup registry — the dead-man switch for applied faults.
//!
//! Every [`crate::TargetAdapter::apply`] registers its `AppliedFault`
//! with the registry. On `Drop`, on `SIGINT`, or on `SIGTERM`, the
//! registry iterates registered faults (in reverse order, so a layered
//! set of changes unwinds correctly) and calls
//! [`crate::TargetAdapter::revert`] on each adapter that produced
//! them.
//!
//! Adapters are expected to make `revert` idempotent; the registry
//! will only call it once per registered fault, but a panic in a
//! previous revert handler must not stop the loop.
//!
//! # Signal handling
//!
//! The registry installs `SIGINT`/`SIGTERM` handlers via the
//! `signal-hook` crate the first time it is built. The handler flips
//! an atomic that observers can read via
//! [`Cleanup::signal_received`]. The actual revert is driven by
//! `Drop` of the registry, which runs as the test process unwinds —
//! so a crashed run cannot leave a host partitioned or throttled.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::adapter::{AppliedFault, TargetAdapter};
use crate::error::AgentError;

/// Identifier the registry hands out to adapters for tracking the
/// revert path. The id is also the lookup key for
/// [`Cleanup::revert`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AppliedId(pub u64);

impl std::fmt::Display for AppliedId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AppliedId({})", self.0)
    }
}

/// A record of one applied fault, kept in the registry until it is
/// reverted. The registry holds the adapter by `Arc<dyn TargetAdapter>`
/// so the same adapter instance can serve many registered faults
/// without ownership gymnastics.
struct Entry {
    /// The applied-fault record returned by the adapter.
    applied: AppliedFault,
    /// Adapter that produced the fault, behind an `Arc` so the
    /// registry can call `revert` without owning the adapter.
    adapter: Arc<dyn TargetAdapter>,
}

/// The cleanup registry.
///
/// Construct via [`Cleanup::new`]. The first construction in the
/// process installs the global signal handlers; subsequent
/// constructions are independent local registries that still
/// participate in drop-time cleanup.
pub struct Cleanup {
    /// Monotonic counter that hands out the next id.
    next_id: u64,
    /// Map from `AppliedId` to the entry. Iteration order is
    /// reverse-insertion so the most recent change is reverted first.
    entries: HashMap<AppliedId, Entry>,
    /// Global flag flipped by the signal handler.
    signal_received: Arc<AtomicBool>,
}

impl Default for Cleanup {
    fn default() -> Self {
        Self::new()
    }
}

impl Cleanup {
    /// Construct a new registry and install the process-wide signal
    /// handler the first time it is called.
    #[must_use]
    pub fn new() -> Self {
        install_signal_handlers(&SIGNAL_RECEIVED);
        Self {
            next_id: 1,
            entries: HashMap::new(),
            signal_received: SIGNAL_RECEIVED.clone(),
        }
    }

    /// Allocate a fresh id without registering anything. Useful for
    /// adapters that want to thread an id through their own state
    /// before the registry sees them.
    #[must_use]
    pub const fn next_id(&mut self) -> AppliedId {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        AppliedId(id)
    }

    /// Register an applied fault. Returns the id under which it was
    /// stored so the caller can pass it back to `revert`.
    pub fn register(
        &mut self,
        applied: AppliedFault,
        adapter: Arc<dyn TargetAdapter>,
    ) -> AppliedId {
        let id = self.next_id();
        self.entries.insert(id, Entry { applied, adapter });
        id
    }

    /// Revert a single registered fault. The entry is removed from
    /// the registry whether or not `revert` succeeds so a subsequent
    /// `revert` call is a no-op rather than an infinite loop.
    ///
    /// # Errors
    ///
    /// - [`AgentError::UnknownAppliedFault`] if the id is not in the
    ///   registry.
    /// - [`AgentError::AdapterFailure`] if the adapter's `revert`
    ///   returned an error.
    pub fn revert(&mut self, id: AppliedId) -> Result<(), AgentError> {
        let entry = self
            .entries
            .remove(&id)
            .ok_or(AgentError::UnknownAppliedFault { id: id.0 })?;
        entry.adapter.revert(&entry.applied)
    }

    /// Revert every registered fault in reverse-insertion order.
    /// Returns the number of faults that were reverted successfully.
    /// The registry is empty after this call regardless of outcomes.
    pub fn revert_all(&mut self) -> usize {
        // Reverse-insertion order: gather ids, sort by descending
        // id (since ids are monotonic), and pop in that order.
        let mut ids: Vec<AppliedId> = self.entries.keys().copied().collect();
        ids.sort_by_key(|id| std::cmp::Reverse(id.0));
        let mut reverted = 0usize;
        for id in ids {
            match self.revert(id) {
                Ok(()) => reverted = reverted.saturating_add(1),
                Err(err) => {
                    tracing::error!(
                        target: "malcolm_agent::cleanup",
                        applied_id = %id,
                        error = %err,
                        "revert failed during cleanup; continuing with next entry"
                    );
                }
            }
        }
        reverted
    }

    /// Number of faults still registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no faults are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `true` if a `SIGINT` or `SIGTERM` has been observed since the
    /// registry was created. Adapters and tests can use this to
    /// short-circuit long-running operations when the process is
    /// being torn down.
    #[must_use]
    pub fn signal_received(&self) -> bool {
        self.signal_received.load(Ordering::SeqCst)
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if !self.entries.is_empty() {
            tracing::info!(
                target: "malcolm_agent::cleanup",
                count = self.entries.len(),
                "cleanup registry dropping: reverting remaining applied faults"
            );
            self.revert_all();
        }
    }
}

/// Global atomic flipped by the signal handler. Shared between the
/// handler and every `Cleanup` instance.
static SIGNAL_RECEIVED: std::sync::LazyLock<Arc<AtomicBool>> =
    std::sync::LazyLock::new(|| Arc::new(AtomicBool::new(false)));

/// Install `SIGINT` and `SIGTERM` handlers exactly once per process
/// using the `signal-hook` safe wrapper. The handler stores into the
/// shared atomic; revert logic runs at registry-drop time.
fn install_signal_handlers(flag: &Arc<AtomicBool>) {
    use std::sync::Once;

    static INSTALLED: Once = Once::new();
    INSTALLED.call_once(|| {
        let flag_a = flag.clone();
        let flag_b = flag.clone();
        // signal-hook's safe API registers a closure that runs in a
        // dedicated thread managed by the crate. The closure is
        // ordinary code, not a signal handler, so any tracing/logging
        // it does is safe.
        let result = signal_hook::flag::register(signal_hook::consts::SIGINT, flag_a);
        let result2 = signal_hook::flag::register(signal_hook::consts::SIGTERM, flag_b);
        if result.is_err() || result2.is_err() {
            tracing::warn!(
                target: "malcolm_agent::cleanup",
                "could not install SIGINT/SIGTERM handler; cleanup will still run on Drop"
            );
        }
    });
}
