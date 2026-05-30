//! Post-mortem workflow example for Malcolm Lens.
//!
//! Default provider is Ollama. To use Anthropic instead:
//! `MALCOLM_LENS_PROVIDER=anthropic ANTHROPIC_API_KEY=... cargo run -p malcolm-lens --example lens_postmortem --no-default-features --features anthropic`

use std::error::Error;
use std::time::Duration;

use malcolm::fault::FaultContext;
use malcolm::faults::network::{LatencySpike, PacketLoss};
use malcolm::scenario::ChaosScenario;
use malcolm::core::bifurcation::BifurcationProfile;
use malcolm_lens::{Directive, LensAnalyzer, LensReport};

#[path = "support/ollama_guard.rs"]
mod ollama_guard;

fn main() -> Result<(), Box<dyn Error>> {
    if !provider_ready() {
        return Ok(());
    }

    let scenario = ChaosScenario::builder()
        .name("checkout-degradation")
        .seed(44)
        .add_fault(PacketLoss::builder().seed(2).intensity(0.72).build())
        .add_fault(LatencySpike::builder().seed(3).base_ms(180.0).intensity(0.78).build())
        .profile(BifurcationProfile::latency_cascade())
        .build();

    let mut ctx = FaultContext {
        seed: 44,
        timestamp_ms: 1_000,
        node_id: "checkout-api".to_owned(),
        profile: BifurcationProfile::latency_cascade(),
    };
    let report = scenario.run(&mut ctx);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;
    let analyzer = LensAnalyzer::builder().build()?;

    let lens_report = runtime.block_on(analyzer.analyze(&report, Directive::Narrative))?;

    println!("=== Incident Post-Mortem ===");
    if let LensReport::Narrative(narrative) = lens_report {
        println!("Summary: {}", narrative.summary);
        println!("Regime: {:?}", report.regime);
        println!("Key Events:");
        for event in narrative.key_events {
            println!("- {event}");
        }
        println!("Recommended Actions:");
        for action in narrative.recommended_actions {
            println!("- {action}");
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