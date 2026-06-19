//! Benchmarks for the hot math primitives in `malcolm-core`.
//!
//! Run with `cargo bench -p malcolm --bench core_math`.
#![allow(missing_docs)]

use criterion::{Criterion, criterion_group, criterion_main};
use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::distributions::{DistributionSampler, LogNormal, Pareto, PowerLaw};
use malcolm_core::lyapunov::{LyapunovScorer, SensitivityMap};
use malcolm_core::noise::{BrownNoise, PinkNoise, ScaledNoise};
use rand::SeedableRng;
use rand::rngs::SmallRng;

fn bench_power_law(c: &mut Criterion) {
    let dist = PowerLaw { alpha: 2.0 };
    c.bench_function("power_law_sample", |b| {
        b.iter_batched(
            || SmallRng::seed_from_u64(42),
            |mut rng| {
                let mut acc = 0.0_f64;
                for _ in 0..1_000 {
                    acc += dist.sample(&mut rng);
                }
                acc
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_pareto(c: &mut Criterion) {
    let dist = Pareto {
        alpha: 2.0,
        x_min: 1.0,
    };
    c.bench_function("pareto_sample", |b| {
        b.iter_batched(
            || SmallRng::seed_from_u64(42),
            |mut rng| {
                let mut acc = 0.0_f64;
                for _ in 0..1_000 {
                    acc += dist.sample(&mut rng);
                }
                acc
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_lognormal(c: &mut Criterion) {
    let dist = LogNormal {
        mu: 0.0,
        sigma: 1.0,
    };
    c.bench_function("lognormal_sample", |b| {
        b.iter_batched(
            || SmallRng::seed_from_u64(42),
            |mut rng| {
                let mut acc = 0.0_f64;
                for _ in 0..1_000 {
                    acc += dist.sample(&mut rng);
                }
                acc
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_pink_noise(c: &mut Criterion) {
    c.bench_function("pink_noise_1024_samples", |b| {
        b.iter(|| {
            let samples: alloc::vec::Vec<f64> = PinkNoise::new(42).take(1_024).collect();
            samples
        });
    });
}

fn bench_brown_noise(c: &mut Criterion) {
    c.bench_function("brown_noise_1024_samples", |b| {
        b.iter(|| {
            let samples: alloc::vec::Vec<f64> =
                BrownNoise::with_range(42, 0.0, 1.0).take(1_024).collect();
            samples
        });
    });
}

fn bench_scaled_noise(c: &mut Criterion) {
    c.bench_function("scaled_noise_1024_samples", |b| {
        b.iter(|| {
            let pink = PinkNoise::new(42);
            let samples: alloc::vec::Vec<f64> =
                ScaledNoise::new(pink, 10.0, 200.0).take(1_024).collect();
            samples
        });
    });
}

fn bench_lyapunov_compute(c: &mut Criterion) {
    c.bench_function("lyapunov_compute_1000_iter", |b| {
        b.iter(|| LyapunovScorer::compute(3.9, 1_000));
    });
}

fn bench_lyapunov_compute_chaotic(c: &mut Criterion) {
    c.bench_function("lyapunov_compute_5000_iter", |b| {
        b.iter(|| LyapunovScorer::compute(4.0, 5_000));
    });
}

fn bench_sensitivity_map(c: &mut Criterion) {
    let map = SensitivityMap::new(1.0, 4.0, 100);
    c.bench_function("sensitivity_map_100_steps", |b| {
        b.iter(|| map.compute(2_000));
    });
}

fn bench_bifurcation_classify(c: &mut Criterion) {
    let profile = BifurcationProfile::network_partition();
    c.bench_function("bifurcation_classify_10k", |b| {
        b.iter(|| {
            let mut acc = 0_u32;
            for i in 0..10_000 {
                let intensity = f64::from(i) / 10_000.0;
                if matches!(
                    malcolm_core::bifurcation::classify(intensity, &profile),
                    malcolm_core::bifurcation::Regime::Chaotic
                ) {
                    acc += 1;
                }
            }
            acc
        });
    });
}

extern crate alloc;

criterion_group!(
    benches,
    bench_power_law,
    bench_pareto,
    bench_lognormal,
    bench_pink_noise,
    bench_brown_noise,
    bench_scaled_noise,
    bench_lyapunov_compute,
    bench_lyapunov_compute_chaotic,
    bench_sensitivity_map,
    bench_bifurcation_classify,
);
criterion_main!(benches);
