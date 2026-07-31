//! Bayesian-optimized adaptive fault search.
//!
//! Demonstrates the search API: define what "fragile" means (an
//! [`Objective`]) plus a search space, then ask the optimizer to find
//! the configuration that maximises fragility. The backend is the
//! EGO loop with a Kriging surrogate and Expected-Improvement
//! infill.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example bayesopt_search --features bayesopt
//! ```

#![cfg_attr(not(feature = "bayesopt"), allow(dead_code, unused_imports))]

use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::scenario::ChaosScenario;
use malcolm::topology::Topology;
use malcolm_core::bifurcation::BifurcationProfile;

#[cfg(feature = "bayesopt")]
use malcolm::search::{Dimension, FaultConfig, Objective, SearchConfig, SearchSpace, bayes_search};

#[cfg(feature = "bayesopt")]
fn main() {
    println!("bayesopt_search: Bayesian-optimized adaptive fault search");

    // Search a 1-D space: the fault intensity in [0.0, 1.0]. The
    // objective is the number of `Injected` events the chaos
    // scenario produces — higher intensity, more visible cascade,
    // more "fragile" surface.
    let space = SearchSpace::new(vec![Dimension::Continuous {
        lo: 0.0,
        hi: 1.0,
        name: "intensity".to_owned(),
    }]);

    let topology = Topology::builder()
        .name("diamond")
        .add_edge("A", "B", 0.7)
        .add_edge("A", "C", 0.5)
        .add_edge("B", "D", 1.0)
        .add_edge("C", "D", 0.8)
        .build();

    let objective = FragilityObjective { topology };

    let config = SearchConfig {
        seed: 42,
        max_iters: 12,
        n_doe: 3,
        single_threaded: true,
    };

    let result = match bayes_search(&space, &objective, &config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("search failed: {e}");
            std::process::exit(1);
        }
    };

    println!("\nSearch complete:");
    println!("  evaluations: {}", result.evaluations);
    println!(
        "  best intensity: {:.4}",
        result.best_config.params.first().copied().unwrap_or(0.0)
    );
    println!("  best score:     {:.4}", result.best_score);
    println!("\nTrace (iteration, intensity, score):");
    for entry in &result.trace {
        let p = entry.config.params.first().copied().unwrap_or(0.0);
        println!(
            "  {:>3}  intensity = {:.4}  score = {:.4}",
            entry.iteration, p, entry.score
        );
    }
}

#[cfg(feature = "bayesopt")]
#[derive(Clone)]
struct FragilityObjective {
    topology: Topology,
}

#[cfg(feature = "bayesopt")]
impl Objective for FragilityObjective {
    fn evaluate(&self, cfg: &FaultConfig, seed: u64) -> f64 {
        let intensity = cfg.params.first().copied().unwrap_or(0.0);
        let scenario = ChaosScenario::builder()
            .name("fragility")
            .seed(seed)
            .profile(BifurcationProfile::network_partition())
            .add_fault(
                PacketLoss::builder()
                    .seed(seed)
                    .intensity(intensity)
                    .build(),
            )
            .topology(self.topology.clone())
            .build();
        let mut ctx = FaultContext {
            seed,
            timestamp_ms: 0,
            node_id: "A".to_owned(),
            profile: BifurcationProfile::network_partition(),
        };
        let report = scenario.run(&mut ctx);
        // Score = number of injected events (proxy for cascade
        // reach). Higher = more fragile. The lossy cast is sound
        // here: the count is bounded by the scenario's fault
        // count, which is tiny.
        f64::from(
            u32::try_from(report.events.iter().filter(|e| e.intensity > 0.0).count())
                .unwrap_or(u32::MAX),
        )
    }
}

#[cfg(not(feature = "bayesopt"))]
fn main() {
    eprintln!("This example requires the `bayesopt` feature:");
    eprintln!("  cargo run --example bayesopt_search --features bayesopt");
    std::process::exit(1);
}
