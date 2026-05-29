//! Network fault layer: partition, packet loss, latency spike, bandwidth throttle.
//!
//! Each fault type implements the [`Fault`](crate::fault::Fault) port trait.
//! Builders use an options-struct pattern to apply defaults at `build()` time.
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults::network::{NetworkPartition, NoiseType, LatencySpike};
//! use malcolm::fault::{Fault, FaultContext};
//! use malcolm_core::bifurcation::BifurcationProfile;
//! use malcolm_core::types::FaultResult;
//!
//! let fault = NetworkPartition::builder().seed(42).alpha(1.5).intensity(0.7).build();
//! let ctx = FaultContext {
//!     seed: 42,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
//! ```

use rand::SeedableRng;
use rand::rngs::SmallRng;

use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::distributions::{DistributionSampler, LogNormal, Pareto, PowerLaw};
use malcolm_core::noise::{BrownNoise, PinkNoise};
use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult};

use crate::fault::{Fault, FaultContext};

// ── NoiseType ─────────────────────────────────────────────────────────────────

/// The correlated noise model used to add timing jitter to latency spikes.
///
/// Both variants use the same seed offset (`seed + 1`) so that the noise
/// stream is independent from the base-latency sample stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseType {
    /// Pink (1/f) noise — moderate long-range correlation.
    Pink,
    /// Brown (1/f²) noise — strong low-frequency drift characteristic of
    /// Brownian motion.
    Brown,
}

// ── NetworkPartition ──────────────────────────────────────────────────────────

/// Simulates a network partition between node groups.
///
/// Partition duration is sampled from a [`PowerLaw`] distribution, producing
/// heavy-tailed outage durations consistent with real incident data.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::network::NetworkPartition;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = NetworkPartition::builder()
///     .seed(42)
///     .alpha(1.5)
///     .intensity(0.7)
///     .build();
/// let ctx = FaultContext {
///     seed: 42,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::network_partition(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct NetworkPartition {
    seed: u64,
    alpha: f64,
    intensity: f64,
}

impl NetworkPartition {
    /// Begin constructing a [`NetworkPartition`] fault.
    #[must_use]
    pub fn builder() -> NetworkPartitionBuilder {
        NetworkPartitionBuilder::default()
    }
}

impl Fault for NetworkPartition {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let duration = PowerLaw { alpha: self.alpha }.sample(&mut rng);

        tracing::info!(
            fault_type = "network_partition",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            duration_s = duration,
            "network partition injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "network_partition".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let duration = PowerLaw { alpha: self.alpha }.sample(&mut rng);

        let reason = format!(
            "would partition node {} for {:.2}s at intensity {:.2}",
            ctx.node_id, duration, self.intensity,
        );

        tracing::debug!(
            fault_type = "network_partition",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            duration_s = duration,
            "network partition dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "network_partition"
    }
}

// ── NetworkPartitionBuilder ───────────────────────────────────────────────────

/// Builder for [`NetworkPartition`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `alpha`: `1.5`
/// - `intensity`: `1.0`
#[derive(Debug, Default)]
pub struct NetworkPartitionBuilder {
    seed: Option<u64>,
    alpha: Option<f64>,
    intensity: Option<f64>,
}

impl NetworkPartitionBuilder {
    /// Set the RNG seed for deterministic replay.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the power-law exponent `alpha` (must be > 1 for a proper distribution).
    #[must_use]
    pub const fn alpha(mut self, alpha: f64) -> Self {
        self.alpha = Some(alpha);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Consume the builder and produce a [`NetworkPartition`].
    #[must_use]
    pub fn build(self) -> NetworkPartition {
        NetworkPartition {
            seed: self.seed.unwrap_or(0),
            alpha: self.alpha.unwrap_or(1.5),
            intensity: self.intensity.unwrap_or(1.0),
        }
    }
}

// ── PacketLoss ────────────────────────────────────────────────────────────────

/// Simulates probabilistic packet loss with Pareto-distributed burst sizes.
///
/// The Pareto distribution produces heavy-tailed loss bursts more consistent
/// with real network behaviour than uniform random loss.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::network::PacketLoss;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let fault = PacketLoss::builder()
///     .seed(7)
///     .alpha(2.0)
///     .x_min(1.0)
///     .intensity(0.5)
///     .build();
/// let ctx = FaultContext {
///     seed: 7,
///     timestamp_ms: 0,
///     node_id: "router-0".to_owned(),
///     profile: BifurcationProfile::network_partition(),
/// };
/// let report = fault.dry_run(&ctx);
/// assert!(report.would_inject);
/// ```
#[derive(Debug, Clone)]
pub struct PacketLoss {
    seed: u64,
    alpha: f64,
    x_min: f64,
    intensity: f64,
}

impl PacketLoss {
    /// Begin constructing a [`PacketLoss`] fault.
    #[must_use]
    pub fn builder() -> PacketLossBuilder {
        PacketLossBuilder::default()
    }
}

impl Fault for PacketLoss {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let loss_rate = Pareto {
            alpha: self.alpha,
            x_min: self.x_min,
        }
        .sample(&mut rng);

        tracing::info!(
            fault_type = "packet_loss",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            loss_rate = loss_rate,
            "packet loss injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "packet_loss".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let loss_rate = Pareto {
            alpha: self.alpha,
            x_min: self.x_min,
        }
        .sample(&mut rng);

        let reason = format!(
            "would drop packets at rate {:.3} on node {} (Pareto alpha={:.2}, x_min={:.2})",
            loss_rate, ctx.node_id, self.alpha, self.x_min,
        );

        tracing::debug!(
            fault_type = "packet_loss",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            loss_rate = loss_rate,
            "packet loss dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "packet_loss"
    }
}

// ── PacketLossBuilder ─────────────────────────────────────────────────────────

/// Builder for [`PacketLoss`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `alpha`: `2.0`
/// - `x_min`: `1.0`
/// - `intensity`: `1.0`
#[derive(Debug, Default)]
pub struct PacketLossBuilder {
    seed: Option<u64>,
    alpha: Option<f64>,
    x_min: Option<f64>,
    intensity: Option<f64>,
}

impl PacketLossBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the Pareto shape parameter `alpha` (must be > 1 for finite mean).
    #[must_use]
    pub const fn alpha(mut self, alpha: f64) -> Self {
        self.alpha = Some(alpha);
        self
    }

    /// Set the Pareto scale parameter `x_min`.
    #[must_use]
    pub const fn x_min(mut self, x_min: f64) -> Self {
        self.x_min = Some(x_min);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Consume the builder and produce a [`PacketLoss`].
    #[must_use]
    pub fn build(self) -> PacketLoss {
        PacketLoss {
            seed: self.seed.unwrap_or(0),
            alpha: self.alpha.unwrap_or(2.0),
            x_min: self.x_min.unwrap_or(1.0),
            intensity: self.intensity.unwrap_or(1.0),
        }
    }
}

// ── LatencySpike ──────────────────────────────────────────────────────────────

/// Injects a latency spike using a log-normal base delay with correlated jitter.
///
/// The base latency is drawn from `LogNormal(mu, sigma)` where
/// `mu = ln(base_ms)`.  Jitter is added via one sample from either
/// [`PinkNoise`] or [`BrownNoise`], scaled to ±10 % of the base delay.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::network::{LatencySpike, NoiseType};
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = LatencySpike::builder()
///     .base_ms(50.0)
///     .sigma(0.3)
///     .noise(NoiseType::Pink)
///     .seed(42)
///     .build();
/// let ctx = FaultContext {
///     seed: 42,
///     timestamp_ms: 0,
///     node_id: "api-0".to_owned(),
///     profile: BifurcationProfile::latency_cascade(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct LatencySpike {
    seed: u64,
    mu: f64,
    sigma: f64,
    noise_type: NoiseType,
    intensity: f64,
}

impl LatencySpike {
    /// Begin constructing a [`LatencySpike`] fault.
    #[must_use]
    pub fn builder() -> LatencySpikeBuilder {
        LatencySpikeBuilder::default()
    }

    /// Sample a latency value in milliseconds from the configured distribution.
    ///
    /// Base delay comes from `LogNormal(mu, sigma)`; jitter is ±10 % of the
    /// base value scaled by one noise sample in `[-1, 1]`.
    fn sample_latency_ms(&self) -> f64 {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let base_ms = LogNormal {
            mu: self.mu,
            sigma: self.sigma,
        }
        .sample(&mut rng);

        // Use seed+1 so the noise stream is independent of the base-latency draw.
        let noise_val = match self.noise_type {
            NoiseType::Pink => PinkNoise::new(self.seed.wrapping_add(1))
                .next()
                .unwrap_or(0.0),
            NoiseType::Brown => BrownNoise::new(self.seed.wrapping_add(1))
                .next()
                .unwrap_or(0.0),
        };

        let jitter = base_ms * 0.1 * noise_val;
        (base_ms + jitter).max(0.0)
    }
}

impl Fault for LatencySpike {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let latency_ms = self.sample_latency_ms();

        tracing::info!(
            fault_type = "latency_spike",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            latency_ms = latency_ms,
            noise_type = ?self.noise_type,
            "latency spike injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "latency_spike".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let latency_ms = self.sample_latency_ms();

        let reason = format!(
            "would inject {latency_ms:.2}ms latency on node {} (mu={:.2}, sigma={:.2}, noise={:?})",
            ctx.node_id, self.mu, self.sigma, self.noise_type,
        );

        tracing::debug!(
            fault_type = "latency_spike",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            latency_ms = latency_ms,
            "latency spike dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "latency_spike"
    }
}

// ── LatencySpikeBuilder ───────────────────────────────────────────────────────

/// Builder for [`LatencySpike`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `base_ms`: `50.0` (stored as `mu = ln(50.0)`)
/// - `sigma`: `0.3`
/// - `noise_type`: [`NoiseType::Pink`]
/// - `intensity`: `1.0`
#[derive(Debug, Default)]
pub struct LatencySpikeBuilder {
    seed: Option<u64>,
    base_ms: Option<f64>,
    sigma: Option<f64>,
    noise_type: Option<NoiseType>,
    intensity: Option<f64>,
}

impl LatencySpikeBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the base latency in milliseconds.
    ///
    /// This is stored internally as `mu = ln(base_ms)` for the log-normal
    /// distribution.  Must be positive.
    #[must_use]
    pub const fn base_ms(mut self, base_ms: f64) -> Self {
        self.base_ms = Some(base_ms);
        self
    }

    /// Set the log-normal sigma (standard deviation in log-space).
    #[must_use]
    pub const fn sigma(mut self, sigma: f64) -> Self {
        self.sigma = Some(sigma);
        self
    }

    /// Set the noise model used for jitter.
    #[must_use]
    pub const fn noise(mut self, noise_type: NoiseType) -> Self {
        self.noise_type = Some(noise_type);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Consume the builder and produce a [`LatencySpike`].
    #[must_use]
    pub fn build(self) -> LatencySpike {
        let base_ms = self.base_ms.unwrap_or(50.0).max(f64::EPSILON);
        LatencySpike {
            seed: self.seed.unwrap_or(0),
            mu: base_ms.ln(),
            sigma: self.sigma.unwrap_or(0.3),
            noise_type: self.noise_type.unwrap_or(NoiseType::Pink),
            intensity: self.intensity.unwrap_or(1.0),
        }
    }
}

// ── BandwidthThrottle ─────────────────────────────────────────────────────────

/// Caps throughput to a configurable bytes-per-second rate.
///
/// The fault records a throttle event on every injection.  When compiled with
/// the `tokio` feature, an `async` companion method (not part of the [`Fault`]
/// trait) is available for inserting an actual `tokio::time::sleep` delay.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::network::BandwidthThrottle;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = BandwidthThrottle::builder()
///     .seed(1)
///     .bytes_per_sec(1_048_576)
///     .intensity(0.9)
///     .build();
/// let ctx = FaultContext {
///     seed: 1,
///     timestamp_ms: 0,
///     node_id: "edge-0".to_owned(),
///     profile: BifurcationProfile::network_partition(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct BandwidthThrottle {
    seed: u64,
    bytes_per_sec: u64,
    intensity: f64,
}

impl BandwidthThrottle {
    /// Begin constructing a [`BandwidthThrottle`] fault.
    #[must_use]
    pub fn builder() -> BandwidthThrottleBuilder {
        BandwidthThrottleBuilder::default()
    }
}

impl Fault for BandwidthThrottle {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        tracing::info!(
            fault_type = "bandwidth_throttle",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            bytes_per_sec = self.bytes_per_sec,
            "bandwidth throttle injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "bandwidth_throttle".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let reason = format!(
            "would cap bandwidth to {} bytes/sec on node {} at intensity {:.2}",
            self.bytes_per_sec, ctx.node_id, self.intensity,
        );

        tracing::debug!(
            fault_type = "bandwidth_throttle",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            bytes_per_sec = self.bytes_per_sec,
            "bandwidth throttle dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "bandwidth_throttle"
    }
}

// ── BandwidthThrottleBuilder ──────────────────────────────────────────────────

/// Builder for [`BandwidthThrottle`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `bytes_per_sec`: `1 MiB/s` (`1_048_576`)
/// - `intensity`: `1.0`
#[derive(Debug, Default)]
pub struct BandwidthThrottleBuilder {
    seed: Option<u64>,
    bytes_per_sec: Option<u64>,
    intensity: Option<f64>,
}

impl BandwidthThrottleBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the maximum throughput in bytes per second.
    #[must_use]
    pub const fn bytes_per_sec(mut self, bytes_per_sec: u64) -> Self {
        self.bytes_per_sec = Some(bytes_per_sec);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Consume the builder and produce a [`BandwidthThrottle`].
    #[must_use]
    pub fn build(self) -> BandwidthThrottle {
        BandwidthThrottle {
            seed: self.seed.unwrap_or(0),
            bytes_per_sec: self.bytes_per_sec.unwrap_or(1_048_576),
            intensity: self.intensity.unwrap_or(1.0),
        }
    }
}

// ── NetworkFaultSuite ─────────────────────────────────────────────────────────

/// A named bundle of network faults with an associated [`BifurcationProfile`].
///
/// When [`inject_all`](Self::inject_all) is called, every registered fault is
/// injected in registration order and the results are returned as a `Vec`.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::network::{
///     NetworkFaultSuite, NetworkPartition, PacketLoss,
/// };
/// use malcolm::fault::FaultContext;
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let suite = NetworkFaultSuite::builder()
///     .name("chaos-01")
///     .profile(BifurcationProfile::network_partition())
///     .fault(Box::new(NetworkPartition::builder().seed(1).build()))
///     .fault(Box::new(PacketLoss::builder().seed(2).build()))
///     .build();
///
/// assert_eq!(suite.name(), "chaos-01");
/// assert_eq!(suite.len(), 2);
/// ```
pub struct NetworkFaultSuite {
    name: String,
    profile: BifurcationProfile,
    faults: Vec<Box<dyn Fault + Send + Sync>>,
}

impl NetworkFaultSuite {
    /// Begin constructing a [`NetworkFaultSuite`].
    #[must_use]
    pub fn builder() -> NetworkFaultSuiteBuilder {
        NetworkFaultSuiteBuilder::default()
    }

    /// The name of this suite.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The bifurcation profile governing the suite's stability regime.
    #[must_use]
    pub const fn profile(&self) -> &BifurcationProfile {
        &self.profile
    }

    /// Number of faults registered in the suite.
    #[must_use]
    pub fn len(&self) -> usize {
        self.faults.len()
    }

    /// Returns `true` if the suite contains no faults.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.faults.is_empty()
    }

    /// Inject all faults in registration order, returning the collected results.
    pub fn inject_all(&self, ctx: &FaultContext) -> Vec<FaultResult> {
        self.faults.iter().map(|f| f.inject(ctx)).collect()
    }
}

// ── NetworkFaultSuiteBuilder ──────────────────────────────────────────────────

/// Builder for [`NetworkFaultSuite`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `name`: `"default"`
/// - `profile`: [`BifurcationProfile::network_partition()`]
#[derive(Default)]
pub struct NetworkFaultSuiteBuilder {
    name: Option<String>,
    profile: Option<BifurcationProfile>,
    faults: Vec<Box<dyn Fault + Send + Sync>>,
}

impl NetworkFaultSuiteBuilder {
    /// Set the suite name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Set the bifurcation profile for this suite.
    #[must_use]
    pub const fn profile(mut self, profile: BifurcationProfile) -> Self {
        self.profile = Some(profile);
        self
    }

    /// Add a fault to the suite.
    #[must_use]
    pub fn fault(mut self, fault: Box<dyn Fault + Send + Sync>) -> Self {
        self.faults.push(fault);
        self
    }

    /// Consume the builder and produce a [`NetworkFaultSuite`].
    #[must_use]
    pub fn build(self) -> NetworkFaultSuite {
        NetworkFaultSuite {
            name: self.name.unwrap_or_else(|| "default".to_owned()),
            profile: self
                .profile
                .unwrap_or_else(BifurcationProfile::network_partition),
            faults: self.faults,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_test::traced_test;

    use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
    use malcolm_core::types::FaultResult;

    fn default_ctx(node_id: &str) -> FaultContext {
        FaultContext {
            seed: 42,
            timestamp_ms: 1_000,
            node_id: node_id.to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    #[traced_test]
    fn network_partition_inject_returns_injected_and_emits_event() {
        let fault = NetworkPartition::builder()
            .seed(42)
            .alpha(1.5)
            .intensity(0.7)
            .build();
        let ctx = default_ctx("node-0");
        let result = fault.inject(&ctx);
        assert!(matches!(result, FaultResult::Injected(_)));
        assert!(logs_contain("network partition injected"));
    }

    #[test]
    fn packet_loss_dry_run_would_inject() {
        let fault = PacketLoss::builder()
            .seed(7)
            .alpha(2.0)
            .x_min(1.0)
            .intensity(0.5)
            .build();
        let ctx = default_ctx("router-0");
        let report = fault.dry_run(&ctx);
        assert!(report.would_inject);
        assert_eq!(report.fault_type, "packet_loss");
    }

    #[test]
    fn latency_spike_samples_positive_over_many_seeds() {
        // LogNormal always produces positive values; verify this holds for 1000
        // distinct seeds covering the full seed space spread.
        for seed in 0_u64..1_000 {
            let fault = LatencySpike::builder()
                .base_ms(50.0)
                .sigma(0.3)
                .noise(NoiseType::Pink)
                .seed(seed)
                .build();
            let ms = fault.sample_latency_ms();
            assert!(
                ms > 0.0,
                "expected positive latency, got {ms} for seed {seed}"
            );
        }
    }

    #[test]
    fn bandwidth_throttle_inject_returns_injected() {
        let fault = BandwidthThrottle::builder()
            .seed(1)
            .bytes_per_sec(1_048_576)
            .intensity(0.9)
            .build();
        let ctx = default_ctx("edge-0");
        assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
    }

    #[test]
    fn network_partition_high_intensity_classifies_chaotic() {
        let profile = BifurcationProfile::network_partition();
        // intensity = 0.9 > threshold 0.6 + window/2 0.1 → Chaotic
        let fault = NetworkPartition::builder()
            .seed(42)
            .alpha(1.5)
            .intensity(0.9)
            .build();
        // Access the injected FaultEvent to retrieve the intensity used.
        let ctx = default_ctx("node-0");
        let FaultResult::Injected(event) = fault.inject(&ctx) else {
            panic!("expected Injected result");
        };
        let regime = classify(event.intensity, &profile);
        assert_eq!(regime, Regime::Chaotic);
    }
}
