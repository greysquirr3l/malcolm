//! Bayesian root-cause posterior: run the cascade *backwards*.
//!
//! Root-cause analysis: given a chain `A -> B -> C`, inject a single
//! fault at `A` and let `run()` produce a `ScenarioReport`. Then ask
//! the posterior: which injected fault *most likely* explains the
//! observed failure pattern?
//!
//! Run with:
//!
//! ```bash
//! cargo run --example root_cause_analysis
//! ```

use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::rootcause::{RootCauseConfig, root_cause_from_scenario};
use malcolm::scenario::ChaosScenario;
use malcolm::topology::Topology;
use malcolm_core::bifurcation::BifurcationProfile;

fn main() {
    println!("root_cause_analysis: Bayesian root-cause posterior");

    // Chain A -> B -> C. Inject a packet-loss fault at A.
    let topology = Topology::builder()
        .name("chain-abc")
        .add_edge("A", "B", 0.5)
        .add_edge("B", "C", 0.5)
        .build();

    let scenario = ChaosScenario::builder()
        .name("root-cause-example")
        .seed(42)
        .profile(BifurcationProfile::network_partition())
        .add_fault(PacketLoss::builder().seed(42).intensity(0.9).build())
        .topology(topology.clone())
        .build();

    let mut ctx = FaultContext {
        seed: 42,
        timestamp_ms: 0,
        node_id: "A".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };
    let report = scenario.run(&mut ctx);

    println!("\nScenario 'root-cause-example' ran:");
    println!("  events recorded: {}", report.events.len());
    for event in &report.events {
        println!(
            "    {} on node {} (intensity {:.2})",
            event.fault_type, event.node_id, event.intensity
        );
    }

    // Now run the posterior: which candidate origin is the most likely
    // cause of the observed failure pattern?
    let config = RootCauseConfig::new();
    let rcr = root_cause_from_scenario(&report, &topology, &config);

    println!("\nRoot-cause posterior:");
    println!("  candidate count: {}", rcr.candidate_count);
    println!("  graph nodes:     {}", rcr.graph_node_count);
    println!("  failed (observed):     {:?}", rcr.observation.failed);
    println!("  healthy (observed):    {:?}", rcr.observation.healthy);
    println!(
        "  unobserved (marginalised): {}",
        rcr.observation.unobserved_count
    );

    for cand in &rcr.posterior.candidates {
        println!(
            "  {:>4}  posterior = {:.4}  log_likelihood = {:+.3}  log_prior = {:+.3}",
            cand.origin, cand.posterior, cand.log_likelihood, cand.log_prior
        );
    }
    println!("  entropy: {:.3} nats", rcr.posterior.entropy);

    // Augment with an external observation: we know B is healthy (a
    // health check passed). The posterior should re-weight.
    println!("\nWith external observation: B is healthy");
    let mut cfg = RootCauseConfig::new();
    cfg.add_healthy("B");
    let rcr_b = root_cause_from_scenario(&report, &topology, &cfg);
    for cand in &rcr_b.posterior.candidates {
        println!(
            "  {:>4}  posterior = {:.4}  log_likelihood = {:+.3}",
            cand.origin, cand.posterior, cand.log_likelihood
        );
    }
}
