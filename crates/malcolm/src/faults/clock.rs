//! Clock fault layer: skew, freeze, jump.
//!
//! Provides three fault primitives for simulating clock anomalies in distributed
//! systems, critical for testing consensus algorithms, TTL logic, and time-based
//! retry policies.
//!
//! # Important
//!
//! These faults only affect code that uses [`MalcolmClock`](crate::fault::MalcolmClock)
//! to read time. They do **not** modify the system clock.
//!
//! # Primitives
//!
//! - [`ClockSkew`]: continuously drifts reported time using [`BrownNoise`].
//! - [`ClockFreeze`]: captures the current timestamp and records when the freeze started.
//! - [`ClockJump`]: jumps time forward or backward by a [`LogNormal`]-sampled amount.
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults::clock::{ClockJump, JumpDirection};
//! use malcolm::fault::{Fault, FaultContext, MockClock};
//! use malcolm_core::bifurcation::BifurcationProfile;
//! use malcolm_core::types::FaultResult;
//!
//! let fault = ClockJump::builder()
//!     .seed(42)
//!     .mu(3.0)
//!     .sigma(0.5)
//!     .direction(JumpDirection::Forward)
//!     .clock(Box::new(MockClock::default()))
//!     .build();
//! let ctx = FaultContext {
//!     seed: 42,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::clock_skew(),
//! };
//! assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
//! ```

use std::fmt;
use std::sync::Mutex;
use std::time::Instant;

use rand::SeedableRng as _;
use rand::rngs::SmallRng;

use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::distributions::{DistributionSampler as _, LogNormal};
use malcolm_core::noise::BrownNoise;
use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult};

use crate::fault::{Fault, FaultContext, MalcolmClock, RealClock};

// ── JumpDirection ─────────────────────────────────────────────────────────────

/// Direction of a clock jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JumpDirection {
    /// Jump time forward (increase the reported timestamp).
    Forward,
    /// Jump time backward (decrease the reported timestamp, clamped to zero).
    Backward,
}

// ── ClockSkew ─────────────────────────────────────────────────────────────────

/// Simulates a continuously drifting clock.
///
/// Each call to [`inject`](Fault::inject) samples one step of a [`BrownNoise`]
/// generator to compute the current drift offset (in milliseconds). Over many
/// calls the drift wanders realistically, modelling oscillator jitter or NTP
/// desynchronisation.
///
/// **Only code that reads time via [`MalcolmClock::now_ms`] through this fault
/// is affected. The system clock is not modified.**
///
/// # Example
///
/// ```rust
/// use malcolm::faults::clock::ClockSkew;
/// use malcolm::fault::{Fault, FaultContext, MockClock};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = ClockSkew::builder()
///     .seed(1)
///     .drift_rate_ms_per_step(10.0)
///     .intensity(0.5)
///     .clock(Box::new(MockClock::default()))
///     .build();
/// let ctx = FaultContext {
///     seed: 1,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::clock_skew(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
pub struct ClockSkew {
    inner: Box<dyn MalcolmClock>,
    seed: u64,
    drift_rate_ms_per_step: f64,
    intensity: f64,
    noise: Mutex<BrownNoise>,
}

impl fmt::Debug for ClockSkew {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockSkew")
            .field("seed", &self.seed)
            .field("drift_rate_ms_per_step", &self.drift_rate_ms_per_step)
            .field("intensity", &self.intensity)
            .finish_non_exhaustive()
    }
}

impl ClockSkew {
    /// Begin constructing a [`ClockSkew`] fault.
    #[must_use]
    pub fn builder() -> ClockSkewBuilder {
        ClockSkewBuilder::default()
    }
}

impl Fault for ClockSkew {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let noise_sample = {
            let mut guard = match self.noise.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            guard.next().unwrap_or(0.0)
        };

        // noise_sample is in [-1.0, 1.0]; scale to [-drift_rate * intensity, +drift_rate * intensity]
        let drift_ms = noise_sample * self.drift_rate_ms_per_step * self.intensity;
        let real_ms = self.inner.now_ms();

        let drift_abs = drift_ms.abs();
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "drift_abs is bounded by drift_rate_ms_per_step * intensity, expected well within u64 range"
        )]
        let drift_abs_u64 = drift_abs as u64;

        let skewed_ms = if drift_ms >= 0.0 {
            real_ms.saturating_add(drift_abs_u64)
        } else {
            real_ms.saturating_sub(drift_abs_u64)
        };

        tracing::info!(
            target: "malcolm",
            fault_type = "clock_skew",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            drift_ms = drift_ms,
            real_ms = real_ms,
            skewed_ms = skewed_ms,
            dry_run = false,
            "clock skew injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "clock_skew".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: skewed_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let max_drift = self.drift_rate_ms_per_step * self.intensity;
        let reason = format!(
            "would drift clock for node {} by up to \u{00b1}{max_drift:.2}ms per step (intensity {:.2})",
            ctx.node_id, self.intensity,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "clock_skew",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            max_drift_ms = max_drift,
            dry_run = true,
            "clock skew dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "clock_skew"
    }
}

// ── ClockSkewBuilder ──────────────────────────────────────────────────────────

/// Builder for [`ClockSkew`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `drift_rate_ms_per_step`: `10.0`
/// - `intensity`: `1.0`
/// - `clock`: [`RealClock`]
#[derive(Default)]
pub struct ClockSkewBuilder {
    seed: Option<u64>,
    drift_rate_ms_per_step: Option<f64>,
    intensity: Option<f64>,
    clock: Option<Box<dyn MalcolmClock>>,
}

impl ClockSkewBuilder {
    /// Set the RNG seed for deterministic noise generation.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the maximum drift magnitude per noise step in milliseconds.
    #[must_use]
    pub const fn drift_rate_ms_per_step(mut self, rate: f64) -> Self {
        self.drift_rate_ms_per_step = Some(rate);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Set the inner clock to wrap.
    #[must_use]
    pub fn clock(mut self, clock: Box<dyn MalcolmClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Consume the builder and produce a [`ClockSkew`].
    #[must_use]
    pub fn build(self) -> ClockSkew {
        let seed = self.seed.unwrap_or(0);
        let drift_rate = self.drift_rate_ms_per_step.unwrap_or(10.0);
        let intensity = self.intensity.unwrap_or(1.0);
        let inner: Box<dyn MalcolmClock> = self.clock.unwrap_or_else(|| Box::new(RealClock));

        ClockSkew {
            inner,
            seed,
            drift_rate_ms_per_step: drift_rate,
            intensity,
            noise: Mutex::new(BrownNoise::new(seed)),
        }
    }
}

// ── ClockFreeze ───────────────────────────────────────────────────────────────

/// Simulates a frozen clock that returns the same timestamp for a configurable
/// duration.
///
/// The frozen timestamp is captured from the inner clock at
/// [`inject`](Fault::inject) time. The `freeze_duration_ms` field documents the
/// intended freeze window and is included in tracing events for observability.
///
/// **Only code that reads time via [`MalcolmClock::now_ms`] is affected. The
/// system clock is not modified.**
///
/// # Example
///
/// ```rust
/// use malcolm::faults::clock::ClockFreeze;
/// use malcolm::fault::{Fault, FaultContext, MockClock};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = ClockFreeze::builder()
///     .seed(7)
///     .freeze_duration_ms(500)
///     .clock(Box::new(MockClock::default()))
///     .build();
/// let ctx = FaultContext {
///     seed: 7,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::clock_skew(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
pub struct ClockFreeze {
    inner: Box<dyn MalcolmClock>,
    seed: u64,
    freeze_duration_ms: u64,
    freeze_started: Mutex<Option<Instant>>,
}

impl fmt::Debug for ClockFreeze {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockFreeze")
            .field("seed", &self.seed)
            .field("freeze_duration_ms", &self.freeze_duration_ms)
            .finish_non_exhaustive()
    }
}

impl ClockFreeze {
    /// Begin constructing a [`ClockFreeze`] fault.
    #[must_use]
    pub fn builder() -> ClockFreezeBuilder {
        ClockFreezeBuilder::default()
    }
}

impl Fault for ClockFreeze {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let frozen_ms = self.inner.now_ms();

        {
            let mut guard = match self.freeze_started.lock() {
                Ok(g) => g,
                Err(e) => e.into_inner(),
            };
            *guard = Some(Instant::now());
        }

        tracing::info!(
            target: "malcolm",
            fault_type = "clock_freeze",
            node_id = %ctx.node_id,
            seed = self.seed,
            frozen_ms = frozen_ms,
            freeze_duration_ms = self.freeze_duration_ms,
            dry_run = false,
            "clock freeze injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "clock_freeze".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: 1.0,
            dry_run: false,
            timestamp_ms: frozen_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let frozen_ms = self.inner.now_ms();
        let reason = format!(
            "would freeze clock for node {} at {frozen_ms}ms for {}ms",
            ctx.node_id, self.freeze_duration_ms,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "clock_freeze",
            node_id = %ctx.node_id,
            seed = self.seed,
            frozen_ms = frozen_ms,
            freeze_duration_ms = self.freeze_duration_ms,
            dry_run = true,
            "clock freeze dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "clock_freeze"
    }
}

// ── ClockFreezeBuilder ────────────────────────────────────────────────────────

/// Builder for [`ClockFreeze`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `freeze_duration_ms`: `1_000`
/// - `clock`: [`RealClock`]
#[derive(Default)]
pub struct ClockFreezeBuilder {
    seed: Option<u64>,
    freeze_duration_ms: Option<u64>,
    clock: Option<Box<dyn MalcolmClock>>,
}

impl ClockFreezeBuilder {
    /// Set the RNG seed (recorded in the emitted [`FaultEvent`]).
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the intended freeze duration in milliseconds.
    #[must_use]
    pub const fn freeze_duration_ms(mut self, ms: u64) -> Self {
        self.freeze_duration_ms = Some(ms);
        self
    }

    /// Set the inner clock to wrap.
    #[must_use]
    pub fn clock(mut self, clock: Box<dyn MalcolmClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Consume the builder and produce a [`ClockFreeze`].
    #[must_use]
    pub fn build(self) -> ClockFreeze {
        ClockFreeze {
            inner: self.clock.unwrap_or_else(|| Box::new(RealClock)),
            seed: self.seed.unwrap_or(0),
            freeze_duration_ms: self.freeze_duration_ms.unwrap_or(1_000),
            freeze_started: Mutex::new(None),
        }
    }
}

// ── ClockJump ─────────────────────────────────────────────────────────────────

/// Simulates an abrupt clock jump — a forward or backward time discontinuity.
///
/// The jump magnitude is sampled from a [`LogNormal`] distribution, producing
/// realistic heavy-tailed jump sizes consistent with NTP step corrections and
/// virtualised environment clock resets.
///
/// **Clock jumps emit at `tracing::warn!` level** because abrupt discontinuities
/// can corrupt distributed consensus, invalidate TTL-based caches, and break
/// time-ordered event logs.
///
/// **Only code that reads time via [`MalcolmClock::now_ms`] is affected. The
/// system clock is not modified.**
///
/// # Example
///
/// ```rust
/// use malcolm::faults::clock::{ClockJump, JumpDirection};
/// use malcolm::fault::{Fault, FaultContext, MockClock};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = ClockJump::builder()
///     .seed(9)
///     .mu(5.0)
///     .sigma(1.0)
///     .direction(JumpDirection::Backward)
///     .clock(Box::new(MockClock::at(100_000)))
///     .build();
/// let ctx = FaultContext {
///     seed: 9,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::clock_skew(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
pub struct ClockJump {
    inner: Box<dyn MalcolmClock>,
    seed: u64,
    mu: f64,
    sigma: f64,
    direction: JumpDirection,
}

impl fmt::Debug for ClockJump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClockJump")
            .field("seed", &self.seed)
            .field("mu", &self.mu)
            .field("sigma", &self.sigma)
            .field("direction", &self.direction)
            .finish_non_exhaustive()
    }
}

impl ClockJump {
    /// Begin constructing a [`ClockJump`] fault.
    #[must_use]
    pub fn builder() -> ClockJumpBuilder {
        ClockJumpBuilder::default()
    }
}

impl Fault for ClockJump {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let jump_magnitude = LogNormal {
            mu: self.mu,
            sigma: self.sigma,
        }
        .sample(&mut rng);

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "jump_magnitude from LogNormal with typical mu/sigma is far below u64::MAX"
        )]
        let jump_ms = jump_magnitude as u64;

        let real_ms = self.inner.now_ms();
        let jumped_ms = match self.direction {
            JumpDirection::Forward => real_ms.saturating_add(jump_ms),
            JumpDirection::Backward => real_ms.saturating_sub(jump_ms),
        };

        tracing::warn!(
            target: "malcolm",
            fault_type = "clock_jump",
            node_id = %ctx.node_id,
            seed = self.seed,
            direction = ?self.direction,
            jump_ms = jump_ms,
            real_ms = real_ms,
            jumped_ms = jumped_ms,
            dry_run = false,
            "clock jump injected — abrupt time discontinuity",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "clock_jump".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: 1.0,
            dry_run: false,
            timestamp_ms: jumped_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let jump_magnitude = LogNormal {
            mu: self.mu,
            sigma: self.sigma,
        }
        .sample(&mut rng);

        let reason = format!(
            "would jump clock for node {} {:?} by {jump_magnitude:.2}ms (LogNormal mu={:.2} sigma={:.2})",
            ctx.node_id, self.direction, self.mu, self.sigma,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "clock_jump",
            node_id = %ctx.node_id,
            seed = self.seed,
            direction = ?self.direction,
            jump_magnitude = jump_magnitude,
            dry_run = true,
            "clock jump dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "clock_jump"
    }
}

// ── ClockJumpBuilder ──────────────────────────────────────────────────────────

/// Builder for [`ClockJump`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `mu`: `3.0` (log-mean jump ≈ 20 seconds)
/// - `sigma`: `1.0`
/// - `direction`: [`JumpDirection::Forward`]
/// - `clock`: [`RealClock`]
#[derive(Default)]
pub struct ClockJumpBuilder {
    seed: Option<u64>,
    mu: Option<f64>,
    sigma: Option<f64>,
    direction: Option<JumpDirection>,
    clock: Option<Box<dyn MalcolmClock>>,
}

impl ClockJumpBuilder {
    /// Set the RNG seed for deterministic jump sampling.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the log-space mean of the jump distribution.
    #[must_use]
    pub const fn mu(mut self, mu: f64) -> Self {
        self.mu = Some(mu);
        self
    }

    /// Set the log-space standard deviation of the jump distribution.
    #[must_use]
    pub const fn sigma(mut self, sigma: f64) -> Self {
        self.sigma = Some(sigma);
        self
    }

    /// Set the jump direction.
    #[must_use]
    pub const fn direction(mut self, direction: JumpDirection) -> Self {
        self.direction = Some(direction);
        self
    }

    /// Set the inner clock to wrap.
    #[must_use]
    pub fn clock(mut self, clock: Box<dyn MalcolmClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Consume the builder and produce a [`ClockJump`].
    #[must_use]
    pub fn build(self) -> ClockJump {
        ClockJump {
            inner: self.clock.unwrap_or_else(|| Box::new(RealClock)),
            seed: self.seed.unwrap_or(0),
            mu: self.mu.unwrap_or(3.0),
            sigma: self.sigma.unwrap_or(1.0),
            direction: self.direction.unwrap_or(JumpDirection::Forward),
        }
    }
}

// ── ClockFaultSuite ───────────────────────────────────────────────────────────

/// A bundled set of clock faults sharing a [`BifurcationProfile`].
///
/// Provides a convenience constructor for common clock-fault scenarios, and an
/// [`inject_all`](Self::inject_all) helper that fires all three fault types in
/// sequence.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::clock::ClockFaultSuite;
/// use malcolm::fault::FaultContext;
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let suite = ClockFaultSuite::new(42);
/// assert_eq!(suite.profile, BifurcationProfile::clock_skew());
/// ```
pub struct ClockFaultSuite {
    /// Continuously drifting clock fault.
    pub skew: ClockSkew,
    /// Momentarily frozen clock fault.
    pub freeze: ClockFreeze,
    /// Abrupt clock jump fault.
    pub jump: ClockJump,
    /// Bifurcation profile for the clock regime.
    pub profile: BifurcationProfile,
}

impl ClockFaultSuite {
    /// Construct a suite with all three clock faults sharing the given seed.
    ///
    /// Seeds are offset by 1 and 2 for `freeze` and `jump` respectively, so
    /// the three noise streams are independent.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::faults::clock::ClockFaultSuite;
    /// use malcolm::fault::{Fault, FaultContext};
    /// use malcolm_core::bifurcation::BifurcationProfile;
    ///
    /// let suite = ClockFaultSuite::new(1);
    /// let ctx = FaultContext {
    ///     seed: 1,
    ///     timestamp_ms: 0,
    ///     node_id: "n0".to_owned(),
    ///     profile: BifurcationProfile::clock_skew(),
    /// };
    /// let results = suite.inject_all(&ctx);
    /// assert_eq!(results.len(), 3);
    /// ```
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self {
            skew: ClockSkew::builder().seed(seed).build(),
            freeze: ClockFreeze::builder().seed(seed.wrapping_add(1)).build(),
            jump: ClockJump::builder().seed(seed.wrapping_add(2)).build(),
            profile: BifurcationProfile::clock_skew(),
        }
    }

    /// Inject all three clock faults and return their results.
    pub fn inject_all(&self, ctx: &FaultContext) -> Vec<FaultResult> {
        vec![
            self.skew.inject(ctx),
            self.freeze.inject(ctx),
            self.jump.inject(ctx),
        ]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use tracing_test::traced_test;

    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::FaultResult;

    use crate::fault::{Fault, FaultContext, MalcolmClock, MockClock};

    use super::{ClockFreeze, ClockJump, ClockSkew, JumpDirection};

    fn make_ctx() -> FaultContext {
        FaultContext {
            seed: 1,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::clock_skew(),
        }
    }

    // ── Test 1: MockClock::advance ───────────────────────────────────────────

    #[test]
    fn mock_clock_advance_increments_time() {
        let clock = MockClock::default();
        let initial = clock.now_ms();
        clock.advance(100);
        assert_eq!(clock.now_ms(), initial + 100);
    }

    // ── Test 2: MockClock::freeze ────────────────────────────────────────────

    #[test]
    fn mock_clock_freeze_returns_same_value() {
        let clock = MockClock::at(5_000);
        clock.freeze();
        let t1 = clock.now_ms();
        clock.advance(999); // ignored while frozen
        let t2 = clock.now_ms();
        clock.advance(999);
        let t3 = clock.now_ms();
        assert_eq!(t1, 5_000);
        assert_eq!(t2, 5_000);
        assert_eq!(t3, 5_000);
    }

    // ── Test 3: MockClock::jump ──────────────────────────────────────────────

    #[test]
    fn mock_clock_jump_forward_and_backward() {
        let clock = MockClock::at(1_000);
        clock.jump(500);
        assert_eq!(clock.now_ms(), 1_500);
        clock.jump(-200);
        assert_eq!(clock.now_ms(), 1_300);
    }

    // ── Test 4: ClockSkew::dry_run does not advance noise ────────────────────

    #[test]
    fn clock_skew_dry_run_does_not_advance_noise() {
        let fault = ClockSkew::builder()
            .seed(42)
            .drift_rate_ms_per_step(10.0)
            .intensity(1.0)
            .clock(Box::new(MockClock::default()))
            .build();
        let ctx = make_ctx();

        let report1 = fault.dry_run(&ctx);
        let report2 = fault.dry_run(&ctx);

        // dry_run must not touch the noise iterator; both reports are identical
        assert!(report1.would_inject);
        assert!(report2.would_inject);
        assert_eq!(report1.reason, report2.reason);
        assert_eq!(report1.fault_type, "clock_skew");
    }

    // ── Test 5: ClockJump::inject emits tracing::warn ────────────────────────

    #[traced_test]
    #[test]
    fn clock_jump_inject_emits_warn() {
        let fault = ClockJump::builder()
            .seed(7)
            .mu(3.0)
            .sigma(1.0)
            .direction(JumpDirection::Forward)
            .clock(Box::new(MockClock::default()))
            .build();
        let ctx = make_ctx();
        let _ = fault.inject(&ctx);
        assert!(logs_contain("clock jump injected"));
    }

    // ── Test 6: ClockFreeze::inject returns FaultResult::Injected ────────────

    #[test]
    fn clock_freeze_inject_returns_injected() {
        let fault = ClockFreeze::builder()
            .seed(3)
            .freeze_duration_ms(200)
            .clock(Box::new(MockClock::at(42_000)))
            .build();
        let ctx = make_ctx();
        assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
    }
}
