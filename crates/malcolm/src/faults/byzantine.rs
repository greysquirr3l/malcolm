//! Byzantine fault primitives: lies, partial responses, slow-correct.
//!
//! Implements three fault types for testing systems that must tolerate nodes
//! behaving incorrectly but not crashing. All faults operate on [`Vec<u8>`]
//! payloads to remain format-agnostic.
//!
//! # Primitives
//!
//! - [`LyingNode`]: corrupts a payload according to a [`CorruptionStrategy`].
//! - [`PartialResponse`]: truncates a payload at a [`Pareto`]-sampled byte offset.
//! - [`SlowCorrect`]: returns the correct payload after a [`LogNormal`]-sampled delay.
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults::byzantine::{LyingNode, CorruptionStrategy};
//! use malcolm::fault::{Fault, FaultContext};
//! use malcolm_core::bifurcation::BifurcationProfile;
//! use malcolm_core::types::FaultResult;
//!
//! let fault = LyingNode::builder()
//!     .seed(42)
//!     .payload(vec![0xAA, 0xBB, 0xCC, 0xDD])
//!     .strategy(CorruptionStrategy::StructuralCorruption { fill_byte: 0x00 })
//!     .byzantine_probability(1.0)
//!     .build();
//! let ctx = FaultContext {
//!     seed: 42,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::byzantine_node(),
//! };
//! assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
//! ```

use std::thread;
use std::time::Duration;

use rand::Rng as _;
use rand::SeedableRng as _;
use rand::rngs::SmallRng;

use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::distributions::{DistributionSampler as _, LogNormal, Pareto};
use malcolm_core::lyapunov::LyapunovScorer;
use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult};

use crate::fault::{Fault, FaultContext};

// ── CorruptionStrategy ────────────────────────────────────────────────────────

/// Describes how a [`LyingNode`] corrupts its payload.
///
/// All variants are deterministic given the fault's seed, enabling replay.
#[derive(Debug, Clone)]
pub enum CorruptionStrategy {
    /// Flip exactly `num_bits` distinct bits at seeded-random positions.
    BitFlip {
        /// Number of bits to flip (must be ≥ 1).
        num_bits: usize,
    },
    /// Splice `replacement` bytes into the payload starting at `offset`.
    ///
    /// Bytes at `offset..offset + replacement.len()` are overwritten. If
    /// `offset ≥ payload.len()` the payload is returned unchanged.
    FieldSubstitution {
        /// Byte index at which to begin the substitution.
        offset: usize,
        /// Replacement bytes to write at `offset`.
        replacement: Vec<u8>,
    },
    /// Overwrite the entire payload with `fill_byte` repeated for its original length.
    StructuralCorruption {
        /// Byte value to fill the corrupted payload with.
        fill_byte: u8,
    },
}

// ── ByzantineProfile ──────────────────────────────────────────────────────────

/// Controls what fraction of responses from a node are Byzantine.
///
/// Each call to [`is_byzantine`](Self::is_byzantine) draws from a uniform
/// distribution and returns `true` with probability `probability`.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::byzantine::ByzantineProfile;
/// use rand::SeedableRng;
/// use rand::rngs::SmallRng;
///
/// let mut rng = SmallRng::seed_from_u64(0);
/// let never = ByzantineProfile { probability: 0.0 };
/// assert!(!never.is_byzantine(&mut rng));
///
/// let always = ByzantineProfile { probability: 1.0 };
/// assert!(always.is_byzantine(&mut rng));
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ByzantineProfile {
    /// Probability (in `[0.0, 1.0]`) that a response is Byzantine.
    pub probability: f64,
}

impl ByzantineProfile {
    /// Return `true` with probability [`Self::probability`], sampled from `rng`.
    ///
    /// When `probability` is exactly `0.0` this always returns `false`; when
    /// exactly `1.0` it always returns `true`.
    #[must_use]
    pub fn is_byzantine(&self, rng: &mut SmallRng) -> bool {
        if self.probability <= 0.0 {
            return false;
        }
        if self.probability >= 1.0 {
            return true;
        }
        rng.r#gen::<f64>() < self.probability
    }
}

// ── LyingNode ─────────────────────────────────────────────────────────────────

/// Corrupts a byte-payload according to a [`CorruptionStrategy`].
///
/// The corruption is deterministic given `seed` — identical seeds always
/// produce identical corrupted outputs, enabling fault replay.
///
/// `byzantine_probability` controls how often the fault is applied; when the
/// draw from a uniform distribution exceeds the threshold the original payload
/// is returned via [`FaultResult::Injected`] unchanged (the "fault" is that
/// the node might lie, not that it always lies).
///
/// # Example
///
/// ```rust
/// use malcolm::faults::byzantine::{LyingNode, CorruptionStrategy};
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = LyingNode::builder()
///     .seed(1)
///     .payload(vec![0x01, 0x02, 0x03, 0x04])
///     .strategy(CorruptionStrategy::BitFlip { num_bits: 1 })
///     .byzantine_probability(1.0)
///     .build();
/// let ctx = FaultContext {
///     seed: 1,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::byzantine_node(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct LyingNode {
    seed: u64,
    payload: Vec<u8>,
    strategy: CorruptionStrategy,
    byzantine_probability: f64,
}

impl LyingNode {
    /// Begin constructing a [`LyingNode`] fault.
    #[must_use]
    pub fn builder() -> LyingNodeBuilder {
        LyingNodeBuilder::default()
    }

    /// Corrupt `data` according to `strategy` using `rng` for any randomness.
    ///
    /// The mutation is in-place on the returned `Vec<u8>` — the input is consumed.
    #[must_use]
    pub fn corrupt(&self, mut data: Vec<u8>, rng: &mut SmallRng) -> Vec<u8> {
        match &self.strategy {
            CorruptionStrategy::BitFlip { num_bits } => {
                if data.is_empty() {
                    return data;
                }
                let total_bits = data.len() * 8;
                let flips = (*num_bits).min(total_bits);

                // Select unique bit positions with a partial Fisher-Yates shuffle.
                let mut positions: Vec<usize> = (0..total_bits).collect();
                for i in 0..flips {
                    let j = i + (rng.r#gen::<usize>() % (total_bits - i));
                    positions.swap(i, j);
                    let Some(&bit_index) = positions.get(i) else {
                        continue;
                    };
                    let byte_index = bit_index / 8;
                    let bit_offset = bit_index % 8;
                    if let Some(b) = data.get_mut(byte_index) {
                        *b ^= 1 << bit_offset;
                    }
                }
                data
            }
            CorruptionStrategy::FieldSubstitution {
                offset,
                replacement,
            } => {
                let end = offset.saturating_add(replacement.len());
                if *offset >= data.len() {
                    return data;
                }
                let write_end = end.min(data.len());
                let src_end = write_end - offset;
                for (dst, src) in data
                    .get_mut(*offset..write_end)
                    .into_iter()
                    .flatten()
                    .zip(replacement.get(..src_end).into_iter().flatten())
                {
                    *dst = *src;
                }
                data
            }
            CorruptionStrategy::StructuralCorruption { fill_byte } => {
                data.fill(*fill_byte);
                data
            }
        }
    }

    /// Apply this Byzantine fault to an arbitrary response payload.
    ///
    /// Returns either the original response (non-Byzantine draw) or a corrupted
    /// response (Byzantine draw), based on [`ByzantineProfile`].
    #[must_use]
    pub fn respond(&self, response: Vec<u8>) -> Vec<u8> {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let profile = ByzantineProfile {
            probability: self.byzantine_probability,
        };

        if profile.is_byzantine(&mut rng) {
            self.corrupt(response, &mut rng)
        } else {
            response
        }
    }
}

impl Fault for LyingNode {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let data = self.respond(self.payload.clone());

        tracing::info!(
            target: "malcolm",
            fault_type = "lying_node",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.byzantine_probability,
            payload_len = data.len(),
            dry_run = false,
            "lying node fault injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "lying_node".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.byzantine_probability,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let reason = format!(
            "would corrupt {}-byte payload for node {} (strategy: {:?}, p={:.2})",
            self.payload.len(),
            ctx.node_id,
            self.strategy,
            self.byzantine_probability,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "lying_node",
            node_id = %ctx.node_id,
            seed = self.seed,
            payload_len = self.payload.len(),
            dry_run = true,
            "lying node dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "lying_node"
    }
}

// ── LyingNodeBuilder ──────────────────────────────────────────────────────────

/// Builder for [`LyingNode`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `payload`: empty `Vec<u8>`
/// - `strategy`: [`CorruptionStrategy::BitFlip { num_bits: 1 }`]
/// - `byzantine_probability`: `0.5`
#[derive(Default)]
pub struct LyingNodeBuilder {
    seed: Option<u64>,
    payload: Option<Vec<u8>>,
    strategy: Option<CorruptionStrategy>,
    byzantine_probability: Option<f64>,
}

impl LyingNodeBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the byte payload to corrupt.
    #[must_use]
    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Set the corruption strategy.
    #[must_use]
    pub fn strategy(mut self, strategy: CorruptionStrategy) -> Self {
        self.strategy = Some(strategy);
        self
    }

    /// Set the probability that a response is Byzantine (0.0–1.0).
    #[must_use]
    pub const fn byzantine_probability(mut self, p: f64) -> Self {
        self.byzantine_probability = Some(p);
        self
    }

    /// Consume the builder and produce a [`LyingNode`].
    #[must_use]
    pub fn build(self) -> LyingNode {
        LyingNode {
            seed: self.seed.unwrap_or(0),
            payload: self.payload.unwrap_or_default(),
            strategy: self
                .strategy
                .unwrap_or(CorruptionStrategy::BitFlip { num_bits: 1 }),
            byzantine_probability: self.byzantine_probability.unwrap_or(0.5),
        }
    }
}

// ── PartialResponse ───────────────────────────────────────────────────────────

/// Truncates a byte-payload at a [`Pareto`]-sampled byte offset.
///
/// The Pareto distribution produces heavy-tailed truncation offsets: most
/// truncations are minor (near the end of the payload) but the heavy tail
/// models occasional severe truncations, consistent with real network behaviour.
///
/// The effective truncation point is `min(pareto_sample as usize, data.len())`,
/// so the returned slice is always valid.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::byzantine::PartialResponse;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = PartialResponse::builder()
///     .seed(7)
///     .payload(vec![0u8; 128])
///     .alpha(2.0)
///     .x_min(1.0)
///     .build();
/// let ctx = FaultContext {
///     seed: 7,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::byzantine_node(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct PartialResponse {
    seed: u64,
    payload: Vec<u8>,
    alpha: f64,
    x_min: f64,
}

impl PartialResponse {
    /// Begin constructing a [`PartialResponse`] fault.
    #[must_use]
    pub fn builder() -> PartialResponseBuilder {
        PartialResponseBuilder::default()
    }

    /// Truncate `data` at a [`Pareto`]-sampled offset and return the prefix.
    ///
    /// The sampled value is cast to `usize` and clamped to `data.len()`, so
    /// the full payload is returned when the sample exceeds its length.
    #[must_use]
    pub fn truncate(&self, data: Vec<u8>, rng: &mut SmallRng) -> Vec<u8> {
        if data.is_empty() {
            return data;
        }

        let dist = Pareto {
            alpha: self.alpha,
            x_min: self.x_min,
        };
        let raw = dist.sample(rng);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to data.len() immediately below; truncation and sign-loss are safe here"
        )]
        let removed = (raw as usize).min(data.len());
        let keep = data.len().saturating_sub(removed);
        data.into_iter().take(keep).collect()
    }

    /// Apply truncation to an arbitrary response payload.
    #[must_use]
    pub fn respond(&self, response: Vec<u8>) -> Vec<u8> {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        self.truncate(response, &mut rng)
    }
}

impl Fault for PartialResponse {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let truncated = self.respond(self.payload.clone());

        tracing::info!(
            target: "malcolm",
            fault_type = "partial_response",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = 1.0_f64,
            original_len = self.payload.len(),
            truncated_len = truncated.len(),
            dry_run = false,
            "partial response fault injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "partial_response".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: 1.0,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let reason = format!(
            "would truncate {}-byte payload for node {} using Pareto(alpha={:.2}, x_min={:.2})",
            self.payload.len(),
            ctx.node_id,
            self.alpha,
            self.x_min,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "partial_response",
            node_id = %ctx.node_id,
            seed = self.seed,
            payload_len = self.payload.len(),
            dry_run = true,
            "partial response dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "partial_response"
    }
}

// ── PartialResponseBuilder ────────────────────────────────────────────────────

/// Builder for [`PartialResponse`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `payload`: empty `Vec<u8>`
/// - `alpha`: `2.0`
/// - `x_min`: `1.0`
#[derive(Default)]
pub struct PartialResponseBuilder {
    seed: Option<u64>,
    payload: Option<Vec<u8>>,
    alpha: Option<f64>,
    x_min: Option<f64>,
}

impl PartialResponseBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the byte payload to truncate.
    #[must_use]
    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Set the Pareto shape parameter (must be > 0; > 1 for finite mean).
    #[must_use]
    pub const fn alpha(mut self, alpha: f64) -> Self {
        self.alpha = Some(alpha);
        self
    }

    /// Set the Pareto scale parameter (minimum possible sample value).
    #[must_use]
    pub const fn x_min(mut self, x_min: f64) -> Self {
        self.x_min = Some(x_min);
        self
    }

    /// Consume the builder and produce a [`PartialResponse`].
    #[must_use]
    pub fn build(self) -> PartialResponse {
        PartialResponse {
            seed: self.seed.unwrap_or(0),
            payload: self.payload.unwrap_or_default(),
            alpha: self.alpha.unwrap_or(2.0),
            x_min: self.x_min.unwrap_or(1.0),
        }
    }
}

// ── SlowCorrect ───────────────────────────────────────────────────────────────

/// Returns the correct payload, but after a [`LogNormal`]-sampled delay.
///
/// This is the hardest Byzantine fault to detect: the content is correct, but
/// the delivery latency is wrong. Systems relying on timely responses (e.g.
/// consensus protocols with heartbeat timeouts) will misbehave.
///
/// The delay is sampled from `LogNormal(mu, sigma)` and applied as a blocking
/// sleep on the calling thread. For async environments, wrap the fault in a
/// task or use the optional tokio feature.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::byzantine::SlowCorrect;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// // Very short delay for testing (mu=-10 → ~0.000045ms)
/// let fault = SlowCorrect::builder()
///     .seed(3)
///     .payload(vec![0x01, 0x02])
///     .mu(-10.0)
///     .sigma(0.1)
///     .build();
/// let ctx = FaultContext {
///     seed: 3,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::byzantine_node(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct SlowCorrect {
    seed: u64,
    payload: Vec<u8>,
    mu: f64,
    sigma: f64,
}

impl SlowCorrect {
    /// Begin constructing a [`SlowCorrect`] fault.
    #[must_use]
    pub fn builder() -> SlowCorrectBuilder {
        SlowCorrectBuilder::default()
    }

    /// Sample delay in milliseconds from the configured log-normal model.
    #[must_use]
    pub fn sample_delay_ms(&self, rng: &mut SmallRng) -> f64 {
        LogNormal {
            mu: self.mu,
            sigma: self.sigma,
        }
        .sample(rng)
    }

    /// Return the correct response payload after a sampled delay.
    #[must_use]
    pub fn respond(&self, response: Vec<u8>) -> Vec<u8> {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let delay_ms = self.sample_delay_ms(&mut rng);

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "delay_ms is positive from LogNormal; truncation to millisecond sleep is intentional"
        )]
        let delay_u64 = delay_ms as u64;

        if delay_u64 > 0 {
            thread::sleep(Duration::from_millis(delay_u64));
        }

        response
    }
}

impl Fault for SlowCorrect {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let delay_ms = self.sample_delay_ms(&mut rng);

        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "delay_ms is a positive f64 from LogNormal; u64 truncation is intentional here"
        )]
        let delay_u64 = delay_ms as u64;

        if delay_u64 > 0 {
            thread::sleep(Duration::from_millis(delay_u64));
        }

        tracing::info!(
            target: "malcolm",
            fault_type = "slow_correct",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = 1.0_f64,
            delay_ms = delay_ms,
            payload_len = self.payload.len(),
            dry_run = false,
            "slow correct fault injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "slow_correct".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: 1.0,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        let dist = LogNormal {
            mu: self.mu,
            sigma: self.sigma,
        };
        let delay_ms = dist.sample(&mut rng);

        let reason = format!(
            "would delay response to node {} by ~{delay_ms:.2}ms (LogNormal mu={:.2}, sigma={:.2})",
            ctx.node_id, self.mu, self.sigma,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "slow_correct",
            node_id = %ctx.node_id,
            seed = self.seed,
            delay_ms = delay_ms,
            dry_run = true,
            "slow correct dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "slow_correct"
    }
}

// ── SlowCorrectBuilder ────────────────────────────────────────────────────────

/// Builder for [`SlowCorrect`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `seed`: `0`
/// - `payload`: empty `Vec<u8>`
/// - `mu`: `2.0`
/// - `sigma`: `0.5`
#[derive(Default)]
pub struct SlowCorrectBuilder {
    seed: Option<u64>,
    payload: Option<Vec<u8>>,
    mu: Option<f64>,
    sigma: Option<f64>,
}

impl SlowCorrectBuilder {
    /// Set the RNG seed.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the byte payload to deliver with a delay.
    #[must_use]
    pub fn payload(mut self, payload: Vec<u8>) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Set the log-normal mean parameter (log-space).
    #[must_use]
    pub const fn mu(mut self, mu: f64) -> Self {
        self.mu = Some(mu);
        self
    }

    /// Set the log-normal standard deviation parameter (log-space).
    #[must_use]
    pub const fn sigma(mut self, sigma: f64) -> Self {
        self.sigma = Some(sigma);
        self
    }

    /// Consume the builder and produce a [`SlowCorrect`].
    #[must_use]
    pub fn build(self) -> SlowCorrect {
        SlowCorrect {
            seed: self.seed.unwrap_or(0),
            payload: self.payload.unwrap_or_default(),
            mu: self.mu.unwrap_or(2.0),
            sigma: self.sigma.unwrap_or(0.5),
        }
    }
}

// ── ByzantineSuite ────────────────────────────────────────────────────────────

/// Bundles all three Byzantine fault primitives with a [`BifurcationProfile`].
///
/// Provides a [`sensitivity_score`](Self::sensitivity_score) method that
/// uses the [`LyapunovScorer`] to quantify how destabilising a given Byzantine
/// fraction is, mapping it to a Lyapunov exponent.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::byzantine::{ByzantineSuite, LyingNode, PartialResponse, SlowCorrect, CorruptionStrategy};
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let suite = ByzantineSuite::builder()
///     .name("byzantine-01")
///     .lying_node(
///         LyingNode::builder()
///             .seed(1)
///             .payload(vec![0xAA; 16])
///             .strategy(CorruptionStrategy::BitFlip { num_bits: 1 })
///             .byzantine_probability(0.3)
///             .build()
///     )
///     .partial_response(
///         PartialResponse::builder()
///             .seed(2)
///             .payload(vec![0u8; 64])
///             .alpha(2.0)
///             .x_min(1.0)
///             .build()
///     )
///     .slow_correct(
///         SlowCorrect::builder()
///             .seed(3)
///             .payload(vec![0x01, 0x02])
///             .mu(-10.0)
///             .sigma(0.1)
///             .build()
///     )
///     .build();
///
/// assert_eq!(suite.name(), "byzantine-01");
/// assert_eq!(suite.len(), 3);
/// let score = suite.sensitivity_score(3.9);
/// assert!(score > 0.0);
/// ```
#[derive(Debug)]
pub struct ByzantineSuite {
    name: String,
    profile: BifurcationProfile,
    lying_node: LyingNode,
    partial_response: PartialResponse,
    slow_correct: SlowCorrect,
}

impl ByzantineSuite {
    /// Begin constructing a [`ByzantineSuite`].
    #[must_use]
    pub fn builder() -> ByzantineSuiteBuilder {
        ByzantineSuiteBuilder::default()
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

    /// Number of faults in the suite (always 3).
    #[must_use]
    pub const fn len(&self) -> usize {
        3
    }

    /// Returns `false`; a [`ByzantineSuite`] always contains exactly three faults.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Compute the Lyapunov sensitivity score for a given Byzantine fraction.
    ///
    /// Maps `byzantine_fraction` in `[0, 1]` to a logistic-map control
    /// parameter `r` in `[1, 4]` and returns the Lyapunov exponent over 1000
    /// iterations. Positive values indicate a chaotic regime; negative values
    /// indicate stability.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::faults::byzantine::ByzantineSuite;
    ///
    /// let suite = ByzantineSuite::builder().build();
    /// assert!(suite.sensitivity_score(0.95) > 0.0);
    /// ```
    #[must_use]
    pub fn sensitivity_score(&self, byzantine_fraction: f64) -> f64 {
        let clamped = byzantine_fraction.clamp(0.0, 1.0);
        let r = clamped.mul_add(3.0, 1.0);
        LyapunovScorer::compute(r, 1000)
    }

    /// Inject all three faults in order and return the collected results.
    pub fn inject_all(&self, ctx: &FaultContext) -> Vec<FaultResult> {
        vec![
            self.lying_node.inject(ctx),
            self.partial_response.inject(ctx),
            self.slow_correct.inject(ctx),
        ]
    }
}

// ── ByzantineSuiteBuilder ─────────────────────────────────────────────────────

/// Builder for [`ByzantineSuite`].
///
/// Unset fields receive the following defaults at [`build`](Self::build):
/// - `name`: `"default"`
/// - `profile`: [`BifurcationProfile::byzantine_node()`]
/// - `lying_node`: default [`LyingNode`]
/// - `partial_response`: default [`PartialResponse`]
/// - `slow_correct`: default [`SlowCorrect`]
#[derive(Default)]
pub struct ByzantineSuiteBuilder {
    name: Option<String>,
    profile: Option<BifurcationProfile>,
    lying_node: Option<LyingNode>,
    partial_response: Option<PartialResponse>,
    slow_correct: Option<SlowCorrect>,
}

impl ByzantineSuiteBuilder {
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

    /// Set the lying node fault.
    #[must_use]
    pub fn lying_node(mut self, fault: LyingNode) -> Self {
        self.lying_node = Some(fault);
        self
    }

    /// Set the partial response fault.
    #[must_use]
    pub fn partial_response(mut self, fault: PartialResponse) -> Self {
        self.partial_response = Some(fault);
        self
    }

    /// Set the slow correct fault.
    #[must_use]
    pub fn slow_correct(mut self, fault: SlowCorrect) -> Self {
        self.slow_correct = Some(fault);
        self
    }

    /// Consume the builder and produce a [`ByzantineSuite`].
    #[must_use]
    pub fn build(self) -> ByzantineSuite {
        ByzantineSuite {
            name: self.name.unwrap_or_else(|| "default".to_owned()),
            profile: self
                .profile
                .unwrap_or_else(BifurcationProfile::byzantine_node),
            lying_node: self
                .lying_node
                .unwrap_or_else(|| LyingNode::builder().build()),
            partial_response: self
                .partial_response
                .unwrap_or_else(|| PartialResponse::builder().build()),
            slow_correct: self
                .slow_correct
                .unwrap_or_else(|| SlowCorrect::builder().build()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rand::SeedableRng as _;
    use rand::rngs::SmallRng;
    use tracing_test::traced_test;

    use super::*;
    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::FaultResult;

    fn default_ctx(node_id: &str) -> FaultContext {
        FaultContext {
            seed: 42,
            timestamp_ms: 1_000,
            node_id: node_id.to_owned(),
            profile: BifurcationProfile::byzantine_node(),
        }
    }

    // ── LyingNode ────────────────────────────────────────────────────────────

    #[test]
    fn lying_node_bitflip_flips_exactly_configured_bits() {
        let payload = vec![0xAA_u8, 0xBB, 0xCC, 0xDD];
        let num_bits = 6;
        let fault = LyingNode::builder()
            .seed(42)
            .payload(payload.clone())
            .strategy(CorruptionStrategy::BitFlip { num_bits })
            .byzantine_probability(1.0)
            .build();

        let corrupted = fault.respond(payload.clone());

        // Count differing bits between original and corrupted
        let differing_bits: u32 = payload
            .iter()
            .zip(corrupted.iter())
            .map(|(a, b)| (a ^ b).count_ones())
            .sum();

        #[expect(
            clippy::cast_possible_truncation,
            reason = "num_bits is tiny in this test"
        )]
        let expected = num_bits as u32;

        assert_eq!(
            differing_bits, expected,
            "expected exactly {num_bits} bit flips, got {differing_bits}"
        );
    }

    #[test]
    fn lying_node_field_substitution_patches_correct_bytes() {
        let payload = vec![0x00_u8, 0x01, 0x02, 0x03, 0x04];
        let fault = LyingNode::builder()
            .seed(1)
            .payload(payload.clone())
            .strategy(CorruptionStrategy::FieldSubstitution {
                offset: 2,
                replacement: vec![0xFF, 0xFE],
            })
            .byzantine_probability(1.0)
            .build();

        let mut rng = SmallRng::seed_from_u64(1);
        let corrupted = fault.corrupt(payload, &mut rng);

        assert_eq!(
            corrupted.get(2).copied(),
            Some(0xFF),
            "byte at offset 2 should be 0xFF"
        );
        assert_eq!(
            corrupted.get(3).copied(),
            Some(0xFE),
            "byte at offset 3 should be 0xFE"
        );
    }

    #[test]
    #[traced_test]
    fn lying_node_inject_returns_injected_and_logs() {
        let fault = LyingNode::builder()
            .seed(7)
            .payload(vec![0xDE, 0xAD, 0xBE, 0xEF])
            .strategy(CorruptionStrategy::StructuralCorruption { fill_byte: 0x00 })
            .byzantine_probability(1.0)
            .build();
        let ctx = default_ctx("node-0");

        let result = fault.inject(&ctx);

        assert!(
            matches!(result, FaultResult::Injected(_)),
            "expected Injected, got {result:?}"
        );
        assert!(logs_contain("lying node fault injected"));
    }

    // ── PartialResponse ──────────────────────────────────────────────────────

    #[test]
    fn partial_response_length_shows_pareto_shape_over_trials() {
        const TRIALS: usize = 1_000;
        const PAYLOAD_LEN: usize = 1_000;

        let payload = vec![0xAB_u8; PAYLOAD_LEN];
        let fault = PartialResponse::builder()
            .seed(99)
            .payload(payload.clone())
            .alpha(2.0)
            .x_min(1.0)
            .build();

        let mut lengths = Vec::with_capacity(TRIALS);
        for i in 0..TRIALS {
            let mut rng = SmallRng::seed_from_u64(i as u64);
            let truncated = fault.truncate(payload.clone(), &mut rng);
            lengths.push(truncated.len());
        }

        lengths.sort_unstable();

        let p50 = lengths.get(TRIALS / 2).copied().unwrap_or(PAYLOAD_LEN);
        let p10 = lengths.get(TRIALS / 10).copied().unwrap_or(PAYLOAD_LEN);
        let severe = lengths
            .iter()
            .filter(|&&len| len <= PAYLOAD_LEN - 10)
            .count();

        assert!(
            p50 >= (PAYLOAD_LEN * 9) / 10,
            "expected most responses to remain mostly intact; median length={p50}"
        );
        assert!(
            p10 < p50,
            "expected long-tail severe truncations; p10={p10}, p50={p50}"
        );
        assert!(
            severe > 0,
            "expected at least one non-trivial truncation in {TRIALS} Pareto trials"
        );
    }

    #[test]
    #[traced_test]
    fn partial_response_inject_returns_injected_and_logs() {
        let fault = PartialResponse::builder()
            .seed(5)
            .payload(vec![0u8; 256])
            .alpha(2.0)
            .x_min(1.0)
            .build();
        let ctx = default_ctx("node-1");

        let result = fault.inject(&ctx);

        assert!(
            matches!(result, FaultResult::Injected(_)),
            "expected Injected, got {result:?}"
        );
        assert!(logs_contain("partial response fault injected"));
    }

    // ── SlowCorrect ──────────────────────────────────────────────────────────

    #[test]
    #[traced_test]
    fn slow_correct_inject_returns_injected_with_correct_fault_type() {
        // Use very negative mu so the sampled delay rounds to 0ms (fast test)
        let fault = SlowCorrect::builder()
            .seed(11)
            .payload(vec![0x01, 0x02, 0x03])
            .mu(-20.0)
            .sigma(0.1)
            .build();
        let ctx = default_ctx("node-2");

        let result = fault.inject(&ctx);

        assert!(matches!(result, FaultResult::Injected(_)));
        assert_eq!(fault.fault_type(), "slow_correct");
        assert!(logs_contain("slow correct fault injected"));
    }

    #[test]
    fn slow_correct_returns_correct_response_content() {
        let response = vec![0x10, 0x20, 0x30, 0x40];
        let fault = SlowCorrect::builder()
            .seed(11)
            .payload(response.clone())
            .mu(-20.0)
            .sigma(0.1)
            .build();

        let returned = fault.respond(response.clone());
        assert_eq!(
            returned, response,
            "slow-correct must preserve payload bytes"
        );
    }

    #[test]
    fn slow_correct_delay_samples_follow_lognormal_shape() {
        const TRIALS: usize = 1_000;

        let fault = SlowCorrect::builder()
            .seed(123)
            .payload(vec![0x01])
            .mu(1.0)
            .sigma(0.8)
            .build();

        let mut samples = Vec::with_capacity(TRIALS);
        for i in 0..TRIALS {
            let mut rng = SmallRng::seed_from_u64(i as u64 + 10_000);
            samples.push(fault.sample_delay_ms(&mut rng));
        }

        let total: f64 = samples.iter().copied().sum();
        #[expect(clippy::cast_precision_loss, reason = "TRIALS is small and bounded")]
        let mean = total / TRIALS as f64;

        samples.sort_by(f64::total_cmp);
        let median = samples.get(TRIALS / 2).copied().unwrap_or(0.0);

        assert!(
            mean > median,
            "expected right-skewed LogNormal delays (mean > median), mean={mean}, median={median}"
        );
    }

    // ── ByzantineProfile ─────────────────────────────────────────────────────

    #[test]
    fn byzantine_profile_probability_converges_to_configured_fraction() {
        const TRIALS: usize = 10_000;

        let profile = ByzantineProfile { probability: 0.3 };
        let mut rng = SmallRng::seed_from_u64(0);
        let mut fired = 0usize;

        for _ in 0..TRIALS {
            if profile.is_byzantine(&mut rng) {
                fired = fired.saturating_add(1);
            }
        }

        #[expect(clippy::cast_precision_loss, reason = "values are small and bounded")]
        let observed = fired as f64 / TRIALS as f64;
        let err = (observed - profile.probability).abs();

        assert!(
            err < 0.03,
            "observed fraction {observed:.4} should converge near configured {}",
            profile.probability
        );
    }

    #[test]
    fn byzantine_profile_probability_zero_never_fires() {
        let profile = ByzantineProfile { probability: 0.0 };
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1_000 {
            assert!(
                !profile.is_byzantine(&mut rng),
                "probability=0.0 should never return true"
            );
        }
    }

    #[test]
    fn byzantine_profile_probability_one_always_fires() {
        let profile = ByzantineProfile { probability: 1.0 };
        let mut rng = SmallRng::seed_from_u64(0);
        for _ in 0..1_000 {
            assert!(
                profile.is_byzantine(&mut rng),
                "probability=1.0 should always return true"
            );
        }
    }

    // ── ByzantineSuite ───────────────────────────────────────────────────────

    #[test]
    fn byzantine_suite_sensitivity_score_chaotic_regime() {
        let suite = ByzantineSuite::builder().build();
        let score = suite.sensitivity_score(0.95);
        assert!(
            score > 0.0,
            "sensitivity score for byzantine_fraction=0.95 should be positive (chaotic regime), got {score}"
        );
    }

    #[test]
    fn byzantine_suite_len_is_three() {
        let suite = ByzantineSuite::builder().build();
        assert_eq!(suite.len(), 3);
        assert!(!suite.is_empty());
    }
}
