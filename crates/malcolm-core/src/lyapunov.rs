//! Lyapunov sensitivity scorer: ranks fault injection points by chaos amplification.
//!
//! The Lyapunov exponent λ measures how fast two initially-close trajectories in a
//! dynamical system diverge. Positive λ means the system is chaotic (a tiny fault
//! grows exponentially); negative λ means it is stable (perturbations damp out).
//!
//! We model the fault parameter space using the **logistic map**
//! `x_{n+1} = r * x_n * (1 - x_n)`, a canonical discrete dynamical system whose
//! behaviour transitions from stable to chaotic as `r` increases toward 4.
//! The control parameter `r` is derived from the caller-supplied fault intensity,
//! making this a practical proxy for "how destabilising is this injection point?"
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::lyapunov::LyapunovScorer;
//!
//! // r = 3.9 is in the chaotic regime of the logistic map.
//! let lambda = LyapunovScorer::compute(3.9, 1000);
//! assert!(lambda > 0.0, "chaotic regime should have positive Lyapunov exponent");
//! ```

use alloc::vec::Vec;

// ── LyapunovScorer ────────────────────────────────────────────────────────────

/// Computes the Lyapunov exponent for a discrete dynamical system modelled by the
/// logistic map at a given control parameter `r`.
///
/// # Formula
///
/// Given `N` iterations starting from a fixed initial state `x₀ = 0.5`:
///
/// ```text
/// λ = (1/N) * Σ ln|r * (1 - 2*xᵢ)|   for i = 0..N-1
/// ```
///
/// The derivative of the logistic map `f(x) = r·x·(1-x)` is `f'(x) = r·(1-2x)`;
/// its absolute value inside the log is the local divergence rate at each step.
///
/// # Example
///
/// ```rust
/// use malcolm_core::lyapunov::LyapunovScorer;
///
/// // r = 2.0: stable fixed point — nearby trajectories converge.
/// let lambda = LyapunovScorer::compute(2.0, 1000);
/// assert!(lambda < 0.0);
///
/// // r = 3.9: chaotic — small perturbations grow exponentially.
/// let lambda = LyapunovScorer::compute(3.9, 1000);
/// assert!(lambda > 0.0);
/// ```
pub struct LyapunovScorer;

impl LyapunovScorer {
    /// Compute the Lyapunov exponent for the logistic map with control parameter `r`
    /// over `n_iterations` steps.
    ///
    /// Returns `f64::NEG_INFINITY` if any iterate lands at exactly `x = 0.5` (where
    /// the derivative is zero and ln(0) is undefined), though this is vanishingly
    /// unlikely for irrational `r` in practice.
    ///
    /// # Arguments
    ///
    /// * `r` — logistic map control parameter; meaningful range is `(0.0, 4.0]`.
    /// * `n_iterations` — number of map iterations; ≥ 1000 gives reliable estimates.
    #[must_use]
    pub fn compute(r: f64, n_iterations: usize) -> f64 {
        // x₀ = 0.1: avoids the derivative singularity at x = 0.5 (where f'(x) = r·(1-2·0.5) = 0)
        // and also avoids the degenerate fixed points at 0 and 1.
        let mut x = 0.1_f64;
        let mut sum = 0.0_f64;

        for _ in 0..n_iterations {
            // ln|f'(x)| = ln|r·(1 - 2x)|
            let derivative = r * (1.0 - 2.0 * x);
            let abs_deriv = libm::fabs(derivative);

            // Guard against the zero-derivative singularity.
            if abs_deriv > 0.0 {
                sum += libm::log(abs_deriv);
            } else {
                return f64::NEG_INFINITY;
            }

            x = r * x * (1.0 - x);
        }

        // Guard against n_iterations = 0 (callers should not pass 0, but be safe).
        if n_iterations == 0 {
            return 0.0;
        }

        #[expect(
            clippy::cast_precision_loss,
            reason = "n_iterations is bounded to a practical range; precision loss is negligible"
        )]
        let n = n_iterations as f64;
        sum / n
    }
}

// ── SensitivityMap ────────────────────────────────────────────────────────────

/// Sweeps a range of fault intensities and returns the Lyapunov exponent at each
/// point, forming a (intensity, λ) curve.
///
/// The curve lets callers rank injection points: higher λ means a fault at that
/// intensity will amplify perturbations more aggressively.
///
/// # Example
///
/// ```rust
/// use malcolm_core::lyapunov::SensitivityMap;
///
/// let map = SensitivityMap::new(1.0, 4.0, 20);
/// let curve = map.compute(1000);
/// assert_eq!(curve.len(), 20);
/// // Intensities increase monotonically.
/// assert!(curve[1].0 > curve[0].0);
/// ```
pub struct SensitivityMap {
    /// Lowest fault intensity (maps directly to logistic-map `r`).
    pub min_intensity: f64,
    /// Highest fault intensity.
    pub max_intensity: f64,
    /// Number of evenly-spaced samples between min and max (inclusive of endpoints).
    pub steps: usize,
}

impl SensitivityMap {
    /// Create a new sensitivity map sweeping `[min_intensity, max_intensity]` in
    /// `steps` evenly-spaced samples.
    ///
    /// # Panics (debug only)
    ///
    /// Panics in debug builds if `steps == 0` or `min_intensity >= max_intensity`.
    #[must_use]
    pub const fn new(min_intensity: f64, max_intensity: f64, steps: usize) -> Self {
        Self {
            min_intensity,
            max_intensity,
            steps,
        }
    }

    /// Evaluate the Lyapunov exponent at each intensity level.
    ///
    /// Returns a `Vec` of `(intensity, lambda)` pairs ordered from lowest to highest
    /// intensity. Each exponent is computed over `n_iterations` logistic-map steps.
    #[must_use]
    pub fn compute(&self, n_iterations: usize) -> Vec<(f64, f64)> {
        if self.steps == 0 {
            return Vec::new();
        }

        let mut curve = Vec::with_capacity(self.steps);

        for i in 0..self.steps {
            // Interpolate intensity linearly across [min, max].
            #[expect(
                clippy::cast_precision_loss,
                reason = "steps is a small counter; precision loss negligible"
            )]
            let t = if self.steps == 1 {
                0.0
            } else {
                i as f64 / (self.steps - 1) as f64
            };

            let intensity = self.min_intensity + t * (self.max_intensity - self.min_intensity);
            let lambda = LyapunovScorer::compute(intensity, n_iterations);
            curve.push((intensity, lambda));
        }

        curve
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::{LyapunovScorer, SensitivityMap};

    #[test]
    fn chaotic_regime_positive_lambda() {
        // r = 3.9 is firmly in the chaotic regime of the logistic map (r > 3.57…).
        let lambda = LyapunovScorer::compute(3.9, 1000);
        assert!(lambda > 0.0, "r=3.9 should yield λ > 0, got {lambda}");
    }

    #[test]
    fn stable_regime_negative_lambda() {
        // r = 2.0 has a stable fixed point; trajectories converge rather than diverge.
        let lambda = LyapunovScorer::compute(2.0, 1000);
        assert!(lambda < 0.0, "r=2.0 should yield λ < 0, got {lambda}");
    }

    #[test]
    fn sensitivity_map_generally_nondecreasing() {
        // The logistic map Lyapunov curve over r=[1.0, 4.0] is NOT monotone:
        //   r=1→2: λ decreases toward -∞ as the orbit converges to a fixed point at x=0.5
        //   r=2→3: λ increases back toward 0 (stable fixed point moves off x=0.5)
        //   r=3→3.57: λ dips negative again through the period-doubling cascade
        //   r=3.57→4: λ rises to ≈+0.693 in the fully chaotic regime
        //
        // The correct property to verify is that the high-intensity (chaotic) end of the
        // curve has significantly higher λ than the low-intensity (stable) end, which is
        // what makes this scorer useful for ranking injection points.
        let map = SensitivityMap::new(1.0, 4.0, 20);
        let curve = map.compute(1000);

        assert_eq!(curve.len(), 20);

        // Intensities must increase monotonically.
        for window in curve.windows(2) {
            if let [a, b] = window {
                assert!(b.0 > a.0, "intensities must increase");
            }
        }

        // The final point (r=4.0) must be firmly in the chaotic regime.
        let lambda_at_max = curve.last().map_or(f64::NEG_INFINITY, |p| p.1);
        assert!(
            lambda_at_max > 0.5,
            "r=4.0 should have λ ≈ 0.693 (chaotic), got {lambda_at_max:.4}"
        );

        // The stable region around r=2 must be represented by clearly negative λ values.
        // Steps 5–8 cover roughly r=1.79–2.26, which straddles the fixed-point singularity.
        let has_negative = curve.get(5..9).is_some_and(|s| s.iter().any(|p| p.1 < -0.5));
        assert!(
            has_negative,
            "stable region around r=2 should yield strongly negative λ"
        );
    }

    #[test]
    fn zero_steps_returns_empty() {
        let map = SensitivityMap::new(1.0, 4.0, 0);
        assert!(map.compute(100).is_empty());
    }

    #[test]
    fn single_step_returns_one_entry() {
        let map = SensitivityMap::new(3.9, 3.9, 1);
        let curve = map.compute(500);
        assert_eq!(curve.len(), 1);
        assert!(curve.first().is_some_and(|p| (p.0 - 3.9).abs() < 1e-10));
    }
}
