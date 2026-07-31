//! Bayesian cascade network: analytic inference over a failure graph.
//!
//! Bayesian cascade inference: analytic per-node failure probabilities
//! and blast-radius distribution.
//!
//! Shows how to build a [`FailureGraph`] from a topology, then query
//! it for the per-node failure probability and the full blast-radius
//! distribution — the "Plinko landing bins" of the cascade.
//!
//! Run with:
//!
//! ```bash
//! cargo run --example cascade_inference
//! ```

use malcolm::topology::Topology;
use malcolm_core::inference::Clamp;

fn main() {
    println!("cascade_inference: analytic Bayesian cascade");

    // Diamond topology: A -> B -> D, A -> C -> D. Clamping A as the
    // single root cause: how does failure propagate?
    let topology = Topology::builder()
        .name("diamond")
        .add_edge("A", "B", 0.7)
        .add_edge("A", "C", 0.5)
        .add_edge("B", "D", 1.0)
        .add_edge("C", "D", 0.8)
        .build();

    let cascade = malcolm::topology::BayesianCascade::from_topology(&topology);
    let report = cascade.analyse(&clamp_a(), 50_000, 42);

    println!(
        "\nTopology: {} edges, exact marginals: {}",
        topology.edges().len(),
        report.is_exact()
    );

    println!("\nPer-node failure probabilities (A clamped to failed):");
    for (node, p) in &report.per_node {
        println!("  {node}: {p:.4}");
    }

    let br = &report.blast_radius;
    println!("\nBlast radius distribution:");
    println!("  expected failed nodes: {:.3}", br.expected());
    println!("  mode (most likely count): {}", br.mode());
    println!("  exact: {}", br.is_exact());
    for k in 0..=report.per_node.len() {
        println!("  P(exactly {k} failed) = {:.4}", br.p_exactly(k));
    }

    // Show how the marginals change under a different root-cause
    // hypothesis (B instead of A).
    println!("\nWhat if B is the root cause instead?");
    let cascade_b = malcolm::topology::BayesianCascade::from_topology(&topology);
    let report_b = cascade_b.analyse(&clamp_b(), 50_000, 42);
    for (node, p) in &report_b.per_node {
        println!("  {node}: {p:.4}");
    }
}

fn clamp_a() -> Clamp {
    let mut c = Clamp::new();
    c.insert("A".to_owned(), 1.0);
    c
}

fn clamp_b() -> Clamp {
    let mut c = Clamp::new();
    c.insert("B".to_owned(), 1.0);
    c
}
