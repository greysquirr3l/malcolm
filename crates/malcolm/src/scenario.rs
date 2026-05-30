//! Composable named fault bundles with seed and profile support.
//!
//! # Example
//!
//! ```rust
//! use malcolm::fault::FaultContext;
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::scenario::ChaosScenario;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let scenario = ChaosScenario::builder()
//!     .name("flaky-network")
//!     .seed(1337)
//!     .add_fault(PacketLoss::builder().seed(42).build())
//!     .profile(BifurcationProfile::network_partition())
//!     .build();
//!
//! let mut ctx = FaultContext {
//!     seed: 1337,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! let report = scenario.run(&mut ctx);
//! assert_eq!(report.name, "flaky-network");
//! ```

use std::time::Instant;

use serde::{Deserialize, Serialize};

use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult};

use crate::fault::{Fault, FaultContext};
use crate::topology::Topology;

/// Serializable scenario regime used in report payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioRegime {
    /// Stable operating region.
    Stable,
    /// Near-threshold operating region.
    Sensitive,
    /// Chaotic operating region.
    Chaotic,
}

impl From<Regime> for ScenarioRegime {
    fn from(value: Regime) -> Self {
        match value {
            Regime::Stable => Self::Stable,
            Regime::Sensitive => Self::Sensitive,
            Regime::Chaotic => Self::Chaotic,
            _ => unreachable!("future Regime variants should not appear in malcolm-core"),
        }
    }
}

/// Serializable event record used in [`ScenarioReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioEvent {
    /// Identifier for the kind of fault.
    pub fault_type: String,
    /// Identifier of the targeted node.
    pub node_id: String,
    /// Seed used for deterministic replay.
    pub seed: u64,
    /// Normalised fault intensity in `[0.0, 1.0]`.
    pub intensity: f64,
    /// Whether this event came from dry-run mode.
    pub dry_run: bool,
    /// Event timestamp in milliseconds.
    pub timestamp_ms: u64,
}

impl From<FaultEvent> for ScenarioEvent {
    fn from(value: FaultEvent) -> Self {
        Self {
            fault_type: value.fault_type,
            node_id: value.node_id,
            seed: value.seed,
            intensity: value.intensity,
            dry_run: value.dry_run,
            timestamp_ms: value.timestamp_ms,
        }
    }
}

/// Full report emitted from one scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioReport {
    /// Scenario name.
    pub name: String,
    /// Scenario seed.
    pub seed: u64,
    /// Classified regime from observed fault intensity.
    pub regime: ScenarioRegime,
    /// Injected fault events in execution order.
    pub events: Vec<ScenarioEvent>,
    /// Total wall time spent running the scenario.
    pub total_duration_ms: u64,
}

impl ScenarioReport {
    /// Serialize this report to JSON for post-mortem workflows.
    ///
    /// # Errors
    ///
    /// Returns an error if this report cannot be serialized to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Dry-run report for a full scenario.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioDryRunReport {
    /// Scenario name.
    pub name: String,
    /// Scenario seed.
    pub seed: u64,
    /// Dry-run output from each fault in execution order.
    pub fault_reports: Vec<DryRunReport>,
    /// `true` when at least one fault would inject.
    pub would_inject_any: bool,
}

/// A composable scenario of named faults sharing a profile and seed.
pub struct ChaosScenario {
    name: String,
    seed: u64,
    faults: Vec<Box<dyn Fault>>,
    profile: BifurcationProfile,
    topology: Option<Topology>,
}

impl ChaosScenario {
    /// Begin building a scenario.
    #[must_use]
    pub fn builder() -> ChaosScenarioBuilder {
        ChaosScenarioBuilder::default()
    }

    /// Scenario name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Scenario seed.
    #[must_use]
    pub const fn seed(&self) -> u64 {
        self.seed
    }

    /// Scenario profile.
    #[must_use]
    pub const fn profile(&self) -> BifurcationProfile {
        self.profile
    }

    /// Optional topology attached to this scenario.
    #[must_use]
    pub const fn topology(&self) -> Option<&Topology> {
        self.topology.as_ref()
    }

    /// Execute all faults in order and return a deterministic report.
    pub fn run(&self, ctx: &mut FaultContext) -> ScenarioReport {
        let span = tracing::info_span!(
            target: "malcolm",
            "scenario_run",
            fault_type = "scenario_run",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = 0.0_f64,
            regime = ?ScenarioRegime::from(classify(0.0, &self.profile)),
            dry_run = false,
            scenario_name = %self.name,
            profile = %self.profile.label,
        );
        let _guard = span.enter();
        let start = Instant::now();

        let mut events = Vec::new();
        let mut max_intensity = 0.0_f64;

        for fault in &self.faults {
            let fault_ctx = FaultContext {
                seed: self.seed,
                timestamp_ms: ctx.timestamp_ms,
                node_id: ctx.node_id.clone(),
                profile: self.profile,
            };

            match fault.inject(&fault_ctx) {
                FaultResult::Injected(event) => {
                    if event.intensity > max_intensity {
                        max_intensity = event.intensity;
                    }
                    events.push(ScenarioEvent::from(event));
                }
                FaultResult::Skipped(reason) => {
                    tracing::info!(
                        target: "malcolm",
                        fault_type = fault.fault_type(),
                        node_id = %ctx.node_id,
                        seed = self.seed,
                        intensity = 0.0_f64,
                        regime = ?ScenarioRegime::Stable,
                        dry_run = true,
                        skip_reason = ?reason,
                        "fault skipped during scenario run",
                    );
                }
            }
        }

        let regime = ScenarioRegime::from(classify(max_intensity, &self.profile));
        let total_duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);

        tracing::info!(
            target: "malcolm",
            fault_type = "scenario_run",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = max_intensity,
            regime = ?regime,
            dry_run = false,
            events = events.len(),
            total_duration_ms,
            has_topology = self.topology.is_some(),
            "scenario run complete",
        );

        ScenarioReport {
            name: self.name.clone(),
            seed: self.seed,
            regime,
            events,
            total_duration_ms,
        }
    }

    /// Execute a full dry-run across all scenario faults.
    #[must_use]
    pub fn dry_run(&self, ctx: &FaultContext) -> ScenarioDryRunReport {
        let mut fault_reports = Vec::with_capacity(self.faults.len());

        for fault in &self.faults {
            let fault_ctx = FaultContext {
                seed: self.seed,
                timestamp_ms: ctx.timestamp_ms,
                node_id: ctx.node_id.clone(),
                profile: self.profile,
            };
            fault_reports.push(fault.dry_run(&fault_ctx));
        }

        let would_inject_any = fault_reports.iter().any(|report| report.would_inject);

        tracing::debug!(
            target: "malcolm",
            fault_type = "scenario_dry_run",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = 0.0_f64,
            regime = ?ScenarioRegime::Stable,
            dry_run = true,
            scenario_name = %self.name,
            "scenario dry-run complete",
        );

        ScenarioDryRunReport {
            name: self.name.clone(),
            seed: self.seed,
            fault_reports,
            would_inject_any,
        }
    }
}

/// Builder for [`ChaosScenario`].
#[derive(Default)]
pub struct ChaosScenarioBuilder {
    name: Option<String>,
    seed: Option<u64>,
    faults: Vec<Box<dyn Fault>>,
    profile: Option<BifurcationProfile>,
    topology: Option<Topology>,
}

impl ChaosScenarioBuilder {
    /// Set scenario name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set scenario seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Add one fault to this scenario.
    #[must_use]
    pub fn add_fault<F: Fault + 'static>(mut self, fault: F) -> Self {
        self.faults.push(Box::new(fault));
        self
    }

    /// Set scenario bifurcation profile.
    #[must_use]
    pub const fn profile(mut self, profile: BifurcationProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Optionally attach topology metadata.
    #[must_use]
    pub fn topology(mut self, topology: Topology) -> Self {
        self.topology = Some(topology);
        self
    }

    /// Build the final scenario.
    #[must_use]
    pub fn build(self) -> ChaosScenario {
        ChaosScenario {
            name: self.name.unwrap_or_else(|| "default-scenario".to_owned()),
            seed: self.seed.unwrap_or(0),
            faults: self.faults,
            profile: self
                .profile
                .unwrap_or_else(BifurcationProfile::network_partition),
            topology: self.topology,
        }
    }
}

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use super::*;
    use crate::faults::network::PacketLoss;

    struct AlwaysSkipFault;

    impl Fault for AlwaysSkipFault {
        fn inject(&self, _ctx: &FaultContext) -> FaultResult {
            FaultResult::Skipped(malcolm_core::types::SkipReason::DryRun)
        }

        fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
            DryRunReport {
                fault_type: self.fault_type().to_owned(),
                node_id: ctx.node_id.clone(),
                would_inject: false,
                reason: "skipped in test".to_owned(),
            }
        }

        fn fault_type(&self) -> &'static str {
            "always_skip"
        }
    }

    fn make_ctx(seed: u64, node: &str, profile: BifurcationProfile) -> FaultContext {
        FaultContext {
            seed,
            timestamp_ms: 42,
            node_id: node.to_owned(),
            profile,
        }
    }

    #[test]
    fn same_seed_and_faults_produce_identical_reports() {
        let scenario_a = ChaosScenario::builder()
            .name("deterministic")
            .seed(7)
            .add_fault(PacketLoss::builder().seed(11).intensity(0.8).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let scenario_b = ChaosScenario::builder()
            .name("deterministic")
            .seed(7)
            .add_fault(PacketLoss::builder().seed(11).intensity(0.8).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx_a = make_ctx(7, "node-a", BifurcationProfile::network_partition());
        let mut ctx_b = make_ctx(7, "node-a", BifurcationProfile::network_partition());

        let report_a = scenario_a.run(&mut ctx_a);
        let report_b = scenario_b.run(&mut ctx_b);

        assert_eq!(report_a.name, report_b.name);
        assert_eq!(report_a.seed, report_b.seed);
        assert_eq!(report_a.regime, report_b.regime);
        assert_eq!(report_a.events, report_b.events);
    }

    #[test]
    fn dry_run_returns_no_side_effects_for_mock_fault() -> Result<(), std::io::Error> {
        let scenario = ChaosScenario::builder()
            .name("dry-run")
            .seed(9)
            .add_fault(AlwaysSkipFault)
            .profile(BifurcationProfile::network_partition())
            .build();
        let ctx = make_ctx(9, "dry-node", BifurcationProfile::network_partition());

        let report = scenario.dry_run(&ctx);

        assert_eq!(report.name, "dry-run");
        assert_eq!(report.fault_reports.len(), 1);
        assert!(!report.would_inject_any);
        let first_report = report
            .fault_reports
            .first()
            .ok_or_else(|| std::io::Error::other("expected one fault report"))?;
        assert_eq!(first_report.fault_type, "always_skip");

        Ok(())
    }

    #[test]
    fn scenario_report_serializes_to_json() {
        let scenario = ChaosScenario::builder()
            .name("json")
            .seed(13)
            .add_fault(PacketLoss::builder().seed(1).intensity(0.7).build())
            .profile(BifurcationProfile::network_partition())
            .build();
        let mut ctx = make_ctx(13, "node-j", BifurcationProfile::network_partition());

        let report = scenario.run(&mut ctx);
        let json = report.to_json();

        assert!(json.is_ok());
        let payload = json.unwrap_or_default();
        assert!(payload.contains("\"name\":\"json\""));
    }

    #[test]
    #[traced_test]
    fn chaotic_regime_reflected_when_threshold_exceeded() {
        let scenario = ChaosScenario::builder()
            .name("chaotic")
            .seed(21)
            .add_fault(PacketLoss::builder().seed(3).intensity(0.95).build())
            .profile(BifurcationProfile::network_partition())
            .build();
        let mut ctx = make_ctx(21, "node-c", BifurcationProfile::network_partition());

        let report = scenario.run(&mut ctx);

        assert_eq!(report.regime, ScenarioRegime::Chaotic);
        assert!(logs_contain("scenario run complete"));
    }
}
