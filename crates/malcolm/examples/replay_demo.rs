//! End-to-end replay demo: build, record, serialize, reload, and verify.

use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::malcolm;
use malcolm::replay::{RecordingHarness, ReplayHarness, ScenarioRecord};
use malcolm_core::bifurcation::BifurcationProfile;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("replay_demo: preparing deterministic scenario");

    let scenario = malcolm! {
        name: "replay-failure-run",
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
    let bytes = record.to_bytes()?;
    let decoded = ScenarioRecord::from_bytes(&bytes)?;
    let replay = ReplayHarness::new(decoded);

    let verified = replay.verify();
    assert!(verified);

    let replay_report = replay.replay();
    println!(
        "replay_demo: replayed={} events={}",
        replay_report.name,
        replay_report.events.len()
    );
    println!("replay_demo: integrity_verified={verified}");

    Ok(())
}
