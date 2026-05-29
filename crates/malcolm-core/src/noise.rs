//! Correlated noise generators: pink (1/f) and brown noise for realistic jitter.
//!
//! Three noise types are provided:
//!
//! - [`PinkNoise`]: 1/f noise via the Voss-McCartney algorithm.
//! - [`BrownNoise`]: Brownian (1/f²) noise via integrated white noise steps.
//! - [`ScaledNoise`]: adapter that maps any `[-1, 1]` iterator to a
//!   user-supplied millisecond range.
//!
//! All types are `no_std`-safe and implement [`Iterator`] for ergonomic use in
//! lazy pipelines.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::noise::{PinkNoise, BrownNoise, ScaledNoise};
//!
//! // Ten correlated pink-noise samples.
//! let pink: Vec<f64> = PinkNoise::new(42).take(10).collect();
//! assert_eq!(pink.len(), 10);
//!
//! // Brown noise scaled to [10 ms, 100 ms].
//! let brown = BrownNoise::with_range(42, -1.0, 1.0);
//! let ms: Vec<f64> = ScaledNoise::new(brown, 10.0, 100.0).take(5).collect();
//! assert!(ms.iter().all(|&v| v >= 10.0 && v <= 100.0));
//! ```

extern crate alloc;

use rand::Rng as _;
use rand::SeedableRng as _;
use rand::rngs::SmallRng;

/// Number of octave registers used by the Voss-McCartney algorithm.
const OCTAVES: usize = 8;
const OCTAVES_F64: f64 = 8.0;

// ── PinkNoise ─────────────────────────────────────────────────────────────────

/// Pink-noise generator producing 1/f spectral density.
///
/// Implements the **Voss-McCartney** algorithm: eight octave registers are
/// maintained, each updated at half the rate of the previous one.  On every
/// step the register indexed by the trailing-zero count of an internal counter
/// is refreshed with a new white-noise value.  The output — the mean of all
/// registers — accumulates long-range correlation characteristic of 1/f noise.
///
/// All outputs lie in the open interval `(-1.0, 1.0)`.
///
/// # Example
///
/// ```rust
/// use malcolm_core::noise::PinkNoise;
///
/// let samples: Vec<f64> = PinkNoise::new(42).take(100).collect();
/// assert_eq!(samples.len(), 100);
/// assert!(samples.iter().all(|&v| v > -1.0 && v < 1.0));
/// ```
pub struct PinkNoise {
    /// White-noise register for each octave band.
    rows: [f64; OCTAVES],
    /// Running sum of all registers (avoids re-summing on every step).
    running_sum: f64,
    /// Internal step counter; the trailing-zero count selects the register to update.
    counter: u32,
    /// Owned RNG enabling use as a standalone iterator.
    rng: SmallRng,
}

impl PinkNoise {
    /// Create a new [`PinkNoise`] generator seeded with `seed`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::noise::PinkNoise;
    ///
    /// let samples: Vec<f64> = PinkNoise::new(0xdead_beef).take(3).collect();
    /// assert!(samples.iter().all(|&v| v > -1.0 && v < 1.0));
    /// ```
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            rows: [0.0_f64; OCTAVES],
            running_sum: 0.0,
            counter: 0,
            rng: SmallRng::seed_from_u64(seed),
        }
    }
}

impl Iterator for PinkNoise {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        self.counter = self.counter.wrapping_add(1);
        // Trailing zeros select which octave register to refresh.
        // Modulo OCTAVES keeps the index in-bounds for get_mut.
        let idx = (self.counter.trailing_zeros() as usize) % OCTAVES;

        // New white-noise value in (-1, 1).
        let new_val: f64 = self.rng.r#gen::<f64>() * 2.0 - 1.0;

        // Update register and maintain running sum.
        if let Some(slot) = self.rows.get_mut(idx) {
            self.running_sum -= *slot;
            *slot = new_val;
            self.running_sum += new_val;
        }

        Some(self.running_sum / OCTAVES_F64)
    }
}

// ── BrownNoise ────────────────────────────────────────────────────────────────

/// Brown-noise generator producing 1/f² (Brownian) spectral density.
///
/// Each sample is computed by adding a small Gaussian-approximated step
/// (sum of two uniform random variables minus one) to the current value,
/// then clamping the result to the configured `[min, max]` range.  This
/// integration of white noise produces the strong lag-1 autocorrelation
/// characteristic of Brownian motion.
///
/// # Example
///
/// ```rust
/// use malcolm_core::noise::BrownNoise;
///
/// let samples: Vec<f64> =
///     BrownNoise::with_range(42, -1.0, 1.0).take(100).collect();
/// assert!(samples.iter().all(|&v| v >= -1.0 && v <= 1.0));
/// ```
pub struct BrownNoise {
    /// Current accumulated value.
    value: f64,
    /// Owned RNG for iterator usage.
    rng: SmallRng,
    /// Lower bound of the clamping range.
    min: f64,
    /// Upper bound of the clamping range.
    max: f64,
}

impl BrownNoise {
    /// Create a new [`BrownNoise`] generator with the default range `[-1.0, 1.0]`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::noise::BrownNoise;
    ///
    /// let v = BrownNoise::new(42).next().unwrap();
    /// assert!(v >= -1.0 && v <= 1.0);
    /// ```
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self::with_range(seed, -1.0, 1.0)
    }

    /// Create a [`BrownNoise`] generator whose output is clamped to `[min, max]`.
    ///
    /// The generator starts at the midpoint `(min + max) / 2`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::noise::BrownNoise;
    ///
    /// let samples: Vec<f64> =
    ///     BrownNoise::with_range(7, 0.0, 500.0).take(50).collect();
    /// assert!(samples.iter().all(|&v| v >= 0.0 && v <= 500.0));
    /// ```
    #[must_use]
    pub fn with_range(seed: u64, min: f64, max: f64) -> Self {
        Self {
            value: f64::midpoint(min, max),
            rng: SmallRng::seed_from_u64(seed),
            min,
            max,
        }
    }
}

impl Iterator for BrownNoise {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        // Triangular-distributed step: sum of two uniforms minus one, range [-1, 1].
        // Scaled to 5 % of the total range for strong autocorrelation.
        let step_raw = self.rng.r#gen::<f64>() + self.rng.r#gen::<f64>() - 1.0;
        let step = step_raw * (self.max - self.min) * 0.05;
        self.value += step;
        // Clamp to [min, max] to prevent unbounded drift.
        self.value = self.value.max(self.min).min(self.max);
        Some(self.value)
    }
}

// ── ScaledNoise ───────────────────────────────────────────────────────────────

/// Adapter that linearly maps any `[-1, 1]` noise iterator to `[min_ms, max_ms]`.
///
/// Useful for converting dimensionless noise into concrete timing jitter values
/// (e.g. milliseconds).  The inner iterator is consumed lazily.
///
/// # Example
///
/// ```rust
/// use malcolm_core::noise::{PinkNoise, ScaledNoise};
///
/// let pink = PinkNoise::new(42);
/// let jitter_ms: Vec<f64> =
///     ScaledNoise::new(pink, 10.0, 200.0).take(20).collect();
/// assert!(jitter_ms.iter().all(|&v| v >= 10.0 && v <= 200.0));
/// ```
pub struct ScaledNoise<I: Iterator<Item = f64>> {
    /// Inner noise source whose values are expected in `[-1.0, 1.0]`.
    inner: I,
    /// Output lower bound.
    min_ms: f64,
    /// Output upper bound.
    max_ms: f64,
}

impl<I: Iterator<Item = f64>> ScaledNoise<I> {
    /// Wrap `inner` and map its `[-1, 1]` output to `[min_ms, max_ms]`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm_core::noise::{BrownNoise, ScaledNoise};
    ///
    /// let samples: Vec<f64> =
    ///     ScaledNoise::new(BrownNoise::new(1), 5.0, 50.0).take(3).collect();
    /// assert!(samples.iter().all(|&v| v >= 5.0 && v <= 50.0));
    /// ```
    #[must_use]
    pub const fn new(inner: I, min_ms: f64, max_ms: f64) -> Self {
        Self {
            inner,
            min_ms,
            max_ms,
        }
    }
}

impl<I: Iterator<Item = f64>> Iterator for ScaledNoise<I> {
    type Item = f64;

    fn next(&mut self) -> Option<f64> {
        self.inner.next().map(|v| {
            // Map [-1, 1] → [0, 1] → [min_ms, max_ms], then clamp for safety.
            let t = f64::midpoint(v, 1.0);
            let scaled = self.min_ms + t * (self.max_ms - self.min_ms);
            scaled.max(self.min_ms).min(self.max_ms)
        })
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// Compute lag-1 (Pearson) autocorrelation of `samples`.
    #[expect(clippy::cast_precision_loss, reason = "sample counts fit exactly in f64 mantissa")]
    fn lag1_autocorr(samples: &[f64]) -> f64 {
        let n = samples.len() as f64;
        let mean = samples.iter().sum::<f64>() / n;
        let centered: Vec<f64> = samples.iter().map(|&x| x - mean).collect();
        // Numerator: sum of c[i] * c[i+1]
        let num: f64 = centered
            .iter()
            .zip(centered.iter().skip(1))
            .map(|(a, b)| a * b)
            .sum();
        // Denominator: sum of c[i]^2
        let den: f64 = centered.iter().map(|x| x * x).sum();
        if den == 0.0 { 0.0 } else { num / den }
    }

    #[test]
    fn pink_noise_determinism() {
        let a: Vec<f64> = PinkNoise::new(42).take(100).collect();
        let b: Vec<f64> = PinkNoise::new(42).take(100).collect();
        assert_eq!(a, b, "PinkNoise with same seed must be deterministic");
    }

    #[test]
    fn pink_noise_lag1_autocorrelation_positive() {
        let samples: Vec<f64> = PinkNoise::new(42).take(1000).collect();
        let r = lag1_autocorr(&samples);
        assert!(
            r > 0.3,
            "Pink noise lag-1 autocorrelation {r:.4} should be > 0.3 (white noise ≈ 0)"
        );
    }

    #[test]
    fn brown_noise_stays_within_range() {
        let samples: Vec<f64> = BrownNoise::with_range(42, -1.0, 1.0).take(10_000).collect();
        assert!(
            samples.iter().all(|&v| (-1.0_f64..=1.0).contains(&v)),
            "BrownNoise must stay within [-1.0, 1.0]"
        );
    }

    #[test]
    fn brown_noise_lag1_autocorrelation_strong() {
        let samples: Vec<f64> = BrownNoise::with_range(42, -1.0, 1.0).take(1000).collect();
        let r = lag1_autocorr(&samples);
        assert!(
            r > 0.8,
            "Brown noise lag-1 autocorrelation {r:.4} should be > 0.8 (strong persistence)"
        );
    }

    #[test]
    fn scaled_noise_stays_within_bounds() {
        let pink = PinkNoise::new(99);
        let samples: Vec<f64> = ScaledNoise::new(pink, 10.0, 200.0).take(500).collect();
        assert!(
            samples.iter().all(|&v| (10.0_f64..=200.0).contains(&v)),
            "ScaledNoise must map all outputs into [10.0, 200.0]"
        );
    }
}
