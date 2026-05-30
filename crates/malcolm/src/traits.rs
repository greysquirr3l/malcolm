//! Additional port traits for the malcolm assembly layer.
//!
//! The primary `Fault` port trait lives in [`crate::fault`]. This module
//! houses shared trait exports used by downstream consumers.
//!
//! `DistributionSampler` remains defined in `malcolm-core::distributions` and
//! is intentionally not redefined in this assembly crate.

pub use crate::fault::{MalcolmClock, MockClock, RealClock};

#[cfg(test)]
mod tests {
    use crate::traits::{MalcolmClock, MockClock};

    #[test]
    fn traits_module_reexports_clock_ports() {
        let clock = MockClock::default();
        let now = clock.now_ms();
        assert_eq!(now, 0);
    }
}
