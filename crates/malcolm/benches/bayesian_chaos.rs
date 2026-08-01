//! Benchmarks for the Phase 12 Bayesian chaos primitives:
//!
//! - `FailureGraph::marginals` — exact DAG path (`DPStep` / laplace-amp)
//! - `FailureGraph::blast_radius` — exact DP convolution (the Plinko
//!   landing bins)
//! - `infer_posterior` — log-space Bayes' rule over the failure graph
//!   (T40 root-cause posterior)
//!
//! Run with `cargo bench -p malcolm --bench bayesian_chaos`.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use malcolm_core::inference::{Clamp, FailureGraph};
use malcolm_core::posterior::{Observation, OriginPrior};

fn chain(n: usize) -> FailureGraph {
    let mut g = FailureGraph::new();
    for i in 0..(n.saturating_sub(1)) {
        g.add_edge(i.to_string(), (i + 1).to_string(), 1.0);
    }
    g
}

fn star(n: usize) -> FailureGraph {
    let mut g = FailureGraph::new();
    for i in 1..n {
        g.add_edge("0".to_owned(), i.to_string(), 1.0);
    }
    g
}

fn random_graph(n: usize, seed: u64) -> FailureGraph {
    use rand::RngExt;
    use rand::SeedableRng;
    use rand::rngs::SmallRng;
    let mut rng = SmallRng::seed_from_u64(seed);
    let mut g = FailureGraph::new();
    for i in 0..n {
        g.add_node(i.to_string());
    }
    for i in 0..n {
        for j in (i + 1)..n {
            if rng.random::<f64>() < 0.3 {
                g.add_edge(i.to_string(), j.to_string(), 1.0);
            }
        }
    }
    g
}

fn clamp_one(node: usize) -> Clamp {
    let mut c = Clamp::new();
    c.insert(node.to_string(), 1.0);
    c
}

fn observation_failed(nodes: &[usize]) -> Observation {
    let mut o = Observation::new();
    for n in nodes {
        o.add_failed(n.to_string());
    }
    o
}

fn origin_prior_uniform(n: usize) -> OriginPrior {
    let names: Vec<String> = (0..n).map(|i| i.to_string()).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    OriginPrior::uniform(refs)
}

fn origin_prior_weighted(n: usize) -> OriginPrior {
    let entries: Vec<(String, f64)> = (0..n).map(|i| (i.to_string(), 1.0)).collect();
    OriginPrior::weighted(entries)
}

fn bench_marginals_chain(c: &mut Criterion) {
    let g = chain(50);
    let clamp = clamp_one(0);
    c.bench_function("marginals_chain_50", |b| {
        b.iter(|| g.marginals(&clamp));
    });
}

fn bench_marginals_random_20(c: &mut Criterion) {
    let g = random_graph(20, 42);
    let clamp = clamp_one(0);
    c.bench_function("marginals_random_20", |b| {
        b.iter(|| g.marginals(&clamp));
    });
}

fn bench_marginals_random_50(c: &mut Criterion) {
    let g = random_graph(50, 42);
    let clamp = clamp_one(0);
    c.bench_function("marginals_random_50", |b| {
        b.iter(|| g.marginals(&clamp));
    });
}

fn bench_blast_radius_chain(c: &mut Criterion) {
    let g = chain(50);
    let clamp = clamp_one(0);
    c.bench_function("blast_radius_chain_50", |b| {
        b.iter(|| g.blast_radius(&clamp, 1_000_000, 42));
    });
}

fn bench_blast_radius_random_20(c: &mut Criterion) {
    let g = random_graph(20, 42);
    let clamp = clamp_one(0);
    c.bench_function("blast_radius_random_20", |b| {
        b.iter(|| g.blast_radius(&clamp, 1_000_000, 42));
    });
}

fn bench_infer_posterior_chain(c: &mut Criterion) {
    let g = chain(20);
    let prior = origin_prior_uniform(20);
    let obs = observation_failed(&[19]);
    c.bench_function("infer_posterior_chain_20", |b| {
        b.iter(|| malcolm_core::posterior::infer_posterior(&g, &prior, &obs));
    });
}

fn bench_infer_posterior_star(c: &mut Criterion) {
    let g = star(20);
    let prior = origin_prior_uniform(20);
    let obs = observation_failed(&[5, 10, 15]);
    c.bench_function("infer_posterior_star_20", |b| {
        b.iter(|| malcolm_core::posterior::infer_posterior(&g, &prior, &obs));
    });
}

fn bench_infer_posterior_random_15(c: &mut Criterion) {
    let g = random_graph(15, 42);
    let prior = origin_prior_weighted(15);
    let obs = observation_failed(&[7, 14]);
    c.bench_function("infer_posterior_random_15", |b| {
        b.iter(|| {
            for _ in 0..10 {
                let _ = malcolm_core::posterior::infer_posterior(&g, &prior, &obs);
            }
        });
    });
}

criterion_group!(
    benches,
    bench_marginals_chain,
    bench_marginals_random_20,
    bench_marginals_random_50,
    bench_blast_radius_chain,
    bench_blast_radius_random_20,
    bench_infer_posterior_chain,
    bench_infer_posterior_star,
    bench_infer_posterior_random_15,
);
criterion_main!(benches);
