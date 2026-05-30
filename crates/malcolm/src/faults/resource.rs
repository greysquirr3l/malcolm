//! Resource fault layer: memory pressure, CPU throttle, I/O degradation.
//!
//! Provides three fault primitives for simulating infrastructure resource
//! degradation without requiring OS-level privileges (no cgroups, ulimit,
//! or kernel hooks required).
//!
//! # Primitives
//!
//! - [`MemoryPressure`]: allocates a `Vec<u8>` and holds it for a configurable
//!   duration, simulating memory pressure.
//! - [`CpuThrottle`]: spin-waits to consume CPU for a configurable fraction of
//!   a time window, simulating CPU saturation.
//! - [`IoDegradationWriter`] / [`IoDegradationReader`]: wrap [`Write`]/[`Read`]
//!   implementations and inject correlated [`PinkNoise`] sleep latency per call.
//!
//! [`MemoryPressure`] and [`CpuThrottle`] implement the [`Fault`] port trait.
//! The I/O degradation types are transport wrappers — they *are* the fault
//! mechanism and do not implement `Fault` directly.
//!
//! # Example
//!
//! ```rust
//! use std::io::Write as _;
//! use malcolm::faults::resource::{MemoryPressure, CpuThrottle, IoDegradationWriter};
//! use malcolm::fault::{Fault, FaultContext};
//! use malcolm_core::bifurcation::BifurcationProfile;
//! use malcolm_core::types::FaultResult;
//!
//! let mp = MemoryPressure::builder()
//!     .seed(1)
//!     .max_bytes(512)
//!     .duration_ms(0)
//!     .intensity(0.5)
//!     .build();
//! let ctx = FaultContext {
//!     seed: 1,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::memory_pressure(),
//! };
//! assert!(matches!(mp.inject(&ctx), FaultResult::Injected(_)));
//!
//! let mut writer = IoDegradationWriter::new(Vec::<u8>::new(), 99, 0, 0);
//! writer.write_all(b"hello").unwrap();
//! assert_eq!(writer.into_inner(), b"hello");
//! ```

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use rand::SeedableRng as _;
use rand::rngs::SmallRng;

use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::distributions::{DistributionSampler as _, PowerLaw};
use malcolm_core::noise::{PinkNoise, ScaledNoise};
use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult};

use crate::fault::{Fault, FaultContext};

// ── MemoryPressure ────────────────────────────────────────────────────────────

/// Simulates memory pressure by allocating and holding a `Vec<u8>` for a
/// configurable duration.
///
/// The allocation is `intensity × max_bytes` bytes. After holding the
/// allocation for `duration_ms`, it is dropped, making the fault
/// scoped and fully reversible.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::resource::MemoryPressure;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = MemoryPressure::builder()
///     .seed(42)
///     .max_bytes(1_024)
///     .duration_ms(0)
///     .intensity(0.5)
///     .build();
/// let ctx = FaultContext {
///     seed: 42,
///     timestamp_ms: 0,
///     node_id: "node-0".to_owned(),
///     profile: BifurcationProfile::memory_pressure(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct MemoryPressure {
    seed: u64,
    intensity: f64,
    max_bytes: usize,
    duration_ms: u64,
}

impl MemoryPressure {
    /// Begin constructing a [`MemoryPressure`] fault.
    #[must_use]
    pub fn builder() -> MemoryPressureBuilder {
        MemoryPressureBuilder::default()
    }
}

impl Fault for MemoryPressure {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        #[expect(
            clippy::cast_precision_loss,
            reason = "max_bytes will not exceed 2^53 bytes in realistic usage"
        )]
        let max_bytes_f = self.max_bytes as f64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "result is bounded by max_bytes, which was originally a usize"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "value is non-negative after max(0.0)"
        )]
        let bytes = (self.intensity * max_bytes_f).max(0.0) as usize;

        // Allocate and hold for the configured duration.
        let _allocation = vec![0u8; bytes];
        if self.duration_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.duration_ms));
        }
        // _allocation is dropped here, releasing the simulated pressure.

        tracing::info!(
            target: "malcolm",
            fault_type = "memory_pressure",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            bytes_allocated = bytes,
            dry_run = false,
            "memory pressure injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "memory_pressure".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.intensity,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        #[expect(
            clippy::cast_precision_loss,
            reason = "max_bytes will not exceed 2^53 bytes in realistic usage"
        )]
        let max_bytes_f = self.max_bytes as f64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "result is bounded by max_bytes, which was originally a usize"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "value is non-negative after max(0.0)"
        )]
        let bytes = (self.intensity * max_bytes_f).max(0.0) as usize;

        let reason = format!(
            "would allocate {} bytes ({:.1}% of {} byte ceiling) for {}ms on node {}",
            bytes,
            self.intensity * 100.0,
            self.max_bytes,
            self.duration_ms,
            ctx.node_id,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "memory_pressure",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.intensity,
            bytes_would_allocate = bytes,
            dry_run = true,
            "memory pressure dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "memory_pressure"
    }
}

// ── MemoryPressureBuilder ─────────────────────────────────────────────────────

/// Builder for [`MemoryPressure`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `intensity`: `1.0`
/// - `max_bytes`: `1_048_576` (1 MiB)
/// - `duration_ms`: `100`
#[derive(Debug, Default)]
pub struct MemoryPressureBuilder {
    seed: Option<u64>,
    intensity: Option<f64>,
    max_bytes: Option<usize>,
    duration_ms: Option<u64>,
}

impl MemoryPressureBuilder {
    /// Set the RNG seed for deterministic replay.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the normalised fault intensity in `[0.0, 1.0]`.
    ///
    /// The effective allocation is `intensity × max_bytes` bytes.
    #[must_use]
    pub const fn intensity(mut self, intensity: f64) -> Self {
        self.intensity = Some(intensity);
        self
    }

    /// Set the maximum allocation ceiling in bytes.
    ///
    /// Defaults to `1_048_576` (1 MiB) if not set.
    #[must_use]
    pub const fn max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// Set how long (milliseconds) to hold the allocation before dropping it.
    ///
    /// Pass `0` to drop immediately after allocation (useful in tests).
    #[must_use]
    pub const fn duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Consume the builder and produce a [`MemoryPressure`] fault.
    #[must_use]
    pub fn build(self) -> MemoryPressure {
        MemoryPressure {
            seed: self.seed.unwrap_or(0),
            intensity: self.intensity.unwrap_or(1.0),
            max_bytes: self.max_bytes.unwrap_or(1_048_576),
            duration_ms: self.duration_ms.unwrap_or(100),
        }
    }
}

// ── CpuThrottle ───────────────────────────────────────────────────────────────

/// Simulates CPU saturation by spin-waiting for `fraction × duration_ms`
/// wall-clock milliseconds.
///
/// A [`PowerLaw`] sample applies a bursty scale factor (capped at 2×) to the
/// effective spin window, producing heavy-tailed CPU usage events consistent
/// with real-world CPU pressure patterns.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::resource::CpuThrottle;
/// use malcolm::fault::{Fault, FaultContext};
/// use malcolm_core::bifurcation::BifurcationProfile;
/// use malcolm_core::types::FaultResult;
///
/// let fault = CpuThrottle::builder()
///     .seed(1)
///     .fraction(0.1)
///     .duration_ms(5)
///     .build();
/// let ctx = FaultContext {
///     seed: 1,
///     timestamp_ms: 0,
///     node_id: "worker-0".to_owned(),
///     profile: BifurcationProfile::memory_pressure(),
/// };
/// assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
/// ```
#[derive(Debug, Clone)]
pub struct CpuThrottle {
    seed: u64,
    fraction: f64,
    duration_ms: u64,
}

impl CpuThrottle {
    /// Begin constructing a [`CpuThrottle`] fault.
    #[must_use]
    pub fn builder() -> CpuThrottleBuilder {
        CpuThrottleBuilder::default()
    }
}

impl Fault for CpuThrottle {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let mut rng = SmallRng::seed_from_u64(self.seed);
        // Sample a burst factor from PowerLaw (minimum = 1.0), capped at 2× to
        // bound test duration while still producing heavy-tailed behaviour.
        let burst = PowerLaw::default().sample(&mut rng).min(2.0);

        #[expect(
            clippy::cast_precision_loss,
            reason = "duration_ms will not exceed 2^53 ms in realistic usage"
        )]
        let duration_f = self.duration_ms as f64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "spin_ms is bounded by 2 × duration_ms, which fits in u64"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "fraction and burst are non-negative; max(0.0) ensures no sign flip"
        )]
        let spin_ms = (self.fraction * burst * duration_f).max(0.0) as u64;

        let spin_duration = Duration::from_millis(spin_ms);
        let start = Instant::now();
        while start.elapsed() < spin_duration {
            std::hint::spin_loop();
        }

        tracing::info!(
            target: "malcolm",
            fault_type = "cpu_throttle",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.fraction,
            fraction = self.fraction,
            duration_ms = self.duration_ms,
            spin_ms = spin_ms,
            dry_run = false,
            "CPU throttle injected",
        );

        FaultResult::Injected(FaultEvent {
            fault_type: "cpu_throttle".to_owned(),
            node_id: ctx.node_id.clone(),
            seed: self.seed,
            intensity: self.fraction,
            dry_run: false,
            timestamp_ms: ctx.timestamp_ms,
        })
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        #[expect(
            clippy::cast_precision_loss,
            reason = "duration_ms will not exceed 2^53 ms in realistic usage"
        )]
        let duration_f = self.duration_ms as f64;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "spin_ms is bounded by duration_ms, which fits in u64"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "fraction is non-negative; max(0.0) ensures no sign flip"
        )]
        let spin_ms = (self.fraction * duration_f).max(0.0) as u64;

        let reason = format!(
            "would spin-wait for ~{}ms ({:.1}% of {}ms window) on node {}",
            spin_ms,
            self.fraction * 100.0,
            self.duration_ms,
            ctx.node_id,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "cpu_throttle",
            node_id = %ctx.node_id,
            seed = self.seed,
            intensity = self.fraction,
            fraction = self.fraction,
            duration_ms = self.duration_ms,
            dry_run = true,
            "CPU throttle dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: true,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "cpu_throttle"
    }
}

// ── CpuThrottleBuilder ────────────────────────────────────────────────────────

/// Builder for [`CpuThrottle`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `seed`: `0`
/// - `fraction`: `0.5`
/// - `duration_ms`: `100`
#[derive(Debug, Default)]
pub struct CpuThrottleBuilder {
    seed: Option<u64>,
    fraction: Option<f64>,
    duration_ms: Option<u64>,
}

impl CpuThrottleBuilder {
    /// Set the RNG seed for deterministic replay.
    #[must_use]
    pub const fn seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }

    /// Set the CPU spin fraction of the duration window (`0.0..=1.0`).
    ///
    /// A value of `0.5` with `duration_ms = 100` will spin for ~50 ms.
    #[must_use]
    pub const fn fraction(mut self, fraction: f64) -> Self {
        self.fraction = Some(fraction);
        self
    }

    /// Set the total time window in milliseconds.
    #[must_use]
    pub const fn duration_ms(mut self, duration_ms: u64) -> Self {
        self.duration_ms = Some(duration_ms);
        self
    }

    /// Consume the builder and produce a [`CpuThrottle`] fault.
    #[must_use]
    pub fn build(self) -> CpuThrottle {
        CpuThrottle {
            seed: self.seed.unwrap_or(0),
            fraction: self.fraction.unwrap_or(0.5),
            duration_ms: self.duration_ms.unwrap_or(100),
        }
    }
}

// ── IoDegradationWriter ───────────────────────────────────────────────────────

/// Wraps a [`Write`] implementation and injects correlated sleep latency before
/// each `write` call.
///
/// Latency values are drawn from a [`PinkNoise`] source scaled to
/// `[min_ms, max_ms]`, producing realistic correlated I/O jitter without
/// corrupting the underlying data stream.
///
/// **Note:** This type is a fault mechanism, not a [`Fault`] implementor.
/// Wrap an I/O target at construction time to inject degradation transparently.
///
/// # Example
///
/// ```rust
/// use std::io::Write as _;
/// use malcolm::faults::resource::IoDegradationWriter;
///
/// let mut writer = IoDegradationWriter::new(Vec::<u8>::new(), 42, 0, 1);
/// writer.write_all(b"hello").unwrap();
/// assert_eq!(writer.into_inner(), b"hello");
/// ```
pub struct IoDegradationWriter<W> {
    inner: W,
    noise: ScaledNoise<PinkNoise>,
    seed: u64,
}

impl<W: Write> IoDegradationWriter<W> {
    /// Wrap `inner` and configure latency injection between `min_ms` and `max_ms`.
    ///
    /// Uses `seed` to initialise the [`PinkNoise`] source for deterministic replay.
    #[must_use]
    pub fn new(inner: W, seed: u64, min_ms: u64, max_ms: u64) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "min_ms and max_ms will not exceed 2^53 ms in realistic usage"
        )]
        let (min_f, max_f) = (min_ms as f64, max_ms as f64);
        Self {
            inner,
            noise: ScaledNoise::new(PinkNoise::new(seed), min_f, max_f),
            seed,
        }
    }

    /// Consume the wrapper and return the inner writer.
    #[must_use]
    pub fn into_inner(self) -> W {
        self.inner
    }
}

impl<W: Write> Write for IoDegradationWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let latency_ms = self.noise.next().unwrap_or(0.0).max(0.0);

        tracing::debug!(
            target: "malcolm",
            fault_type = "io_degradation",
            direction = "write",
            seed = self.seed,
            latency_ms = latency_ms,
            bytes = buf.len(),
            dry_run = false,
            "I/O degradation write latency injected",
        );

        #[expect(
            clippy::cast_possible_truncation,
            reason = "latency_ms is bounded by max_ms, which was originally a u64"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "latency_ms is non-negative after max(0.0)"
        )]
        std::thread::sleep(Duration::from_millis(latency_ms as u64));
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

// ── IoDegradationReader ───────────────────────────────────────────────────────

/// Wraps a [`Read`] implementation and injects correlated sleep latency before
/// each `read` call.
///
/// Latency values are drawn from a [`PinkNoise`] source scaled to
/// `[min_ms, max_ms]`, producing realistic correlated I/O jitter.
///
/// # Example
///
/// ```rust
/// use std::io::{Cursor, Read as _};
/// use malcolm::faults::resource::IoDegradationReader;
///
/// let inner = Cursor::new(b"hello".to_vec());
/// let mut reader = IoDegradationReader::new(inner, 42, 0, 1);
/// let mut buf = [0u8; 5];
/// reader.read_exact(&mut buf).unwrap();
/// assert_eq!(&buf, b"hello");
/// ```
pub struct IoDegradationReader<R> {
    inner: R,
    noise: ScaledNoise<PinkNoise>,
    seed: u64,
}

impl<R: Read> IoDegradationReader<R> {
    /// Wrap `inner` and configure latency injection between `min_ms` and `max_ms`.
    ///
    /// Uses `seed` to initialise the [`PinkNoise`] source for deterministic replay.
    #[must_use]
    pub fn new(inner: R, seed: u64, min_ms: u64, max_ms: u64) -> Self {
        #[expect(
            clippy::cast_precision_loss,
            reason = "min_ms and max_ms will not exceed 2^53 ms in realistic usage"
        )]
        let (min_f, max_f) = (min_ms as f64, max_ms as f64);
        Self {
            inner,
            noise: ScaledNoise::new(PinkNoise::new(seed), min_f, max_f),
            seed,
        }
    }

    /// Consume the wrapper and return the inner reader.
    #[must_use]
    pub fn into_inner(self) -> R {
        self.inner
    }
}

impl<R: Read> Read for IoDegradationReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let latency_ms = self.noise.next().unwrap_or(0.0).max(0.0);

        tracing::debug!(
            target: "malcolm",
            fault_type = "io_degradation",
            direction = "read",
            seed = self.seed,
            latency_ms = latency_ms,
            bytes = buf.len(),
            dry_run = false,
            "I/O degradation read latency injected",
        );

        #[expect(
            clippy::cast_possible_truncation,
            reason = "latency_ms is bounded by max_ms, which was originally a u64"
        )]
        #[expect(
            clippy::cast_sign_loss,
            reason = "latency_ms is non-negative after max(0.0)"
        )]
        std::thread::sleep(Duration::from_millis(latency_ms as u64));
        self.inner.read(buf)
    }
}

// ── ResourceFaultSuite ────────────────────────────────────────────────────────

/// A bundled pair of resource faults with an associated [`BifurcationProfile`].
///
/// Combines [`MemoryPressure`] and [`CpuThrottle`] for composite resource stress
/// scenarios. [`inject_all`](Self::inject_all) injects both faults in order and
/// returns their results.
///
/// # Example
///
/// ```rust
/// use malcolm::faults::resource::{
///     ResourceFaultSuite, MemoryPressure, CpuThrottle,
/// };
/// use malcolm_core::bifurcation::BifurcationProfile;
///
/// let suite = ResourceFaultSuite::builder()
///     .name("resource-chaos-01")
///     .memory_pressure(
///         MemoryPressure::builder().seed(1).max_bytes(512).duration_ms(0).intensity(0.5).build()
///     )
///     .cpu_throttle(
///         CpuThrottle::builder().seed(2).fraction(0.1).duration_ms(1).build()
///     )
///     .build();
/// assert_eq!(suite.name(), "resource-chaos-01");
/// assert_eq!(suite.len(), 2);
/// ```
#[derive(Debug)]
pub struct ResourceFaultSuite {
    name: String,
    profile: BifurcationProfile,
    memory_pressure: MemoryPressure,
    cpu_throttle: CpuThrottle,
}

impl ResourceFaultSuite {
    /// Begin constructing a [`ResourceFaultSuite`].
    #[must_use]
    pub fn builder() -> ResourceFaultSuiteBuilder {
        ResourceFaultSuiteBuilder::default()
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

    /// Number of faults in the suite (always 2: memory pressure + CPU throttle).
    #[must_use]
    pub const fn len(&self) -> usize {
        2
    }

    /// Returns `false`; a [`ResourceFaultSuite`] always contains exactly two faults.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Inject both resource faults in order, returning the collected results.
    pub fn inject_all(&self, ctx: &FaultContext) -> Vec<FaultResult> {
        vec![
            self.memory_pressure.inject(ctx),
            self.cpu_throttle.inject(ctx),
        ]
    }
}

// ── ResourceFaultSuiteBuilder ─────────────────────────────────────────────────

/// Builder for [`ResourceFaultSuite`].
///
/// Unset fields receive the following defaults at [`build()`](Self::build):
/// - `name`: `"default"`
/// - `profile`: [`BifurcationProfile::memory_pressure()`]
/// - `memory_pressure`: default [`MemoryPressure`] (1 MiB ceiling, 100 ms hold)
/// - `cpu_throttle`: default [`CpuThrottle`] (50% fraction, 100 ms window)
#[derive(Debug, Default)]
pub struct ResourceFaultSuiteBuilder {
    name: Option<String>,
    profile: Option<BifurcationProfile>,
    memory_pressure: Option<MemoryPressure>,
    cpu_throttle: Option<CpuThrottle>,
}

impl ResourceFaultSuiteBuilder {
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

    /// Set the memory pressure fault.
    #[must_use]
    pub const fn memory_pressure(mut self, mp: MemoryPressure) -> Self {
        self.memory_pressure = Some(mp);
        self
    }

    /// Set the CPU throttle fault.
    #[must_use]
    pub const fn cpu_throttle(mut self, ct: CpuThrottle) -> Self {
        self.cpu_throttle = Some(ct);
        self
    }

    /// Consume the builder and produce a [`ResourceFaultSuite`].
    #[must_use]
    pub fn build(self) -> ResourceFaultSuite {
        ResourceFaultSuite {
            name: self.name.unwrap_or_else(|| "default".to_owned()),
            profile: self
                .profile
                .unwrap_or_else(BifurcationProfile::memory_pressure),
            memory_pressure: self
                .memory_pressure
                .unwrap_or_else(|| MemoryPressure::builder().build()),
            cpu_throttle: self
                .cpu_throttle
                .unwrap_or_else(|| CpuThrottle::builder().build()),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read as _, Write as _};

    use tracing_test::traced_test;

    use super::*;
    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::FaultResult;

    fn default_ctx(node_id: &str) -> FaultContext {
        FaultContext {
            seed: 42,
            timestamp_ms: 1_000,
            node_id: node_id.to_owned(),
            profile: BifurcationProfile::memory_pressure(),
        }
    }

    // ── MemoryPressure ───────────────────────────────────────────────────────

    #[test]
    #[traced_test]
    fn memory_pressure_inject_returns_injected_with_correct_intensity() {
        let fault = MemoryPressure::builder()
            .seed(42)
            .max_bytes(1_024)
            .duration_ms(0)
            .intensity(0.5)
            .build();
        let ctx = default_ctx("node-0");

        let result = fault.inject(&ctx);

        assert!(matches!(result, FaultResult::Injected(_)));
        let FaultResult::Injected(event) = result else {
            return;
        };
        assert!(
            (event.intensity - 0.5).abs() < f64::EPSILON,
            "intensity mismatch: {}",
            event.intensity,
        );
        assert_eq!(event.fault_type, "memory_pressure");
        assert!(logs_contain("memory pressure injected"));
    }

    #[test]
    #[traced_test]
    fn memory_pressure_dry_run_does_not_allocate() {
        // With tiny max_bytes; if dry_run allocated, the test would still
        // succeed (no allocation is detectable without sanitizers), but the
        // DryRunReport must confirm would_inject = true and no side-effect log.
        let fault = MemoryPressure::builder()
            .seed(1)
            .max_bytes(1)
            .duration_ms(0)
            .intensity(1.0)
            .build();
        let ctx = default_ctx("dry-node");

        let report = fault.dry_run(&ctx);

        assert!(report.would_inject);
        assert_eq!(report.fault_type, "memory_pressure");
        assert_eq!(report.node_id, "dry-node");
        assert!(logs_contain("memory pressure dry-run"));
        // Confirm no inject event was emitted.
        assert!(!logs_contain("memory pressure injected"));
    }

    // ── CpuThrottle ──────────────────────────────────────────────────────────

    #[test]
    #[traced_test]
    fn cpu_throttle_inject_completes_and_returns_injected() {
        let fault = CpuThrottle::builder()
            .seed(99)
            .fraction(0.1)
            .duration_ms(10)
            .build();
        let ctx = default_ctx("worker-0");

        let result = fault.inject(&ctx);

        assert!(matches!(result, FaultResult::Injected(_)));
        assert!(logs_contain("CPU throttle injected"));
    }

    // ── IoDegradationWriter ──────────────────────────────────────────────────

    #[test]
    fn io_degradation_writer_preserves_data() {
        // min_ms = max_ms = 0 so no sleep occurs in tests.
        let mut writer = IoDegradationWriter::new(Vec::<u8>::new(), 7, 0, 0);
        assert!(writer.write_all(b"hello").is_ok());
        let output = writer.into_inner();
        assert_eq!(output, b"hello");
    }

    #[test]
    fn io_degradation_reader_preserves_data() {
        let inner = Cursor::new(b"world".to_vec());
        let mut reader = IoDegradationReader::new(inner, 7, 0, 0);
        let mut buf = [0u8; 5];
        assert!(reader.read_exact(&mut buf).is_ok());
        assert_eq!(&buf, b"world");
    }

    // ── ResourceFaultSuite ───────────────────────────────────────────────────

    #[test]
    fn resource_fault_suite_builds_without_panic() {
        let suite = ResourceFaultSuite::builder()
            .name("test-suite")
            .memory_pressure(
                MemoryPressure::builder()
                    .seed(1)
                    .max_bytes(512)
                    .duration_ms(0)
                    .intensity(0.25)
                    .build(),
            )
            .cpu_throttle(
                CpuThrottle::builder()
                    .seed(2)
                    .fraction(0.05)
                    .duration_ms(1)
                    .build(),
            )
            .build();

        assert_eq!(suite.name(), "test-suite");
        assert_eq!(suite.len(), 2);
        assert!(!suite.is_empty());
    }
}
