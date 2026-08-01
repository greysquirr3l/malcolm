//! Self-exciting point processes: Hawkes conditional intensity for clustered
//! fault arrivals.
//!
//! Where the [`crate::distributions`] module models **how big** a fault
//! magnitude is, this module models **when** faults arrive. Real outages are
//! clustered — one failure raises the probability of the next (retry storms,
//! thundering herds, cascading timeouts). The univariate Hawkes process with
//! exponential kernel captures that with three parameters:
//!
//! `λ(t) = μ + Σ_{tᵢ < t} α · exp(−β · (t − tᵢ))`
//!
//! - `μ` — background rate (events per unit time when nothing is happening)
//! - `α` — excitation amplitude: how much each past event pushes the rate up
//! - `β` — exponential decay: how fast the memory of an event fades
//!
//! The **branching ratio** `n = α / β` is the expected number of offspring
//! per parent event. The process is stationary iff `n < 1`; an explosive
//! (`n ≥ 1`) process is a valid model of a runaway incident but simulations
//! must cap the event count.
//!
//! # Example
//!
//! ```rust
//! use malcolm_core::hawkes::HawkesProcess;
//!
//! // Low background, strong self-excitation, moderate decay.
//! let p = HawkesProcess::new(0.1, 1.5, 2.0).unwrap();
//! assert_eq!(p.branching_ratio(), 0.75);
//! assert!(p.is_stationary());
//!
//! // Simulate 50 events on a long horizon, seeded for replay.
//! let arrivals = p.simulate(100.0, 42, 50);
//! assert!(arrivals.len() <= 50);
//! ```

use alloc::vec::Vec;

use libm::exp;
use libm::log;

use rand::RngExt as _;
use rand::SeedableRng as _;
use rand::rngs::SmallRng;

/// Univariate Hawkes process with exponential kernel.
///
/// Constructed via [`HawkesProcess::new`], which validates parameters. The
/// process can be queried ([`HawkesProcess::intensity_at`]) or simulated
/// ([`HawkesProcess::simulate`]) with Ogata's thinning algorithm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HawkesProcess {
    /// Background rate `μ`. Must be non-negative.
    mu: f64,
    /// Excitation amplitude `α`. Must be non-negative.
    alpha: f64,
    /// Exponential decay rate `β`. Must be positive.
    beta: f64,
}

/// Constructor error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HawkesError {
    /// `mu < 0`. The background rate must be non-negative.
    NegativeBackgroundRate,
    /// `alpha < 0`. The excitation amplitude must be non-negative.
    NegativeExcitation,
    /// `beta <= 0`. The decay rate must be strictly positive.
    NonPositiveDecay,
}

impl core::fmt::Display for HawkesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NegativeBackgroundRate => f.write_str("mu must be non-negative"),
            Self::NegativeExcitation => f.write_str("alpha must be non-negative"),
            Self::NonPositiveDecay => f.write_str("beta must be > 0"),
        }
    }
}

impl HawkesProcess {
    /// Construct a Hawkes process from `mu`, `alpha`, `beta`.
    ///
    /// # Errors
    ///
    /// Returns [`HawkesError`] if any parameter is out of its allowed range.
    pub const fn new(mu: f64, alpha: f64, beta: f64) -> Result<Self, HawkesError> {
        if mu < 0.0 {
            return Err(HawkesError::NegativeBackgroundRate);
        }
        if alpha < 0.0 {
            return Err(HawkesError::NegativeExcitation);
        }
        if beta <= 0.0 {
            return Err(HawkesError::NonPositiveDecay);
        }
        Ok(Self { mu, alpha, beta })
    }

    /// Background rate `μ`.
    #[must_use]
    pub const fn mu(&self) -> f64 {
        self.mu
    }

    /// Excitation amplitude `α`.
    #[must_use]
    pub const fn alpha(&self) -> f64 {
        self.alpha
    }

    /// Decay rate `β`.
    #[must_use]
    pub const fn beta(&self) -> f64 {
        self.beta
    }

    /// Branching ratio `n = α / β`.
    ///
    /// The expected number of offspring per parent event. Stationary iff
    /// `n < 1`. A process with `n ≥ 1` is explosive: it generates an
    /// unbounded expected number of events in finite time. Such a process
    /// is a valid model of a runaway incident, but simulations must cap
    /// the output.
    #[must_use]
    pub const fn branching_ratio(&self) -> f64 {
        self.alpha / self.beta
    }

    /// True if the process is stationary (`branching_ratio() < 1`).
    #[must_use]
    pub const fn is_stationary(&self) -> bool {
        self.branching_ratio() < 1.0
    }

    /// Theoretical long-run rate (events per unit time) for a stationary
    /// process: `μ / (1 − n)`.
    ///
    /// Returns `None` for an explosive process (the long-run rate diverges).
    #[must_use]
    pub const fn long_run_rate(&self) -> Option<f64> {
        if !self.is_stationary() {
            return None;
        }
        Some(self.mu / (1.0 - self.branching_ratio()))
    }

    /// Conditional intensity at time `t` given the arrival history
    /// `history`. Events in `history` must be sorted ascending.
    ///
    /// Computes `μ + Σ_{tᵢ < t} α · exp(−β · (t − tᵢ))` exactly. The
    /// direct form is O(|history|) per call; for the hot-path in
    /// simulation use [`HawkesProcess::intensity_incremental`] instead.
    #[must_use]
    pub fn intensity_at(&self, t: f64, history: &[f64]) -> f64 {
        let mut lambda = self.mu;
        for &ti in history {
            if ti < t {
                lambda += self.alpha * exp(-self.beta * (t - ti));
            }
        }
        lambda.max(0.0)
    }

    /// Incremental intensity update for one event step.
    ///
    /// `last_intensity` is `λ(t_last)` where `t_last` is the most-recent
    /// event time (or the start time if no events yet). `delta_t` is
    /// `t_new - t_last`. Returns `λ(t_new)`:
    ///
    /// `λ(t_new) = (λ(t_last) − μ) · exp(−β · delta_t) + μ`
    ///
    /// Equivalently: the *excitation part* decays by `exp(−βΔt)`, the
    /// background `μ` is unchanged. O(1) per call.
    ///
    /// # Why no `&self` mut borrow
    ///
    /// This signature lets the caller carry `lambda` through a simulation
    /// loop without borrowing the process; the function is fully pure on
    /// its arguments.
    ///
    /// **Note**: this is the *free-decay* form. To compute the intensity
    /// *after* a new event has just been recorded at `t_new`, add the
    /// new event's own contribution `α · exp(0) = α` to the result.
    /// [`HawkesProcess::simulate`] does exactly this.
    #[must_use]
    pub fn intensity_incremental(&self, last_intensity: f64, delta_t: f64) -> f64 {
        let excitation = last_intensity - self.mu;
        let decayed = excitation * exp(-self.beta * delta_t);
        (decayed + self.mu).max(0.0)
    }

    /// Apply an event: decay the running intensity to the new time and
    /// add the new event's own contribution `α`.
    ///
    /// `λ(t_new) = (λ(t_last) − μ) · exp(−β · (t_new − t_last)) + μ + α`
    ///
    /// O(1) per call. Use this in simulation loops instead of
    /// [`HawkesProcess::intensity_incremental`] to keep the running
    /// intensity correct after each event.
    #[must_use]
    pub fn apply_event(&self, last_intensity: f64, delta_t: f64) -> f64 {
        let decayed = self.intensity_incremental(last_intensity, delta_t);
        (decayed + self.alpha).max(0.0)
    }

    /// Simulate arrivals via Ogata's thinning algorithm.
    ///
    /// Returns the event times in ascending order on `[0, horizon]`. The
    /// output is bounded by `max_events` to prevent runaway processes
    /// from producing unbounded allocations. The simulation is
    /// deterministic for a fixed `seed`.
    ///
    /// # Algorithm
    ///
    /// Ogata (1981): at each step,
    /// 1. Draw `U₁, U₂` uniform on `[0, 1]`.
    /// 2. Set `Δt = −ln(U₁) / λ̄` where `λ̄` is an upper bound on the
    ///    intensity over the interval `[t, t + Δt]`. For an
    ///    exponential-kernel Hawkes, `λ̄ = λ(t)` is tight: the intensity
    ///    can only decay between events, never grow.
    /// 3. Accept the candidate iff `U₂ ≤ λ(t + Δt) / λ̄`. If rejected,
    ///    return to step 1 with `t` unchanged.
    /// 4. On accept: append `t + Δt`, advance, recompute intensity via
    ///    [`HawkesProcess::apply_event`].
    /// 5. Stop when `t + Δt > horizon` or `max_events` reached.
    #[must_use]
    pub fn simulate(&self, horizon: f64, seed: u64, max_events: usize) -> Vec<f64> {
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut events: Vec<f64> = Vec::new();
        if horizon <= 0.0 {
            return events;
        }
        // Intensity at t=0 with no events: just mu.
        let mut lambda = self.mu;
        let mut t = 0.0_f64;
        while events.len() < max_events {
            if lambda <= 0.0 {
                // No chance of any more events under a stationary model.
                break;
            }
            // Conservative upper bound: λ(t) + α. Between events the
            // rate only decays, so this dominates the true intensity on
            // the interval and keeps the rejection rate bounded away
            // from 1 (the acceptance ratio is at least exp(-β·dt) with
            // the extra α slack).
            let lambda_bar = lambda + self.alpha;
            // Loop until a candidate is accepted.
            loop {
                let u1: f64 = rng.random::<f64>().max(f64::MIN_POSITIVE);
                let u2: f64 = rng.random::<f64>();
                let dt = -log(u1) / lambda_bar;
                let t_candidate = t + dt;
                if t_candidate > horizon {
                    return events;
                }
                // Intensity at the candidate time: the *excitation*
                // decays by `exp(-β·dt)`; the background is `mu`.
                let lambda_at_candidate = self.intensity_incremental(lambda, dt);
                // Accept iff U₂ ≤ λ(t+dt) / λ̄.
                if u2 * lambda_bar <= lambda_at_candidate {
                    let new_lambda = self.apply_event(lambda, dt);
                    events.push(t_candidate);
                    t = t_candidate;
                    lambda = new_lambda;
                    break;
                }
                // Rejected — loop with the same `t` and `lambda`.
            }
        }
        events
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    clippy::panic,
    reason = "test assertions on exact hand-computed intensity values and parameter validation; tests use panic! for invariant failures"
)]
mod tests {
    use super::*;

    fn unwrap_or_panic<T, E: core::fmt::Debug>(r: Result<T, E>, ctx: &str) -> T {
        match r {
            Ok(v) => v,
            Err(e) => panic!("{ctx}: {e:?}"),
        }
    }

    fn unwrap_err_or_panic<T: core::fmt::Debug, E>(r: Result<T, E>, ctx: &str) -> E {
        match r {
            Err(e) => e,
            Ok(v) => panic!("{ctx}: expected Err, got Ok({v:?})"),
        }
    }

    #[test]
    fn rejects_negative_background_rate() {
        let err = unwrap_err_or_panic(HawkesProcess::new(-0.1, 1.0, 1.0), "negative mu");
        assert_eq!(err, HawkesError::NegativeBackgroundRate);
    }

    #[test]
    fn rejects_negative_excitation() {
        let err = unwrap_err_or_panic(HawkesProcess::new(1.0, -0.1, 1.0), "negative alpha");
        assert_eq!(err, HawkesError::NegativeExcitation);
    }

    #[test]
    fn rejects_non_positive_decay() {
        for beta in [0.0, -1.0] {
            let err = unwrap_err_or_panic(HawkesProcess::new(1.0, 1.0, beta), "non-positive beta");
            assert_eq!(err, HawkesError::NonPositiveDecay);
        }
    }

    #[test]
    fn accepts_zero_background_rate() {
        // Self-exciting only; every event is offspring of a prior one.
        let p = unwrap_or_panic(HawkesProcess::new(0.0, 0.5, 2.0), "construction");
        assert_eq!(p.branching_ratio(), 0.25);
        assert!(p.is_stationary());
        assert_eq!(p.long_run_rate(), Some(0.0));
    }

    #[test]
    fn branching_ratio_correct_for_known_params() {
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 1.5, 2.0), "construction");
        assert!((p.branching_ratio() - 0.75).abs() < 1e-12);
        assert!(p.is_stationary());
        assert!((p.long_run_rate().unwrap_or_else(|| panic!("long_run_rate")) - 0.4).abs() < 1e-12);
    }

    #[test]
    fn explosive_process_flagged() {
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 1.2, 1.0), "construction");
        assert!(!p.is_stationary());
        assert_eq!(p.long_run_rate(), None);
    }

    #[test]
    fn intensity_at_hand_computed() {
        // mu=0, alpha=2, beta=1, one event at t=1.0.
        // λ(2) = 2·exp(-1·1) = 2/e.
        let p = unwrap_or_panic(HawkesProcess::new(0.0, 2.0, 1.0), "construction");
        let lambda = p.intensity_at(2.0, &[1.0]);
        let expected = 2.0_f64 * exp(-1.0);
        assert!((lambda - expected).abs() < 1e-12);
    }

    #[test]
    fn intensity_at_spikes_immediately_after_event() {
        // μ=1, α=5, β=2. At t=0 with no events, λ=1. At t=0+ε after a
        // single event at t=0, λ=6.
        let p = unwrap_or_panic(HawkesProcess::new(1.0, 5.0, 2.0), "construction");
        let lambda_before = p.intensity_at(0.0, &[]);
        let lambda_after = p.intensity_at(1e-9, &[0.0]);
        assert!((lambda_before - 1.0).abs() < 1e-12);
        assert!(
            lambda_after > 5.5,
            "spike should dominate: got {lambda_after}"
        );
        // Decays toward mu as t grows.
        let lambda_far = p.intensity_at(100.0, &[0.0]);
        assert!(
            lambda_far < 1.1,
            "long-run intensity should approach μ: got {lambda_far}"
        );
    }

    #[test]
    fn intensity_at_monotone_decay_between_events() {
        // μ=0, α=10, β=1. One event at t=2. λ(t) = 10·exp(-(t-2)) for
        // t > 2.
        let p = unwrap_or_panic(HawkesProcess::new(0.0, 10.0, 1.0), "construction");
        let samples: Vec<f64> = (3..=8)
            .map(|t| p.intensity_at(f64::from(t), &[2.0]))
            .collect();
        for w in samples.windows(2) {
            let a = w.first().copied();
            let b = w.get(1).copied();
            match (a, b) {
                (Some(a), Some(b)) => assert!(a > b, "intensity must decay between events: {w:?}"),
                _ => panic!("test bug: window of size 2 had fewer than 2 elements"),
            }
        }
    }

    #[test]
    fn intensity_incremental_matches_direct_summation() {
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 0.7, 1.5), "construction");
        let history = [0.0, 1.3, 2.7, 4.1];
        for &t in &[5.0, 6.5, 7.0] {
            let direct = p.intensity_at(t, &history);
            // Build the incremental form: walk the event history, applying
            // each event with [`HawkesProcess::apply_event`]. After the
            // last event time before `t`, decay freely to `t` via
            // [`HawkesProcess::intensity_incremental`].
            let mut lambda = p.mu;
            let mut last_t = 0.0_f64;
            for &ti in &history {
                if ti >= t {
                    break;
                }
                lambda = p.apply_event(lambda, ti - last_t);
                last_t = ti;
            }
            lambda = p.intensity_incremental(lambda, t - last_t);
            assert!(
                (direct - lambda).abs() < 1e-9,
                "incremental {lambda} vs direct {direct} at t={t}"
            );
        }
    }

    #[test]
    fn simulate_is_deterministic_under_fixed_seed() {
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 0.8, 2.0), "construction");
        let a = p.simulate(100.0, 42, 1000);
        let b = p.simulate(100.0, 42, 1000);
        assert_eq!(
            a, b,
            "deterministic replay must produce identical event lists"
        );
    }

    #[test]
    fn simulate_different_seeds_produce_different_arrivals() {
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 0.8, 2.0), "construction");
        let a = p.simulate(100.0, 42, 1000);
        let b = p.simulate(100.0, 43, 1000);
        assert_ne!(
            a, b,
            "different seeds should produce different arrival sequences"
        );
    }

    #[test]
    fn simulate_returns_sorted_arrivals_within_horizon() {
        let p = unwrap_or_panic(HawkesProcess::new(0.5, 0.4, 1.0), "construction");
        let events = p.simulate(20.0, 7, 500);
        for w in events.windows(2) {
            let a = w.first().copied();
            let b = w.get(1).copied();
            match (a, b) {
                (Some(a), Some(b)) => {
                    assert!(a < b, "events must be strictly sorted: {w:?}");
                    assert!(b <= 20.0, "events must lie within the horizon");
                }
                _ => panic!("test bug: window of size 2 had fewer than 2 elements"),
            }
        }
    }

    #[test]
    fn simulate_max_events_caps_output() {
        // A near-critical process — many events on a long horizon.
        let p = unwrap_or_panic(HawkesProcess::new(0.1, 0.95, 1.0), "construction");
        let events = p.simulate(1e6, 0, 50);
        assert_eq!(events.len(), 50, "max_events cap must hold");
    }

    #[test]
    fn simulate_zero_horizon_yields_empty() {
        let p = unwrap_or_panic(HawkesProcess::new(1.0, 0.5, 1.0), "construction");
        let events = p.simulate(0.0, 0, 100);
        assert!(events.is_empty());
    }

    #[test]
    fn simulate_zero_alpha_is_poisson() {
        // α=0 → pure Poisson with rate μ.
        let p = unwrap_or_panic(HawkesProcess::new(2.0, 0.0, 1.0), "construction");
        assert_eq!(p.branching_ratio(), 0.0);
        let events = p.simulate(100.0, 11, 2000);
        // Mean rate ≈ μ = 2 events/unit. Over 100 units → ~200 events.
        assert!(
            events.len() > 100,
            "should produce many events: got {}",
            events.len()
        );
        assert!(events.len() <= 200);
    }

    #[test]
    fn empirical_mean_rate_matches_long_run_for_stationary() {
        // Use small alpha (n = 0.05) so the process equilibrates
        // quickly and the empirical rate is close to the long-run
        // value. The tolerance is loose (20%) to avoid CI flakiness
        // on warm-up / transient sensitivity.
        //
        // μ=1, α=0.05, β=1 → n=0.05 → long-run rate = 1/0.95 ≈ 1.053.
        let p = unwrap_or_panic(HawkesProcess::new(1.0, 0.05, 1.0), "construction");
        let long_run = p.long_run_rate().unwrap_or_else(|| panic!("long_run_rate"));
        let events = p.simulate(1e5, 1, 200_000);
        let horizon = events.last().copied().unwrap_or(0.0);
        if horizon > 0.0 {
            let count = f64::from(u32::try_from(events.len()).unwrap_or(u32::MAX));
            let empirical = count / horizon;
            assert!(
                (empirical - long_run).abs() / long_run < 0.20,
                "empirical {empirical} vs long-run {long_run}"
            );
        }
    }

    #[test]
    fn clustering_is_stronger_with_higher_alpha() {
        // Compare two processes with the same mean rate but different
        // clustering. Same long-run rate:
        //   Poisson: μ=0.5, α=0
        //   Bursty: μ=0.3, α=1.2, β=2 → n=0.6 → rate=0.3/0.4=0.75
        // Pick rate=0.75 for both:
        //   Poisson: μ=0.75
        //   Bursty: μ=0.3, α=1.2, β=2
        let poisson = unwrap_or_panic(HawkesProcess::new(0.75, 0.0, 1.0), "construction");
        let bursty = unwrap_or_panic(HawkesProcess::new(0.3, 1.2, 2.0), "construction");
        let p_evt = poisson.simulate(5e3, 7, 50_000);
        let b_evt = bursty.simulate(5e3, 7, 50_000);
        // Coefficient of variation of inter-arrival times: bursty should
        // be more clustered (higher CV than Poisson's 1.0).
        let cv = |evs: &[f64]| -> f64 {
            if evs.len() < 2 {
                return 0.0;
            }
            // Build inter-arrival diffs without raw indexing: each
            // window is guaranteed to have two elements (the
            // pre-condition above), so unwrap_or_panic with a clear
            // message is the explicit failure path.
            let diffs: Vec<f64> = evs
                .windows(2)
                .map(|w| {
                    let a = w.first().copied();
                    let b = w.get(1).copied();
                    match (a, b) {
                        (Some(a), Some(b)) => b - a,
                        _ => panic!(
                            "test bug: windows(2) yielded a slice with fewer than 2 elements"
                        ),
                    }
                })
                .collect();
            let n = f64::from(u32::try_from(diffs.len()).unwrap_or(u32::MAX));
            let mean = diffs.iter().sum::<f64>() / n;
            if mean <= 0.0 {
                return 0.0;
            }
            let var = diffs.iter().map(|&d| (d - mean) * (d - mean)).sum::<f64>() / n;
            (var / (mean * mean)).sqrt()
        };
        let cv_p = cv(&p_evt);
        let cv_b = cv(&b_evt);
        assert!(
            cv_b > cv_p,
            "bursty CV ({cv_b}) should exceed Poisson CV ({cv_p})"
        );
    }
}
