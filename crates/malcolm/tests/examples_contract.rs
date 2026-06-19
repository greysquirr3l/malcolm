//! Contract tests for the public `malcolm` examples.
//!
//! These tests pin the behaviour that `simulation`, `replay_demo`, and the
//! async service example must guarantee from a consumer's perspective.

use std::error::Error;

use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::malcolm;
use malcolm::replay::{RecordingHarness, ScenarioRecord};
use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::lyapunov::LyapunovScorer;

#[test]
fn simulation_path_produces_positive_lyapunov() {
    let lambda = LyapunovScorer::compute(3.9, 2000);
    assert!(
        lambda > 0.0,
        "expected chaotic regime to have positive lambda, got {lambda}"
    );
}

#[test]
fn replay_demo_contract_verifies_true_for_faithful_replay() {
    let scenario = malcolm! {
        name: "replay-contract",
        seed: 42,
        profile: BifurcationProfile::network_partition(),
        faults: [
            PacketLoss::builder().seed(3).intensity(0.9).build(),
        ],
    };

    let mut ctx = FaultContext {
        seed: 42,
        timestamp_ms: 0,
        node_id: "edge-0".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };

    let record = RecordingHarness::new(&scenario).record(&mut ctx);
    assert!(record.verify(), "expected faithful recording to verify");
}

#[test]
fn replay_integrity_fails_for_tampered_record() -> Result<(), Box<dyn Error>> {
    let scenario = malcolm! {
        name: "replay-tamper",
        seed: 9,
        profile: BifurcationProfile::network_partition(),
        faults: [
            PacketLoss::builder().seed(4).intensity(0.8).build(),
        ],
    };

    let mut ctx = FaultContext {
        seed: 9,
        timestamp_ms: 0,
        node_id: "edge-1".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };

    let record = RecordingHarness::new(&scenario).record(&mut ctx);
    let bytes = record.to_bytes()?;

    let mut tampered = ScenarioRecord::from_bytes(&bytes)?;
    tampered.seed = tampered.seed.saturating_add(1);

    assert!(
        !tampered.verify(),
        "expected tampered record verification to fail"
    );
    Ok(())
}
