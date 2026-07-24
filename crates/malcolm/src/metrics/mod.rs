//! In-process observability seam for malcolm scenarios.
//!
//! Every fault emission produces a structured `tracing` event, but downstream
//! exporters need numbers they can aggregate over time. This module defines
//! the in-process contract those exporters plug into: a [`MetricsRecorder`]
//! port, a [`MetricsHub`] fan-out, and the canonical metric taxonomy used by
//! the scenario layer.
//!
//! The seam is **always compiled in** and ships with a [`NoopRecorder`] that
//! drops every sample, so existing consumers observe identical behavior unless
//! they install a recorder explicitly. The companion exporters (Prometheus,
//! OpenTelemetry, `StatsD` — T27–T29) plug into this trait as feature-gated
//! crates; nothing in this module pulls a heavy dependency.
//!
//! # Taxonomy
//!
//! All metric names are `pub const` so exporters and tests share the same
//! string. Each entry's `kind` and `unit` are fixed by the contract below:
//!
//! | Name | Kind | Unit | Emitted when | Labels |
//! |------|------|------|--------------|--------|
//! | [`FAULTS_INJECTED_TOTAL`] | Counter | Count | a fault's `inject` returns `FaultResult::Injected` | `fault_type`, `node_id`, `scenario`, `regime`, `dry_run` |
//! | [`FAULTS_SKIPPED_TOTAL`] | Counter | Count | a fault's `inject` returns `FaultResult::Skipped` | `fault_type`, `node_id`, `scenario`, `skip_reason` |
//! | [`FAULT_INTENSITY`] | Gauge | Ratio | a fault is injected | `fault_type`, `node_id`, `scenario` |
//! | [`FAULT_LATENCY_MS`] | Histogram | Milliseconds | a fault reports a sampled latency (e.g. `latency_spike`) | `fault_type`, `node_id` |
//! | [`SCENARIO_DURATION_MS`] | Histogram | Milliseconds | a scenario finishes | `scenario`, `regime` |
//!
//! # Writing a custom recorder
//!
//! ```rust
//! use std::sync::{Arc, Mutex};
//! use malcolm::metrics::{MetricSample, MetricsRecorder};
//!
//! #[derive(Default)]
//! struct CollectingRecorder(Arc<Mutex<Vec<MetricSample>>>);
//!
//! impl MetricsRecorder for CollectingRecorder {
//!     fn record(&self, sample: &MetricSample) {
//!         let mut guard = match self.0.lock() {
//!             Ok(g) => g,
//!             Err(p) => p.into_inner(),
//!         };
//!         guard.push(sample.clone());
//!     }
//! }
//! ```

use std::sync::Arc;

use malcolm_core::types::{FaultEvent, SkipReason};

use crate::scenario::{ScenarioRegime, ScenarioReport};

// ── Taxonomy constants ───────────────────────────────────────────────────────

/// Counter incremented once per injected fault.
pub const FAULTS_INJECTED_TOTAL: &str = "malcolm_faults_injected_total";

/// Counter incremented once per skipped fault.
pub const FAULTS_SKIPPED_TOTAL: &str = "malcolm_faults_skipped_total";

/// Gauge reporting the latest observed intensity for a fault/node pair.
pub const FAULT_INTENSITY: &str = "malcolm_fault_intensity";

/// Histogram of fault-induced latencies (when the fault reports one).
pub const FAULT_LATENCY_MS: &str = "malcolm_fault_latency_ms";

/// Histogram of total scenario wall-clock duration.
pub const SCENARIO_DURATION_MS: &str = "malcolm_scenario_duration_ms";

// ── Sample types ─────────────────────────────────────────────────────────────

/// What kind of statistical series a sample belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricKind {
    /// Monotonically increasing cumulative count.
    Counter,
    /// Point-in-time value that may go up or down.
    Gauge,
    /// Distribution of observed values; exporters implement bucketing.
    Histogram,
}

/// Unit attached to a [`MetricSample::value`].
///
/// `f64` carries the value because exporters convert to their wire format
/// (Prometheus needs base units in seconds, `StatsD` takes ms, etc.). The unit
/// is informational at this layer — exporters should not auto-convert.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetricUnit {
    /// Dimensionless integer count.
    Count,
    /// Milliseconds.
    Milliseconds,
    /// Ratio in `[0.0, 1.0]`.
    Ratio,
    /// Bytes.
    Bytes,
}

/// One observation emitted by the scenario layer.
///
/// `name`, `kind`, and `unit` are governed by the taxonomy table; labels are
/// stable key/value strings. `timestamp_ms` is supplied by the caller so the
/// sample is replay-safe.
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSample {
    /// Canonical metric name (one of the `*_TOTAL`/`*_MS` constants).
    pub name: &'static str,
    /// Kind of series this sample belongs to.
    pub kind: MetricKind,
    /// Numeric value. Counters add, gauges replace, histograms observe.
    pub value: f64,
    /// Unit associated with `value`.
    pub unit: MetricUnit,
    /// Stable label set; keys are part of the taxonomy contract.
    pub labels: Vec<(&'static str, String)>,
    /// Caller-supplied wall-clock timestamp in milliseconds.
    pub timestamp_ms: u64,
}

// ── Recorder port ────────────────────────────────────────────────────────────

/// Sink that consumes metric samples.
///
/// Object-safe: implemented for [`NoopRecorder`], [`MetricsHub`], and any
/// downstream exporter.
pub trait MetricsRecorder: Send + Sync {
    /// Record one sample. Must not panic on any input.
    fn record(&self, sample: &MetricSample);
}

/// Recorder that drops every sample. The default everywhere.
///
/// Used as the stand-in when no exporter is installed, so the scenario layer
/// never has to branch on "is anything listening?".
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRecorder;

impl MetricsRecorder for NoopRecorder {
    #[inline]
    fn record(&self, _sample: &MetricSample) {}
}

// ── Fan-out hub ──────────────────────────────────────────────────────────────

/// Multi-recorder fan-out.
///
/// Use [`MetricsHub::new`] for an empty hub (no-op), or
/// [`MetricsHub::with_recorder`] / [`MetricsHub::with_recorders`] to chain
/// recorders. Each call to [`MetricsRecorder::record`] on the hub forwards
/// the sample to every registered recorder in registration order.
///
/// The hub itself implements [`MetricsRecorder`] so hubs nest.
#[derive(Clone, Default)]
pub struct MetricsHub {
    recorders: Vec<Arc<dyn MetricsRecorder>>,
}

impl std::fmt::Debug for MetricsHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MetricsHub")
            .field("recorder_count", &self.recorders.len())
            .finish_non_exhaustive()
    }
}

impl MetricsHub {
    /// Build an empty hub. Recording into an empty hub is a no-op.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style: start from `self` and append `recorder`.
    ///
    /// Preserves any previously registered recorders; the new one is appended
    /// to the end of the fan-out list.
    #[must_use]
    pub fn with_recorder(mut self, recorder: Arc<dyn MetricsRecorder>) -> Self {
        self.recorders.push(recorder);
        self
    }

    /// Builder-style: append every recorder in `recorders` to the fan-out.
    #[must_use]
    pub fn with_recorders(
        mut self,
        recorders: impl IntoIterator<Item = Arc<dyn MetricsRecorder>>,
    ) -> Self {
        self.recorders.extend(recorders);
        self
    }

    /// Number of registered recorders (useful for tests and diagnostics).
    #[must_use]
    pub fn recorder_count(&self) -> usize {
        self.recorders.len()
    }
}

impl MetricsRecorder for MetricsHub {
    fn record(&self, sample: &MetricSample) {
        for recorder in &self.recorders {
            recorder.record(sample);
        }
    }
}

// ── Scenario → samples translation ──────────────────────────────────────────

/// Translate a `FaultResult` into the metric samples it implies.
///
/// Pure function: no side effects, no recorder calls. The caller (scenario
/// layer) pushes the resulting samples through a [`MetricsHub`].
#[must_use]
pub fn samples_for_fault_result(
    fault: &FaultEvent,
    scenario_name: &str,
    regime: ScenarioRegime,
    timestamp_ms: u64,
) -> Vec<MetricSample> {
    let regime_label: &'static str = regime_label(regime);

    let mut samples = Vec::with_capacity(3);
    samples.push(MetricSample {
        name: FAULTS_INJECTED_TOTAL,
        kind: MetricKind::Counter,
        value: 1.0,
        unit: MetricUnit::Count,
        labels: vec![
            ("fault_type", fault.fault_type.clone()),
            ("node_id", fault.node_id.clone()),
            ("scenario", scenario_name.to_owned()),
            ("regime", regime_label.to_owned()),
            ("dry_run", fault.dry_run.to_string()),
        ],
        timestamp_ms,
    });
    samples.push(MetricSample {
        name: FAULT_INTENSITY,
        kind: MetricKind::Gauge,
        value: fault.intensity,
        unit: MetricUnit::Ratio,
        labels: vec![
            ("fault_type", fault.fault_type.clone()),
            ("node_id", fault.node_id.clone()),
            ("scenario", scenario_name.to_owned()),
        ],
        timestamp_ms,
    });
    samples
}

/// Translate a `FaultResult::Skipped` into a skipped-fault counter sample.
#[must_use]
pub fn sample_for_skipped_fault(
    fault_type: &'static str,
    node_id: &str,
    scenario_name: &str,
    reason: SkipReason,
    timestamp_ms: u64,
) -> MetricSample {
    MetricSample {
        name: FAULTS_SKIPPED_TOTAL,
        kind: MetricKind::Counter,
        value: 1.0,
        unit: MetricUnit::Count,
        labels: vec![
            ("fault_type", fault_type.to_owned()),
            ("node_id", node_id.to_owned()),
            ("scenario", scenario_name.to_owned()),
            ("skip_reason", skip_reason_label(reason).to_owned()),
        ],
        timestamp_ms,
    }
}

/// Translate a finished scenario report into a duration-histogram sample.
#[must_use]
pub fn sample_for_scenario_duration(report: &ScenarioReport) -> MetricSample {
    MetricSample {
        name: SCENARIO_DURATION_MS,
        kind: MetricKind::Histogram,
        value: f64::from(u32::try_from(report.total_duration_ms).unwrap_or(u32::MAX)),
        unit: MetricUnit::Milliseconds,
        labels: vec![
            ("scenario", report.name.clone()),
            ("regime", regime_label(report.regime).to_owned()),
        ],
        timestamp_ms: report.total_duration_ms,
    }
}

/// Translate a fault-reported latency (e.g. `latency_spike`) into a sample.
#[must_use]
pub fn sample_for_fault_latency(
    fault_type: &'static str,
    node_id: &str,
    latency_ms: f64,
    timestamp_ms: u64,
) -> MetricSample {
    MetricSample {
        name: FAULT_LATENCY_MS,
        kind: MetricKind::Histogram,
        value: latency_ms,
        unit: MetricUnit::Milliseconds,
        labels: vec![
            ("fault_type", fault_type.to_owned()),
            ("node_id", node_id.to_owned()),
        ],
        timestamp_ms,
    }
}

const fn regime_label(regime: ScenarioRegime) -> &'static str {
    match regime {
        ScenarioRegime::Stable => "stable",
        ScenarioRegime::Sensitive => "sensitive",
        ScenarioRegime::Chaotic => "chaotic",
    }
}

const fn skip_reason_label(reason: SkipReason) -> &'static str {
    match reason {
        SkipReason::BelowThreshold => "below_threshold",
        SkipReason::DryRun => "dry_run",
        SkipReason::Cancelled => "cancelled",
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::{DryRunReport, FaultEvent, FaultResult, SkipReason};

    use super::*;
    use crate::fault::{Fault, FaultContext};
    use crate::faults::network::PacketLoss;
    use crate::scenario::ChaosScenario;
    use crate::test_util::lock_or_recover;

    /// Test fault that always skips with `BelowThreshold`.
    struct AlwaysBelowThreshold;

    impl Fault for AlwaysBelowThreshold {
        fn inject(&self, _ctx: &FaultContext) -> FaultResult {
            FaultResult::Skipped(SkipReason::BelowThreshold)
        }
        fn dry_run(&self, _ctx: &FaultContext) -> DryRunReport {
            DryRunReport {
                fault_type: "always_below_threshold".to_owned(),
                node_id: String::new(),
                would_inject: false,
                reason: "below threshold".to_owned(),
            }
        }
        fn fault_type(&self) -> &'static str {
            "always_below_threshold"
        }
    }

    #[derive(Default)]
    struct CollectingRecorder(Arc<Mutex<Vec<MetricSample>>>);

    impl MetricsRecorder for CollectingRecorder {
        fn record(&self, sample: &MetricSample) {
            lock_or_recover(&self.0).push(sample.clone());
        }
    }

    fn sample_ctx(seed: u64) -> FaultContext {
        FaultContext {
            seed,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    fn count_name(samples: &[MetricSample], name: &str) -> usize {
        samples.iter().filter(|s| s.name == name).count()
    }

    #[test]
    fn noop_recorder_accepts_samples_without_panicking() {
        let rec = NoopRecorder;
        rec.record(&MetricSample {
            name: FAULTS_INJECTED_TOTAL,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });
    }

    #[test]
    fn empty_hub_records_nothing() {
        let hub = MetricsHub::new();
        let recorder = Arc::new(CollectingRecorder::default());
        // Smoke: nothing happens when no recorder is installed.
        hub.record(&MetricSample {
            name: FAULTS_INJECTED_TOTAL,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });
        assert_eq!(lock_or_recover(&recorder.0).len(), 0);
        assert_eq!(hub.recorder_count(), 0);
    }

    #[test]
    fn hub_fans_out_to_every_registered_recorder() {
        let a = Arc::new(CollectingRecorder::default());
        let b = Arc::new(CollectingRecorder::default());
        let hub = MetricsHub::new()
            .with_recorder(a.clone())
            .with_recorder(b.clone());
        hub.record(&MetricSample {
            name: FAULTS_INJECTED_TOTAL,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: MetricUnit::Count,
            labels: vec![("fault_type", "packet_loss".to_owned())],
            timestamp_ms: 0,
        });
        assert_eq!(lock_or_recover(&a.0).len(), 1);
        assert_eq!(lock_or_recover(&b.0).len(), 1);
        assert_eq!(hub.recorder_count(), 2);
    }

    #[test]
    fn scenario_emits_one_injected_counter_per_fault() {
        let recorder = Arc::new(CollectingRecorder::default());
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let scenario = ChaosScenario::builder()
            .name("metrics-wiring")
            .seed(1337)
            .add_fault(PacketLoss::builder().seed(42).intensity(0.9).build())
            .add_fault(PacketLoss::builder().seed(43).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx(1337);
        let _report = scenario.run_with_metrics(&mut ctx, &hub);

        let samples = lock_or_recover(&recorder.0).clone();
        assert_eq!(
            count_name(&samples, FAULTS_INJECTED_TOTAL),
            2,
            "expected one injected counter per fault, samples={samples:?}",
        );
        assert!(samples.iter().any(|s| s.name == SCENARIO_DURATION_MS));
        // Each injected counter carries the right labels.
        for s in samples.iter().filter(|s| s.name == FAULTS_INJECTED_TOTAL) {
            let label_map: std::collections::HashMap<_, _> =
                s.labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
            assert_eq!(label_map.get("fault_type").copied(), Some("packet_loss"));
            assert_eq!(label_map.get("scenario").copied(), Some("metrics-wiring"));
            assert_eq!(label_map.get("node_id").copied(), Some("node-0"));
            assert!(label_map.contains_key("regime"));
        }
    }

    #[test]
    fn scenario_emits_skipped_counter_with_reason_label() {
        let recorder = Arc::new(CollectingRecorder::default());
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        // `AlwaysBelowThreshold` deterministically skips — proves the
        // scenario wiring routes skipped-fault results through the metrics
        // hub with the correct `skip_reason` label.
        let scenario = ChaosScenario::builder()
            .name("skipped-only")
            .seed(1337)
            .add_fault(AlwaysBelowThreshold)
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx(1337);
        let _report = scenario.run_with_metrics(&mut ctx, &hub);

        let samples = lock_or_recover(&recorder.0).clone();
        let skipped: Vec<_> = samples
            .iter()
            .filter(|s| s.name == FAULTS_SKIPPED_TOTAL)
            .collect();
        assert_eq!(skipped.len(), 1);
        let Some(first) = skipped.first() else {
            unreachable!("expected one skipped sample, got none");
        };
        let label_map: std::collections::HashMap<_, _> =
            first.labels.iter().map(|(k, v)| (*k, v.as_str())).collect();
        assert_eq!(
            label_map.get("skip_reason").copied(),
            Some("below_threshold")
        );
        // Skipped faults must not produce injected counters.
        assert_eq!(count_name(&samples, FAULTS_INJECTED_TOTAL), 0);
    }

    #[test]
    fn run_without_hub_is_byte_identical_to_run_with_empty_hub() {
        let scenario = ChaosScenario::builder()
            .name("noop-compat")
            .seed(99)
            .add_fault(PacketLoss::builder().seed(7).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx_a = sample_ctx(99);
        let report_a = scenario.run(&mut ctx_a);

        let mut ctx_b = sample_ctx(99);
        let report_b = scenario.run_with_metrics(&mut ctx_b, &MetricsHub::new());

        // Stable fields must be identical: scenario name, seed, regime, event
        // count, and per-event content.
        assert_eq!(report_a.name, report_b.name);
        assert_eq!(report_a.seed, report_b.seed);
        assert_eq!(report_a.regime, report_b.regime);
        assert_eq!(report_a.events.len(), report_b.events.len());
        for (a, b) in report_a.events.iter().zip(report_b.events.iter()) {
            assert_eq!(a, b);
        }
    }

    #[test]
    fn samples_for_fault_result_produces_injected_and_intensity() {
        let event = FaultEvent {
            fault_type: "latency_spike".to_owned(),
            node_id: "node-1".to_owned(),
            seed: 7,
            intensity: 0.5,
            dry_run: false,
            timestamp_ms: 1234,
        };
        let samples = samples_for_fault_result(&event, "demo", ScenarioRegime::Sensitive, 1234);
        assert_eq!(samples.len(), 2);
        let Some(injected) = samples.first() else {
            unreachable!("expected first sample");
        };
        assert_eq!(injected.name, FAULTS_INJECTED_TOTAL);
        assert!((injected.value - 1.0).abs() < f64::EPSILON);
        let Some(intensity) = samples.get(1) else {
            unreachable!("expected second sample");
        };
        assert_eq!(intensity.name, FAULT_INTENSITY);
        assert!((intensity.value - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn sample_for_skipped_fault_carries_skip_reason() {
        let sample =
            sample_for_skipped_fault("memory_pressure", "node-2", "demo", SkipReason::DryRun, 99);
        assert_eq!(sample.name, FAULTS_SKIPPED_TOTAL);
        assert!(
            sample
                .labels
                .iter()
                .any(|(k, v)| *k == "skip_reason" && v == "dry_run")
        );
    }

    #[test]
    fn sample_for_scenario_duration_uses_report_label() {
        let report = ScenarioReport {
            name: "demo".to_owned(),
            seed: 1,
            regime: ScenarioRegime::Chaotic,
            events: Vec::new(),
            total_duration_ms: 250,
        };
        let sample = sample_for_scenario_duration(&report);
        assert_eq!(sample.name, SCENARIO_DURATION_MS);
        assert!((sample.value - 250.0).abs() < f64::EPSILON);
        assert!(
            sample
                .labels
                .iter()
                .any(|(k, v)| *k == "scenario" && v == "demo")
        );
        assert!(
            sample
                .labels
                .iter()
                .any(|(k, v)| *k == "regime" && v == "chaotic")
        );
    }
}

#[cfg(feature = "prometheus")]
pub mod prometheus;

#[cfg(feature = "otel")]
pub mod otel;

#[cfg(feature = "statsd")]
pub mod statsd;
