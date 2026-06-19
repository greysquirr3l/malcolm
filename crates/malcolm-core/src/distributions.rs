//! Fault-distribution primitives: power-law, Pareto, log-normal.
//!
//! All three distributions implement [`DistributionSampler`] and use
//! inverse-CDF sampling so they are fully compatible with `no_std` environments.
//! [`LogNormal`] uses the Box-Muller transform (via [`libm`]) to generate
//! normal deviates without any allocations.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::distributions::{DistributionSampler, PowerLaw};
//! use rand::SeedableRng;
//! use rand::rngs::SmallRng;
//!
//! let dist = PowerLaw::default();
//! let mut rng = SmallRng::seed_from_u64(42);
//! let sample = dist.sample(&mut rng);
//! assert!(sample >= 1.0);
//! ```

use rand::Rng as _;
use rand::RngCore;

// ── Trait ────────────────────────────────────────────────────────────────────

/// A trait for drawing samples from a continuous probability distribution.
///
/// All implementations in this module are `no_std`-safe and require only a
/// mutable reference to any [`RngCore`] source.
pub trait DistributionSampler {
    /// Draw a single sample from this distribution.
    #[must_use]
    fn sample(&self, rng: &mut impl RngCore) -> f64;
}

// ── PowerLaw ─────────────────────────────────────────────────────────────────

/// Power-law distribution P(x) ~ x^(-alpha), sampled via inverse CDF.
///
/// Uses `x_min` = 1.0.  The inverse CDF transform gives:
/// `x = (1 - u)^(-1 / (alpha - 1))` for uniform `u ~ U(0, 1)`.
///
/// Requires `alpha > 1` for a proper distribution.
/// The default exponent is `alpha = 1.5`.
///
/// # Example
///
/// ```rust
/// use malcolm_core::distributions::{DistributionSampler, PowerLaw};
/// use rand::SeedableRng;
/// use rand::rngs::SmallRng;
///
/// let dist = PowerLaw { alpha: 2.0 };
/// let mut rng = SmallRng::seed_from_u64(0);
/// let x = dist.sample(&mut rng);
/// assert!(x >= 1.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PowerLaw {
    /// Exponent alpha (must be > 1 for a normalizable distribution).
    pub alpha: f64,
}

impl Default for PowerLaw {
    fn default() -> Self {
        Self { alpha: 1.5 }
    }
}

impl DistributionSampler for PowerLaw {
    /// Draw one sample: `x = (1 - u)^(-1 / (alpha - 1))`.
    fn sample(&self, rng: &mut impl RngCore) -> f64 {
        let u: f64 = rng.r#gen::<f64>();
        let exponent = -1.0 / (self.alpha - 1.0);
        libm::pow(1.0 - u, exponent)
    }
}

// ── Pareto ───────────────────────────────────────────────────────────────────

/// Pareto distribution with scale `x_min` and shape `alpha`.
///
/// The survival function is `P(X > x) = (x_min / x)^alpha`.
/// Sampling uses the inverse CDF: `x = x_min * (1 - u)^(-1 / alpha)`.
///
/// For a finite mean, `alpha > 1` is required.
///
/// # Example
///
/// ```rust
/// use malcolm_core::distributions::{DistributionSampler, Pareto};
/// use rand::SeedableRng;
/// use rand::rngs::SmallRng;
///
/// let dist = Pareto { alpha: 2.0, x_min: 1.0 };
/// let mut rng = SmallRng::seed_from_u64(0);
/// let x = dist.sample(&mut rng);
/// assert!(x >= 1.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pareto {
    /// Shape parameter (must be > 0; > 1 for finite mean).
    pub alpha: f64,
    /// Scale parameter — the minimum possible value.
    pub x_min: f64,
}

impl DistributionSampler for Pareto {
    /// Draw one sample: `x = x_min * (1 - u)^(-1 / alpha)`.
    fn sample(&self, rng: &mut impl RngCore) -> f64 {
        let u: f64 = rng.r#gen::<f64>();
        self.x_min * libm::pow(1.0 - u, -1.0 / self.alpha)
    }
}

// ── LogNormal ────────────────────────────────────────────────────────────────

/// Log-normal distribution for latency spike modelling.
///
/// If `X ~ LogNormal(mu, sigma)` then `ln(X) ~ N(mu, sigma^2)`.
/// Sampling: `x = exp(mu + sigma * Z)` where `Z` is drawn from a standard
/// normal distribution via the Box-Muller transform.
///
/// # Example
///
/// ```rust
/// use malcolm_core::distributions::{DistributionSampler, LogNormal};
/// use rand::SeedableRng;
/// use rand::rngs::SmallRng;
///
/// let dist = LogNormal { mu: 0.0, sigma: 1.0 };
/// let mut rng = SmallRng::seed_from_u64(0);
/// let x = dist.sample(&mut rng);
/// assert!(x > 0.0);
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogNormal {
    /// Mean of the underlying normal (log-space).
    pub mu: f64,
    /// Standard deviation of the underlying normal (log-space).
    pub sigma: f64,
}

impl DistributionSampler for LogNormal {
    /// Draw one sample: `x = exp(mu + sigma * Z)`, `Z ~ N(0,1)` via Box-Muller.
    fn sample(&self, rng: &mut impl RngCore) -> f64 {
        let z = box_muller(rng);
        libm::exp(self.mu + self.sigma * z)
    }
}

// ── Internal helpers ─────────────────────────────────────────────────────────

/// Standard normal sample via the Box-Muller transform.
///
/// `Z = sqrt(-2 ln U1) * cos(2π U2)` where `U1, U2 ~ U(0, 1)`.
/// `U1` is clamped to `[f64::EPSILON, 1)` to avoid `log(0)`.
fn box_muller(rng: &mut impl RngCore) -> f64 {
    let u1: f64 = rng.r#gen::<f64>().max(f64::EPSILON);
    let u2: f64 = rng.r#gen::<f64>();
    libm::sqrt(-2.0 * libm::log(u1)) * libm::cos(core::f64::consts::TAU * u2)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    use proptest::prelude::*;
    use rand::SeedableRng;
    use rand::rngs::SmallRng;

    const N: usize = 10_000;
    const N_F64: f64 = 10_000.0;

    proptest! {
        // alpha in (1.0, 5.0] is the natural range for a normalizable power-law.
        // x_min in (0.1, 100.0] covers the realistic fault-scale range.
        // mu and sigma cover the realistic log-normal parameter space.
        // `seed` is bounded so the test stays deterministic across runs.
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn power_law_samples_are_at_least_one(alpha in 1.01_f64..=5.0_f64, seed in 0_u64..=10_000) {
            let dist = PowerLaw { alpha };
            let mut rng = SmallRng::seed_from_u64(seed);
            for _ in 0..32 {
                let x = dist.sample(&mut rng);
                prop_assert!(x.is_finite(), "PowerLaw returned non-finite sample: {x}");
                prop_assert!(x >= 1.0, "PowerLaw sample {x} below x_min=1.0 for alpha={alpha}");
            }
        }

        #[test]
        fn pareto_samples_are_at_least_x_min(
            alpha in 0.1_f64..=5.0_f64,
            x_min in 0.001_f64..=100.0_f64,
            seed in 0_u64..=10_000,
        ) {
            let dist = Pareto { alpha, x_min };
            let mut rng = SmallRng::seed_from_u64(seed);
            for _ in 0..32 {
                let x = dist.sample(&mut rng);
                prop_assert!(x.is_finite(), "Pareto returned non-finite sample: {x}");
                prop_assert!(x >= x_min, "Pareto sample {x} below x_min={x_min}");
            }
        }

        #[test]
        fn lognormal_samples_are_positive(
            mu in -2.0_f64..=2.0_f64,
            sigma in 0.01_f64..=2.0_f64,
            seed in 0_u64..=10_000,
        ) {
            let dist = LogNormal { mu, sigma };
            let mut rng = SmallRng::seed_from_u64(seed);
            for _ in 0..32 {
                let x = dist.sample(&mut rng);
                prop_assert!(x.is_finite(), "LogNormal returned non-finite sample: {x}");
                prop_assert!(x > 0.0, "LogNormal sample {x} is non-positive");
            }
        }

        #[test]
        fn sampling_is_deterministic_for_same_seed(
            alpha in 1.1_f64..=4.0_f64,
            seed in 0_u64..=10_000,
        ) {
            let dist = PowerLaw { alpha };
            let mut a = SmallRng::seed_from_u64(seed);
            let mut b = SmallRng::seed_from_u64(seed);
            for _ in 0..8 {
                let x = dist.sample(&mut a);
                let y = dist.sample(&mut b);
                prop_assert!((x - y).abs() < f64::EPSILON, "{x} != {y}");
            }
        }
    }

    /// Hill estimator for the power-law exponent:
    /// `alpha_hat = 1 + n / sum(ln(x_i / x_min))`, `x_min` = 1.
    #[expect(
        clippy::cast_precision_loss,
        reason = "sample count = N = 10_000, lossless in f64"
    )]
    fn hill_estimate(samples: &[f64]) -> f64 {
        let n = samples.len() as f64;
        let sum_ln: f64 = samples.iter().map(|&x| libm::log(x)).sum();
        1.0 + n / sum_ln
    }

    #[test]
    fn power_law_samples_ge_one_and_alpha_within_15pct() {
        let dist = PowerLaw { alpha: 2.0 };
        let mut rng = SmallRng::seed_from_u64(42);
        let samples: Vec<f64> = (0..N).map(|_| dist.sample(&mut rng)).collect();

        assert!(samples.iter().all(|&x| x >= 1.0), "sample below x_min=1");

        let alpha_hat = hill_estimate(&samples);
        let rel_err = libm::fabs(alpha_hat - 2.0) / 2.0;
        assert!(
            rel_err < 0.15,
            "alpha estimate {alpha_hat:.4} is >15% from 2.0 (rel_err={rel_err:.4})"
        );
    }

    #[test]
    fn pareto_samples_ge_x_min_and_finite_mean() {
        let dist = Pareto {
            alpha: 2.0,
            x_min: 1.0,
        };
        let mut rng = SmallRng::seed_from_u64(42);
        let samples: Vec<f64> = (0..N).map(|_| dist.sample(&mut rng)).collect();

        assert!(
            samples.iter().all(|&x| x >= dist.x_min),
            "sample below x_min"
        );

        let mean: f64 = samples.iter().sum::<f64>() / N_F64;
        assert!(
            !mean.is_nan() && !mean.is_infinite(),
            "mean is not finite: {mean}"
        );
    }

    #[test]
    fn lognormal_mean_within_5pct_of_theoretical() {
        let dist = LogNormal {
            mu: 0.0,
            sigma: 1.0,
        };
        let mut rng = SmallRng::seed_from_u64(42);
        let samples: Vec<f64> = (0..N).map(|_| dist.sample(&mut rng)).collect();

        let mean: f64 = samples.iter().sum::<f64>() / N_F64;
        // Theoretical mean: exp(mu + sigma^2/2) = exp(0.5) ≈ 1.6487
        let expected = libm::exp(0.5);
        let rel_err = libm::fabs(mean - expected) / expected;
        assert!(
            rel_err < 0.05,
            "log-normal mean {mean:.4} is >5% from {expected:.4} (rel_err={rel_err:.4})"
        );
    }
}
