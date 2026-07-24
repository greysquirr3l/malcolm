//! Test utilities shared across the `malcolm` crate.
//!
//! Behaviour-only helpers that keep tests free of `#[allow(clippy::panic)]`
//! and `#[allow(clippy::indexing_slicing)]` markers. Anything in this module
//! is gated behind `#[cfg(test)]` so it never enters the production build,
//! and the helpers themselves are `unwrap`-free and match-friendly.

use std::sync::{Mutex, MutexGuard};

/// Acquire a mutex lock, recovering from poison by extracting the inner data.
///
/// In tests, mutex poison indicates a previous panic while holding the lock.
/// We extract the inner value anyway so the test can continue and report
/// clearly rather than cascade-fail with a generic "poisoned" panic.
///
/// In a production build this is unreachable — production code never panics
/// with a lock held, so the `Err` arm is dead during normal operation.
#[cfg(test)]
#[expect(
    unreachable_pub,
    reason = "test_util module is `pub(crate)`; this helper is visible everywhere within the crate"
)]
pub fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Slice a borrowed buffer by `[..n]` without triggering
/// `clippy::indexing_slicing`. Used by UDP and buffer-recv code where `n`
/// is bounded by the recv return value and is guaranteed to be
/// `<= buf.len()` by the underlying syscall.
#[cfg(all(test, feature = "statsd"))]
#[expect(
    unreachable_pub,
    reason = "test_util module is `pub(crate)`; this helper is visible everywhere within the crate"
)]
pub fn slice_recv<T>(buf: &[T], n: usize) -> &[T] {
    debug_assert!(n <= buf.len(), "recv returned n > buf.len()");
    if let Some(s) = buf.get(..n) { s } else { &[] }
}
