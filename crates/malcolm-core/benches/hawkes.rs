//! Benchmarks for the Hawkes conditional-intensity process (T42).
//!
//! - `intensity_at` — direct O(|history|) summation form
//! - `intensity_incremental` — O(1) free-decay form
//! - `apply_event` — O(1) decay + new event
//! - `simulate` — Ogata thinning algorithm
//!
//! Run with `cargo bench -p malcolm-core --bench hawkes`.
#![allow(missing_docs, clippy::expect_used, clippy::unwrap_used)]

use criterion::{Criterion, criterion_group, criterion_main};
use malcolm_core::hawkes::HawkesProcess;

fn bench_intensity_at_small(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.5, 0.4, 1.0) else {
        return;
    };
    let history: Vec<f64> = (0..10).map(|i| f64::from(i) * 0.5).collect();
    c.bench_function("intensity_at_10_events", |b| {
        b.iter(|| p.intensity_at(100.0, &history));
    });
}

fn bench_intensity_at_large(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.5, 0.4, 1.0) else {
        return;
    };
    let history: Vec<f64> = (0..1_000).map(|i| f64::from(i) * 0.1).collect();
    c.bench_function("intensity_at_1000_events", |b| {
        b.iter(|| p.intensity_at(500.0, &history));
    });
}

fn bench_intensity_incremental(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.5, 0.4, 1.0) else {
        return;
    };
    c.bench_function("intensity_incremental", |b| {
        b.iter(|| p.intensity_incremental(1.0, 0.5));
    });
}

fn bench_apply_event(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.5, 0.4, 1.0) else {
        return;
    };
    c.bench_function("apply_event", |b| {
        b.iter(|| p.apply_event(1.0, 0.5));
    });
}

fn bench_simulate_short(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.1, 0.05, 1.0) else {
        return;
    };
    c.bench_function("simulate_horizon_1000", |b| {
        b.iter(|| p.simulate(1_000.0, 42, 1_000));
    });
}

fn bench_simulate_long(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.1, 0.05, 1.0) else {
        return;
    };
    c.bench_function("simulate_horizon_10000", |b| {
        b.iter(|| p.simulate(10_000.0, 42, 10_000));
    });
}

fn bench_branching_ratio(c: &mut Criterion) {
    let Ok(p) = HawkesProcess::new(0.1, 0.4, 1.0) else {
        return;
    };
    c.bench_function("branching_ratio", |b| {
        b.iter(|| p.branching_ratio());
    });
}

criterion_group!(
    benches,
    bench_intensity_at_small,
    bench_intensity_at_large,
    bench_intensity_incremental,
    bench_apply_event,
    bench_simulate_short,
    bench_simulate_long,
    bench_branching_ratio,
);
criterion_main!(benches);
