//! Replay divergence investigation workflow for Malcolm Lens.
//!
//! Default provider is Ollama. To use Anthropic instead:
//! `MALCOLM_LENS_PROVIDER=anthropic ANTHROPIC_API_KEY=... cargo run -p malcolm-lens --example lens_divergence --no-default-features --features anthropic`

use std::error::Error;
use std::time::Duration;

use malcolm::core::bifurcation::BifurcationProfile;
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::replay::{RecordingHarness, ReplayHarness, ScenarioRecord};
use malcolm::scenario::ChaosScenario;
use malcolm_lens::{Directive, LensAnalyzer, LensReport};

#[path = "support/ollama_guard.rs"]
mod ollama_guard;

fn main() -> Result<(), Box<dyn Error>> {
    if !provider_ready() {
        return Ok(());
    }

    let scenario = ChaosScenario::builder()
        .name("replica-state-drift")
        .seed(404)
        .add_fault(PacketLoss::builder().seed(5).intensity(0.81).build())
        .profile(BifurcationProfile::network_partition())
        .build();

    let mut ctx = FaultContext {
        seed: 404,
        timestamp_ms: 4_000,
        node_id: "replica-2".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };

    let pristine = RecordingHarness::new(&scenario).record(&mut ctx);
    let tampered = tamper_record(&pristine)?;
    let replay = ReplayHarness::new(tampered);

    if replay.verify() {
        println!("Replay still verifies; divergence trigger did not reproduce.");
        return Ok(());
    }

    let replay_report = replay.replay();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let analyzer = LensAnalyzer::builder().build()?;

    let lens_report =
        runtime.block_on(analyzer.analyze(&replay_report, Directive::ExplainDivergence))?;

    println!("=== Replay Divergence Analysis ===");
    if let LensReport::Divergence(divergence) = lens_report {
        println!("Divergence point: {}", divergence.divergence_point);
        println!("Likely cause: {}", divergence.likely_cause);
        println!("Suggested fix: {}", divergence.suggested_fix);
    }

    Ok(())
}

fn tamper_record(record: &ScenarioRecord) -> Result<ScenarioRecord, Box<dyn Error>> {
    let mut bytes = record.to_bytes()?;
    if let Some(index) = bytes
        .iter()
        .position(|byte| byte.is_ascii_lowercase() || byte.is_ascii_uppercase())
        && let Some(slot) = bytes.get_mut(index)
    {
        let replacement = if *slot == b'a' { b'b' } else { b'a' };
        *slot = replacement;
    }

    let mut mutated = ScenarioRecord::from_bytes(&bytes)?;
    if let Some(first) = mutated.events.first_mut() {
        first.node_id.push_str("-drift");
    }
    Ok(mutated)
}

fn provider_ready() -> bool {
    let provider = ollama_guard::provider_from_env();
    if provider != "ollama" {
        return true;
    }

    let base_url = ollama_guard::current_base_url();
    if ollama_guard::ollama_reachable(&base_url, Duration::from_millis(300)) {
        return true;
    }

    println!(
        "Ollama is not reachable at {base_url}. Start Ollama, set OLLAMA_BASE_URL, or use MALCOLM_LENS_PROVIDER=anthropic."
    );
    false
}
