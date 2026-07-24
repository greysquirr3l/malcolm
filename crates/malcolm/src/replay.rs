//! Deterministic fault replay engine and scenario recording.
//!
//! # Example
//!
//! ```rust
//! use malcolm::fault::FaultContext;
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::replay::{RecordingHarness, ReplayHarness};
//! use malcolm::scenario::ChaosScenario;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let scenario = ChaosScenario::builder()
//!     .name("replay-demo")
//!     .seed(42)
//!     .add_fault(PacketLoss::builder().seed(7).intensity(0.8).build())
//!     .profile(BifurcationProfile::network_partition())
//!     .build();
//! let mut ctx = FaultContext {
//!     seed: 42,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! let record = RecordingHarness::new(&scenario).record(&mut ctx);
//! let replay = ReplayHarness::new(record);
//! assert!(replay.verify());
//! ```

use serde::{Deserialize, Serialize};

use crate::fault::FaultContext;
use crate::scenario::{ChaosScenario, ScenarioEvent, ScenarioRegime, ScenarioReport};

/// Sealed, authenticated-encrypted telemetry envelopes for scenario records.
pub mod envelope;

/// Serializable snapshot of one topology edge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologyEdgeSnapshot {
    /// Source node id.
    pub from: String,
    /// Destination node id.
    pub to: String,
    /// Edge propagation weight.
    pub weight: f64,
}

/// Serializable topology snapshot attached to a scenario record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologySnapshot {
    /// Topology name.
    pub name: String,
    /// All node ids in sorted order.
    pub nodes: Vec<String>,
    /// All directed edges in sorted order.
    pub edges: Vec<TopologyEdgeSnapshot>,
}

/// Full persisted record of one scenario run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenarioRecord {
    /// Scenario name.
    pub scenario_name: String,
    /// Scenario seed.
    pub seed: u64,
    /// Bifurcation profile label.
    pub profile_label: String,
    /// Classified regime from the original run.
    pub regime: ScenarioRegime,
    /// Fault types in execution order.
    pub fault_sequence: Vec<String>,
    /// Recorded event sequence.
    pub events: Vec<ScenarioEvent>,
    /// Optional topology snapshot.
    pub topology: Option<TopologySnapshot>,
    /// First event timestamp in ms.
    pub first_timestamp_ms: u64,
    /// Last event timestamp in ms.
    pub last_timestamp_ms: u64,
    /// Deterministic integrity tag for tamper detection.
    pub integrity_tag: u64,
}

impl ScenarioRecord {
    /// Build a record from scenario metadata and a run report.
    #[must_use]
    pub fn from_report(scenario: &ChaosScenario, report: &ScenarioReport) -> Self {
        let topology = scenario.topology().map(|topology| TopologySnapshot {
            name: topology.name().to_owned(),
            nodes: topology.node_ids(),
            edges: topology
                .edges()
                .into_iter()
                .map(|(from, to, weight)| TopologyEdgeSnapshot { from, to, weight })
                .collect(),
        });

        let fault_sequence = report
            .events
            .iter()
            .map(|event| event.fault_type.clone())
            .collect::<Vec<_>>();

        let first_timestamp_ms = report.events.first().map_or(0, |event| event.timestamp_ms);
        let last_timestamp_ms = report.events.last().map_or(0, |event| event.timestamp_ms);

        let mut record = Self {
            scenario_name: scenario.name().to_owned(),
            seed: scenario.seed(),
            profile_label: scenario.profile().label.to_owned(),
            regime: report.regime,
            fault_sequence,
            events: report.events.clone(),
            topology,
            first_timestamp_ms,
            last_timestamp_ms,
            integrity_tag: 0,
        };
        record.integrity_tag = record.compute_integrity_tag();
        record
    }

    /// Serialize this record to JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if this record cannot be serialized to JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize one record from JSON.
    ///
    /// # Errors
    ///
    /// Returns an error if `payload` is not valid JSON for [`ScenarioRecord`].
    pub fn from_json(payload: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(payload)
    }

    /// Serialize this record to bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if this record cannot be serialized to JSON bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserialize one record from bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if `bytes` are not valid JSON for [`ScenarioRecord`].
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// Serialize this record to YAML.
    ///
    /// YAML is a friendlier format for operator-edited scenario records than
    /// JSON: comments are allowed, quoting is more lenient, and the output
    /// diffs cleanly in code review.
    ///
    /// # Errors
    ///
    /// Returns an error if this record cannot be serialized to YAML.
    pub fn to_yaml(&self) -> Result<String, serde_yaml::Error> {
        serde_yaml::to_string(self)
    }

    /// Deserialize one record from YAML.
    ///
    /// # Errors
    ///
    /// Returns an error if `payload` is not valid YAML for [`ScenarioRecord`].
    pub fn from_yaml(payload: &str) -> Result<Self, serde_yaml::Error> {
        serde_yaml::from_str(payload)
    }

    /// Verify deterministic integrity tag.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.compute_integrity_tag() == self.integrity_tag
    }

    fn compute_integrity_tag(&self) -> u64 {
        let mut hash = FNV_OFFSET_BASIS;
        push_bytes(&mut hash, self.scenario_name.as_bytes());
        push_bytes(&mut hash, &self.seed.to_le_bytes());
        push_bytes(&mut hash, self.profile_label.as_bytes());
        push_bytes(&mut hash, format!("{:?}", self.regime).as_bytes());

        for fault_type in &self.fault_sequence {
            push_bytes(&mut hash, fault_type.as_bytes());
        }

        for event in &self.events {
            push_bytes(&mut hash, event.fault_type.as_bytes());
            push_bytes(&mut hash, event.node_id.as_bytes());
            push_bytes(&mut hash, &event.seed.to_le_bytes());
            push_bytes(&mut hash, &event.intensity.to_le_bytes());
            push_bytes(&mut hash, &[u8::from(event.dry_run)]);
            push_bytes(&mut hash, &event.timestamp_ms.to_le_bytes());
        }

        if let Some(topology) = &self.topology {
            push_bytes(&mut hash, topology.name.as_bytes());
            for node in &topology.nodes {
                push_bytes(&mut hash, node.as_bytes());
            }
            for edge in &topology.edges {
                push_bytes(&mut hash, edge.from.as_bytes());
                push_bytes(&mut hash, edge.to.as_bytes());
                push_bytes(&mut hash, &edge.weight.to_le_bytes());
            }
        }

        push_bytes(&mut hash, &self.first_timestamp_ms.to_le_bytes());
        push_bytes(&mut hash, &self.last_timestamp_ms.to_le_bytes());

        hash
    }
}

fn push_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 1_099_511_628_211;

/// Captures scenario execution and builds a [`ScenarioRecord`].
pub struct RecordingHarness<'a> {
    scenario: &'a ChaosScenario,
}

impl<'a> RecordingHarness<'a> {
    /// Create a recording harness for one scenario.
    #[must_use]
    pub const fn new(scenario: &'a ChaosScenario) -> Self {
        Self { scenario }
    }

    /// Run scenario and build a deterministic record.
    #[must_use]
    pub fn record(&self, ctx: &mut FaultContext) -> ScenarioRecord {
        let report = self.scenario.run(ctx);
        ScenarioRecord::from_report(self.scenario, &report)
    }
}

/// Replays and verifies one persisted scenario record.
pub struct ReplayHarness {
    record: ScenarioRecord,
}

impl ReplayHarness {
    /// Create a replay harness from one record.
    #[must_use]
    pub const fn new(record: ScenarioRecord) -> Self {
        Self { record }
    }

    /// Return the reconstructed scenario report from this record.
    #[must_use]
    pub fn replay(&self) -> ScenarioReport {
        ScenarioReport {
            name: self.record.scenario_name.clone(),
            seed: self.record.seed,
            regime: self.record.regime,
            events: self.record.events.clone(),
            total_duration_ms: self
                .record
                .last_timestamp_ms
                .saturating_sub(self.record.first_timestamp_ms),
        }
    }

    /// Verify replay integrity.
    #[must_use]
    pub fn verify(&self) -> bool {
        self.record.verify()
    }

    /// Borrow underlying record.
    #[must_use]
    pub const fn record(&self) -> &ScenarioRecord {
        &self.record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::faults::network::PacketLoss;
    use crate::scenario::ChaosScenario;
    use crate::topology::Topology;
    use malcolm_core::bifurcation::BifurcationProfile;

    fn make_ctx() -> FaultContext {
        FaultContext {
            seed: 42,
            timestamp_ms: 100,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    fn make_scenario() -> ChaosScenario {
        let topology = Topology::builder()
            .name("cluster")
            .add_edge("node-0", "node-1", 1.0)
            .build();

        ChaosScenario::builder()
            .name("recordable")
            .seed(42)
            .add_fault(PacketLoss::builder().seed(7).intensity(0.8).build())
            .profile(BifurcationProfile::network_partition())
            .topology(topology)
            .build()
    }

    #[test]
    fn record_replay_verify_matches_event_sequence() {
        let scenario = make_scenario();
        let mut ctx = make_ctx();

        let record = RecordingHarness::new(&scenario).record(&mut ctx);
        let replay = ReplayHarness::new(record.clone());
        let replay_report = replay.replay();

        assert!(replay.verify());
        assert_eq!(record.events, replay_report.events);
    }

    #[test]
    fn mutating_record_data_causes_verify_false() {
        let scenario = make_scenario();
        let mut ctx = make_ctx();
        let mut record = RecordingHarness::new(&scenario).record(&mut ctx);

        if let Some(first) = record.events.first_mut() {
            first.fault_type.push_str("-tampered");
        }

        let replay = ReplayHarness::new(record);
        assert!(!replay.verify());
    }

    #[test]
    fn scenario_record_json_roundtrip_is_lossless() {
        let scenario = make_scenario();
        let mut ctx = make_ctx();
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let payload = record.to_json();
        assert!(payload.is_ok());
        let Some(json) = payload.ok() else {
            return;
        };

        let decoded = ScenarioRecord::from_json(&json);
        assert!(decoded.is_ok());

        let Some(decoded_record) = decoded.ok() else {
            return;
        };
        assert_eq!(record, decoded_record);
    }

    #[test]
    fn scenario_record_yaml_roundtrip_is_lossless() -> Result<(), Box<dyn std::error::Error>> {
        let scenario = make_scenario();
        let mut ctx = make_ctx();
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let yaml = record.to_yaml()?;
        let decoded = ScenarioRecord::from_yaml(&yaml)?;
        assert_eq!(record, decoded);
        Ok(())
    }

    #[test]
    fn scenario_record_yaml_rejects_garbage() {
        let result = ScenarioRecord::from_yaml("this: is_not_a_scenario_record: -");
        assert!(result.is_err());
    }
}
