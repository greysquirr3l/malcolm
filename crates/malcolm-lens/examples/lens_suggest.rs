//! Follow-up scenario suggestion workflow for Malcolm Lens.
//!
//! Default provider is Ollama. To use Anthropic instead:
//! `MALCOLM_LENS_PROVIDER=anthropic ANTHROPIC_API_KEY=... cargo run -p malcolm-lens --example lens_suggest --no-default-features --features anthropic`

use std::error::Error;
use std::time::Duration;

use malcolm::core::bifurcation::BifurcationProfile;
use malcolm::fault::FaultContext;
use malcolm::faults::network::{LatencySpike, NetworkPartition, NoiseType, PacketLoss};
use malcolm::scenario::ChaosScenario;
use malcolm_lens::{Directive, LensAnalyzer, LensReport};

#[path = "support/ollama_guard.rs"]
mod ollama_guard;

fn main() -> Result<(), Box<dyn Error>> {
    if !provider_ready() {
        return Ok(());
    }

    let scenario = ChaosScenario::builder()
        .name("chaotic-quorum-instability")
        .seed(9001)
        .add_fault(
            NetworkPartition::builder()
                .seed(11)
                .alpha(1.2)
                .intensity(0.96)
                .build(),
        )
        .add_fault(
            PacketLoss::builder()
                .seed(12)
                .alpha(1.4)
                .x_min(1.0)
                .intensity(0.92)
                .build(),
        )
        .add_fault(
            LatencySpike::builder()
                .seed(13)
                .base_ms(320.0)
                .sigma(0.8)
                .noise(NoiseType::Brown)
                .intensity(0.94)
                .build(),
        )
        .profile(BifurcationProfile::latency_cascade())
        .build();

    let mut ctx = FaultContext {
        seed: 9001,
        timestamp_ms: 2_000,
        node_id: "consensus-leader".to_owned(),
        profile: BifurcationProfile::latency_cascade(),
    };
    let report = scenario.run(&mut ctx);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let analyzer = LensAnalyzer::builder().build()?;

    let lens_report = runtime.block_on(analyzer.analyze(&report, Directive::SuggestScenarios))?;

    println!("=== Adaptive Scenario Suggestions ===");
    println!("Observed regime: {:?}", report.regime);
    if let LensReport::Suggestions(suggestions) = lens_report {
        for suggestion in suggestions {
            println!("Scenario: {}", suggestion.name);
            println!("Rationale: {}", suggestion.rationale);
            println!("Fault hints: {}", suggestion.fault_hints.join(", "));
            println!();
        }
    }

    Ok(())
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
