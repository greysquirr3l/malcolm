//! Named scenario presets for common chaos patterns.
//!
//! Presets are pre-configured [`ChaosScenarioBuilder`] values that capture
//! recurring failure modes. They exist so operators can
//! describe a scenario by name (`flaky_net`, `byzantine_cluster`, ...) and
//! override only the parameters that matter for their environment.
//!
//! # Example
//!
//! ```rust
//! use malcolm::fault::FaultContext;
//! use malcolm::presets;
//! use malcolm::scenario::ChaosScenario;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let scenario = presets::flaky_net()
//!     .seed(7)
//!     .profile(BifurcationProfile::network_partition())
//!     .build();
//! let mut ctx = FaultContext {
//!     seed: 7,
//!     timestamp_ms: 0,
//!     node_id: "edge-0".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! let report = scenario.run(&mut ctx);
//! assert!(!report.events.is_empty());
//! ```

use malcolm_core::bifurcation::BifurcationProfile;

use crate::faults::byzantine::{LyingNode, SlowCorrect};
use crate::faults::clock::{ClockJump, JumpDirection};
use crate::faults::network::{LatencySpike, NetworkPartition, NoiseType, PacketLoss};
use crate::faults::resource::{CpuThrottle, MemoryPressure};
use crate::scenario::ChaosScenarioBuilder;

/// Build the `flaky_net` preset: a network partition layered with packet loss.
///
/// Models a link that occasionally drops packets before tearing down entirely.
/// The combined intensity is high enough to push the scenario into the
/// `Sensitive` regime against the `network_partition` profile.
#[must_use]
pub fn flaky_net() -> ChaosScenarioBuilder {
    ChaosScenarioBuilder::default()
        .name("flaky_net")
        .add_fault(
            PacketLoss::builder()
                .seed(11)
                .alpha(2.0)
                .x_min(1.0)
                .intensity(0.7)
                .build(),
        )
        .add_fault(
            NetworkPartition::builder()
                .seed(13)
                .alpha(1.5)
                .intensity(0.6)
                .build(),
        )
        .profile(BifurcationProfile::network_partition())
}

/// Build the `slow_disk` preset: heavy-tailed latency spikes plus memory pressure.
///
/// Models a disk subsystem that occasionally stalls for hundreds of
/// milliseconds while the host runs short on free memory. Useful for
/// backpressure and timeout-retry logic tests.
#[must_use]
pub fn slow_disk() -> ChaosScenarioBuilder {
    ChaosScenarioBuilder::default()
        .name("slow_disk")
        .add_fault(
            LatencySpike::builder()
                .seed(17)
                .base_ms(50.0)
                .sigma(0.6)
                .noise(NoiseType::Brown)
                .intensity(0.8)
                .build(),
        )
        .add_fault(
            MemoryPressure::builder()
                .seed(19)
                .max_bytes(4_096)
                .duration_ms(0)
                .intensity(0.4)
                .build(),
        )
        .profile(BifurcationProfile::latency_cascade())
}

/// Build the `byzantine_cluster` preset: a node that lies occasionally and
/// returns late responses on top of the lies.
///
/// Models a partially-compromised consensus participant. Useful for
/// testing BFT-style quorum and leader election logic.
#[must_use]
pub fn byzantine_cluster() -> ChaosScenarioBuilder {
    ChaosScenarioBuilder::default()
        .name("byzantine_cluster")
        .add_fault(
            LyingNode::builder()
                .seed(23)
                .payload(vec![0xAA, 0xBB, 0xCC, 0xDD])
                .byzantine_probability(0.3)
                .build(),
        )
        .add_fault(
            SlowCorrect::builder()
                .seed(29)
                .payload(vec![0xAA, 0xBB, 0xCC, 0xDD])
                .mu(4.0)
                .sigma(0.5)
                .build(),
        )
        .profile(BifurcationProfile::byzantine_node())
}

/// Build the `clock_drift` preset: a forward clock jump on a mock clock.
///
/// Useful for TTL-based retry logic, monotonic-clock assumptions, and
/// distributed lock expiry.
#[must_use]
pub fn clock_drift() -> ChaosScenarioBuilder {
    use crate::fault::MockClock;

    ChaosScenarioBuilder::default()
        .name("clock_drift")
        .add_fault(
            ClockJump::builder()
                .seed(31)
                .mu(3.0)
                .sigma(0.4)
                .direction(JumpDirection::Forward)
                .clock(Box::new(MockClock::default()))
                .build(),
        )
        .profile(BifurcationProfile::clock_skew())
}

/// Build the `memory_pressure` preset: sustained memory and CPU pressure.
///
/// Models a process under load. Useful for backpressure, GC pressure, and
/// thread-pool exhaustion tests.
#[must_use]
pub fn memory_pressure() -> ChaosScenarioBuilder {
    ChaosScenarioBuilder::default()
        .name("memory_pressure")
        .add_fault(
            MemoryPressure::builder()
                .seed(37)
                .max_bytes(8_192)
                .duration_ms(0)
                .intensity(0.7)
                .build(),
        )
        .add_fault(
            CpuThrottle::builder()
                .seed(41)
                .fraction(0.5)
                .duration_ms(20)
                .build(),
        )
        .profile(BifurcationProfile::memory_pressure())
}

/// All preset names, in the order they appear in [`preset`].
///
/// Useful for the CLI runner's `--list-presets` flag.
pub const PRESET_NAMES: &[&str] = &[
    "flaky_net",
    "slow_disk",
    "byzantine_cluster",
    "clock_drift",
    "memory_pressure",
];

/// Look up a preset by its [`PRESET_NAMES`] string.
#[must_use]
pub fn preset(name: &str) -> Option<ChaosScenarioBuilder> {
    match name {
        "flaky_net" => Some(flaky_net()),
        "slow_disk" => Some(slow_disk()),
        "byzantine_cluster" => Some(byzantine_cluster()),
        "clock_drift" => Some(clock_drift()),
        "memory_pressure" => Some(memory_pressure()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault::FaultContext;

    fn ctx(seed: u64) -> FaultContext {
        FaultContext {
            seed,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    fn every_named_preset_resolves() {
        for name in PRESET_NAMES {
            assert!(preset(name).is_some(), "preset '{name}' should resolve");
        }
    }

    #[test]
    fn unknown_preset_returns_none() {
        assert!(preset("does-not-exist").is_none());
    }

    #[test]
    fn presets_emit_at_least_one_event() {
        for name in PRESET_NAMES {
            let builder = preset(name).expect("known preset");
            // A builder must be buildable into a scenario.
            let scenario = builder.name(*name).seed(42).build();
            let mut context = ctx(42);
            let report = scenario.run(&mut context);
            assert!(
                !report.events.is_empty(),
                "preset '{name}' should inject at least one event"
            );
        }
    }

    #[test]
    fn preset_names_are_unique() {
        let mut sorted: Vec<&str> = PRESET_NAMES.to_vec();
        sorted.dedup();
        assert_eq!(sorted.len(), PRESET_NAMES.len(), "duplicate preset name");
    }
}
