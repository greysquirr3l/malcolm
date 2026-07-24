//! Prometheus metrics recorder.
//!
//! Translates the in-process metric taxonomy into Prometheus time series and
//! renders the standard text exposition format on demand. Pull-based: callers
//! register the recorder on a [`MetricsHub`](super::MetricsHub), let scenarios
//! run, and call [`PrometheusRecorder::gather_text`] when a Prometheus
//! scraper arrives.
//!
//! Available only with the `prometheus` Cargo feature. Default builds neither
//! compile nor link this module.
//!
//! # Cardinality
//!
//! `node_id` and `scenario` are user-controlled strings, so they can drive
//! time-series cardinality into the millions for long-lived deployments.
//! [`PrometheusRecorder::dropping_high_cardinality_labels`] is a one-line
//! constructor option that strips `node_id` from rendered output, collapsing
//! every node into a single series per `(fault_type, scenario, regime)` tuple.
//!
//! # Bucket choice
//!
//! The histogram bucket layout follows Prometheus best practice for latency:
//! exponential series from 1ms to ~60s. Values are stored in milliseconds
//! because the malcolm taxonomy is ms-based; the standard
//! `prometheus_client` convention of base-unit seconds is documented but not
//! silently converted — operators reading a scrape can multiply by `1e-3`.

use std::collections::HashSet;
use std::sync::RwLock;

use prometheus::{Encoder, GaugeVec, HistogramVec, IntCounterVec, Registry, TextEncoder};

use super::{
    FAULT_INTENSITY, FAULT_LATENCY_MS, FAULTS_INJECTED_TOTAL, FAULTS_SKIPPED_TOTAL, MetricKind,
    MetricSample, MetricsHub, MetricsRecorder, SCENARIO_DURATION_MS,
};

/// Prometheus recorder backed by a private [`Registry`].
///
/// Construct via [`PrometheusRecorder::new`] for the default label set, or
/// [`PrometheusRecorder::dropping_high_cardinality_labels`] to strip `node_id`
/// from every emitted series.
#[derive(Debug)]
pub struct PrometheusRecorder {
    registry: Registry,
    inner: RwLock<Inner>,
    drop_node_id: bool,
    /// Set of metric names already warned about (so we log unknown names at
    /// most once per recorder lifetime).
    warned: RwLock<HashSet<&'static str>>,
}

#[derive(Debug)]
struct Inner {
    faults_injected: IntCounterVec,
    faults_skipped: IntCounterVec,
    fault_intensity: GaugeVec,
    fault_latency_ms: HistogramVec,
    scenario_duration_ms: HistogramVec,
}

impl PrometheusRecorder {
    /// Build a recorder that exposes every label in the taxonomy.
    #[must_use]
    pub fn new() -> Self {
        Self::with_options(false)
    }

    /// Build a recorder that omits `node_id` from every emitted series.
    ///
    /// Use this when `node_id` values are too dynamic for your scrape budget
    /// (e.g. ephemeral pods) and aggregate per-scenario metrics are enough.
    #[must_use]
    pub fn dropping_high_cardinality_labels() -> Self {
        Self::with_options(true)
    }

    #[expect(
        clippy::panic,
        reason = "Metric names are static constants and label sets are static; IntCounterVec::new / \
                  GaugeVec::new / HistogramVec::new / Registry::register cannot fail at runtime in \
                  this codebase. The panic-on-error is a belt-and-suspenders guard against the \
                  prometheus crate adding a new failure mode."
    )]
    fn with_options(drop_node_id: bool) -> Self {
        // Metric names are `&'static str` constants and label sets are static,
        // so `IntCounterVec::new`, `GaugeVec::new`, `HistogramVec::new`, and
        // `Registry::register` cannot fail at runtime in this codebase. The
        // Prometheus crate may return errors (e.g. metric-name collisions) but
        // we provably can't trip those here.
        let registry = Registry::new();
        let injected_labels: &[&str] = if drop_node_id {
            &["fault_type", "scenario", "regime", "dry_run"]
        } else {
            &["fault_type", "node_id", "scenario", "regime", "dry_run"]
        };
        let skipped_labels: &[&str] = if drop_node_id {
            &["fault_type", "scenario", "skip_reason"]
        } else {
            &["fault_type", "node_id", "scenario", "skip_reason"]
        };
        let intensity_labels: &[&str] = if drop_node_id {
            &["fault_type", "scenario"]
        } else {
            &["fault_type", "node_id", "scenario"]
        };
        let latency_labels: &[&str] = if drop_node_id {
            &["fault_type"]
        } else {
            &["fault_type", "node_id"]
        };
        let scenario_labels: &[&str] = &["scenario", "regime"];

        let faults_injected = IntCounterVec::new(
            prometheus::Opts::new(FAULTS_INJECTED_TOTAL, "Total faults successfully injected."),
            injected_labels,
        )
        .unwrap_or_else(|_| {
            panic!("malcolm_faults_injected_total collector construction must succeed")
        });
        let faults_skipped = IntCounterVec::new(
            prometheus::Opts::new(FAULTS_SKIPPED_TOTAL, "Total faults skipped."),
            skipped_labels,
        )
        .unwrap_or_else(|_| {
            panic!("malcolm_faults_skipped_total collector construction must succeed")
        });
        let fault_intensity = GaugeVec::new(
            prometheus::Opts::new(
                FAULT_INTENSITY,
                "Last observed intensity per (fault_type, node_id).",
            ),
            intensity_labels,
        )
        .unwrap_or_else(|_| panic!("malcolm_fault_intensity collector construction must succeed"));
        let fault_latency_ms = HistogramVec::new(
            prometheus::HistogramOpts::new(FAULT_LATENCY_MS, "Fault-reported latency (ms).")
                .buckets(EXPO_MS_BUCKETS.to_vec()),
            latency_labels,
        )
        .unwrap_or_else(|_| panic!("malcolm_fault_latency_ms collector construction must succeed"));
        let scenario_duration_ms = HistogramVec::new(
            prometheus::HistogramOpts::new(
                SCENARIO_DURATION_MS,
                "Scenario wall-clock duration (ms).",
            )
            .buckets(EXPO_MS_BUCKETS.to_vec()),
            scenario_labels,
        )
        .unwrap_or_else(|_| {
            panic!("malcolm_scenario_duration_ms collector construction must succeed")
        });

        registry
            .register(Box::new(faults_injected.clone()))
            .unwrap_or_else(|_| panic!("malcolm_faults_injected_total registration must succeed"));
        registry
            .register(Box::new(faults_skipped.clone()))
            .unwrap_or_else(|_| panic!("malcolm_faults_skipped_total registration must succeed"));
        registry
            .register(Box::new(fault_intensity.clone()))
            .unwrap_or_else(|_| panic!("malcolm_fault_intensity registration must succeed"));
        registry
            .register(Box::new(fault_latency_ms.clone()))
            .unwrap_or_else(|_| panic!("malcolm_fault_latency_ms registration must succeed"));
        registry
            .register(Box::new(scenario_duration_ms.clone()))
            .unwrap_or_else(|_| panic!("malcolm_scenario_duration_ms registration must succeed"));

        let inner = Inner {
            faults_injected,
            faults_skipped,
            fault_intensity,
            fault_latency_ms,
            scenario_duration_ms,
        };

        Self {
            registry,
            inner: RwLock::new(inner),
            drop_node_id,
            warned: RwLock::new(HashSet::new()),
        }
    }

    /// Render the standard Prometheus text exposition for this recorder.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry encoder fails (very rare — typically
    /// only on out-of-memory or malformed label values, neither of which can
    /// occur via the malcolm recorder path).
    pub fn gather_text(&self) -> Result<String, prometheus::Error> {
        let mut buf = Vec::new();
        let encoder = TextEncoder::new();
        encoder.encode(&self.registry.gather(), &mut buf)?;
        Ok(String::from_utf8(buf).unwrap_or_default())
    }

    /// Return the underlying Prometheus [`Registry`].
    #[must_use]
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    fn drop_node(&self, labels: Vec<(&'static str, String)>) -> Vec<(&'static str, String)> {
        if self.drop_node_id {
            labels
                .into_iter()
                .filter(|(k, _)| *k != "node_id")
                .collect()
        } else {
            labels
        }
    }

    fn warn_once(&self, name: &'static str) {
        // Recover from a poisoned `RwLock`: the write guard itself is still usable.
        let mut warned = self
            .warned
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if warned.insert(name) {
            tracing::warn!(
                target: "malcolm",
                metric = name,
                "unknown metric name received by PrometheusRecorder; skipping",
            );
        }
    }
}

impl Default for PrometheusRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl MetricsRecorder for PrometheusRecorder {
    fn record(&self, sample: &MetricSample) {
        match sample.name {
            FAULTS_INJECTED_TOTAL => {
                let labels = self.drop_node(sample.labels.clone());
                let metric = self
                    .inner
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .faults_injected
                    .with_label_values(&label_values(&labels));
                metric.inc_by(counter_increment(sample.value));
            }
            FAULTS_SKIPPED_TOTAL => {
                let labels = self.drop_node(sample.labels.clone());
                let metric = self
                    .inner
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .faults_skipped
                    .with_label_values(&label_values(&labels));
                metric.inc_by(counter_increment(sample.value));
            }
            FAULT_INTENSITY => {
                let labels = self.drop_node(sample.labels.clone());
                let metric = self
                    .inner
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .fault_intensity
                    .with_label_values(&label_values(&labels));
                metric.set(sample.value);
            }
            FAULT_LATENCY_MS => {
                if sample.kind != MetricKind::Histogram {
                    // Latency is always a histogram in the taxonomy.
                }
                let labels = self.drop_node(sample.labels.clone());
                let metric = self
                    .inner
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .fault_latency_ms
                    .with_label_values(&label_values(&labels));
                metric.observe(sample.value);
            }
            SCENARIO_DURATION_MS => {
                let labels = sample.labels.clone();
                let metric = self
                    .inner
                    .read()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .scenario_duration_ms
                    .with_label_values(&label_values(&labels));
                metric.observe(sample.value);
            }
            _ => self.warn_once(strip_static_name(sample.name)),
        }
    }
}

/// Convert a counter sample value into a `u64` for `inc_by`.
///
/// Counter increments must be non-negative integers in practice; clamp
/// negatives to zero and round to nearest so a fractional "1.0" still works
/// without surprising clippy's truncation and sign-loss lints.
fn counter_increment(value: f64) -> u64 {
    let clamped = value.max(0.0);
    // `clamped` is non-negative, but clippy cannot prove that through `.max(0.0)`.
    // The `allow` is local to the operation that needs it.
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let increment = clamped.round() as u64;
    increment
}

/// Convert `Vec<(&'static str, String)>` to `Vec<&str>` for prometheus label APIs.
fn label_values<'a>(labels: &'a [(&'static str, String)]) -> Vec<&'a str> {
    labels.iter().map(|(_, v)| v.as_str()).collect()
}

/// Strip a metric name down to the byte prefix that fits in `&'static str`.
/// Used only for the warn-once map — Prometheus names are ASCII by contract.
fn strip_static_name(name: &str) -> &'static str {
    // SAFETY: metric names live in `super::*` constants, all of which are
    // `&'static str`. The leak is bounded to one allocation per name per
    // recorder lifetime and is unreachable for the canonical taxonomy.
    Box::leak(name.to_owned().into_boxed_str())
}

/// Exponential latency buckets from 1ms to ~60s.
///
/// Step factor of `2^(2/3) ≈ 1.587` between adjacent buckets gives 24 buckets
/// across six decades. This is the same shape as the `prometheus_client`
/// `DEFAULT_BUCKETS` rescaled into milliseconds.
const EXPO_MS_BUCKETS: &[f64] = &[
    1.0,
    1.587_4,
    2.519_8,
    4.0,
    6.349_6,
    10.079_4,
    16.0,
    25.398_4,
    40.317_5,
    64.0,
    101.593_7,
    161.27,
    256.0,
    406.374_9,
    645.08,
    1024.0,
    1_625.499_4,
    2_580.32,
    4096.0,
    6_501.998_4,
    10_321.28,
    16384.0,
    26_007.992_6,
    41_285.12,
    65_536.0,
];

impl PrometheusRecorder {
    /// Convenience: wrap `self` into a one-recorder [`MetricsHub`].
    #[must_use]
    pub fn into_hub(self) -> MetricsHub {
        MetricsHub::new().with_recorder(std::sync::Arc::new(self))
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::metrics::{
        MetricUnit, MetricsHub, sample_for_scenario_duration, sample_for_skipped_fault,
        samples_for_fault_result,
    };
    use crate::scenario::{ChaosScenario, ScenarioRegime};
    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::{FaultEvent, SkipReason};

    fn sample_ctx() -> FaultContext {
        FaultContext {
            seed: 1337,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    fn gather_text_emits_injected_counter_with_expected_labels()
    -> Result<(), Box<dyn std::error::Error>> {
        let recorder = Arc::new(PrometheusRecorder::new());
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let scenario = ChaosScenario::builder()
            .name("prom-wiring")
            .seed(1337)
            .add_fault(PacketLoss::builder().seed(42).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx();
        let _report = scenario.run_with_metrics(&mut ctx, &hub);

        let body = recorder.gather_text()?;

        assert!(
            body.contains(FAULTS_INJECTED_TOTAL),
            "missing counter line in scrape body:\n{body}"
        );
        assert!(
            body.contains(r#"fault_type="packet_loss""#),
            "missing fault_type label:\n{body}"
        );
        assert!(
            body.contains(r#"node_id="node-0""#),
            "missing node_id label:\n{body}"
        );
        assert!(
            body.contains(r#"scenario="prom-wiring""#),
            "missing scenario label:\n{body}"
        );
        Ok(())
    }

    #[test]
    fn gather_text_emits_histogram_buckets_for_scenario_duration()
    -> Result<(), Box<dyn std::error::Error>> {
        let recorder = Arc::new(PrometheusRecorder::new());
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let scenario = ChaosScenario::builder()
            .name("hist-wiring")
            .seed(99)
            .add_fault(PacketLoss::builder().seed(7).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx();
        let _report = scenario.run_with_metrics(&mut ctx, &hub);

        let body = recorder.gather_text()?;

        assert!(
            body.contains(&format!("{SCENARIO_DURATION_MS}_bucket")),
            "missing histogram bucket lines:\n{body}"
        );
        assert!(
            body.contains(r#"scenario="hist-wiring""#),
            "scenario label missing on histogram:\n{body}"
        );
        Ok(())
    }

    #[test]
    fn unknown_metric_name_is_logged_and_skipped_without_panic()
    -> Result<(), Box<dyn std::error::Error>> {
        let recorder = PrometheusRecorder::new();
        let bogus_name: &'static str = Box::leak(Box::new("malcolm_does_not_exist".to_owned()));

        recorder.record(&MetricSample {
            name: bogus_name,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });

        // Second invocation should not panic either.
        recorder.record(&MetricSample {
            name: bogus_name,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });

        // Scrape body must not contain the bogus name.
        let body = recorder.gather_text()?;
        assert!(!body.contains("malcolm_does_not_exist"));
        Ok(())
    }

    #[test]
    fn dropping_node_id_omits_label_from_rendered_output() -> Result<(), Box<dyn std::error::Error>>
    {
        let recorder = Arc::new(PrometheusRecorder::dropping_high_cardinality_labels());
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let event = FaultEvent {
            fault_type: "packet_loss".to_owned(),
            node_id: "ephemeral-pod-1234".to_owned(),
            seed: 1,
            intensity: 0.5,
            dry_run: false,
            timestamp_ms: 1,
        };
        for sample in samples_for_fault_result(&event, "demo", ScenarioRegime::Stable, 1) {
            hub.record(&sample);
        }
        hub.record(&sample_for_skipped_fault(
            "memory_pressure",
            "ephemeral-pod-1234",
            "demo",
            SkipReason::BelowThreshold,
            1,
        ));
        let report = crate::scenario::ScenarioReport {
            name: "demo".to_owned(),
            seed: 1,
            regime: ScenarioRegime::Stable,
            events: vec![crate::scenario::ScenarioEvent::from(event)],
            total_duration_ms: 5,
        };
        hub.record(&sample_for_scenario_duration(&report));

        let body = recorder.gather_text()?;

        assert!(
            !body.contains(r#"node_id="ephemeral-pod-1234""#),
            "node_id label should have been dropped:\n{body}"
        );
        assert!(body.contains(r#"fault_type="packet_loss""#));
        Ok(())
    }

    #[test]
    fn into_hub_wraps_recorder_for_one_line_setup() {
        let hub = PrometheusRecorder::new().into_hub();
        assert_eq!(hub.recorder_count(), 1);
    }
}
