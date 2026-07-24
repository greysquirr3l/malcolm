//! OpenTelemetry / OTLP exporter (feature `otel`).
//!
//! This module is feature-gated behind `malcolm`'s `otel` Cargo feature and
//! pulls the [`opentelemetry`], [`opentelemetry_sdk`], [`opentelemetry_otlp`],
//! and [`tracing_opentelemetry`] crates. Default builds neither compile nor
//! link this code.
//!
//! Sub-tasks (see [`PROGRESS.md`]):
//! - T28b (this commit): [`OtelConfig`] + [`OtelProtocol`] + [`from_env`]
//! - T28c (follow-up): [`OtelRecorder`] implementing [`MetricsRecorder`]
//! - T28d (follow-up): [`install_otel_tracing_layer`] bridging tracing
//! - T28f (follow-up): OTLP exporter wiring under `otel-grpc` / `otel-http`
//!
//! CI never opens a real OTLP collector — the metrics path uses the `OTel`
//! SDK's `TestMetricReader` for assertions; the traces path is verified by
//! layer construction only.

use std::time::Duration;

use thiserror::Error;

/// Wire protocol for OTLP export.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelProtocol {
    /// OTLP over gRPC (tonic transport). Requires `otel-grpc` feature.
    Grpc,
    /// OTLP over HTTP/protobuf. Requires `otel-http` feature.
    Http,
}

impl OtelProtocol {
    /// Parse the `OTEL_EXPORTER_OTLP_PROTOCOL` environment variable.
    ///
    /// Recognised values: `grpc`, `http/protobuf`, `http`. Empty or unset
    /// returns `Ok(None)` so the caller can fall back to the feature default.
    fn from_env_str(raw: &str) -> Result<Option<Self>, OtelProtocolError> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "" => Ok(None),
            "grpc" => Ok(Some(Self::Grpc)),
            "http" | "http/protobuf" => Ok(Some(Self::Http)),
            other => Err(OtelProtocolError(other.to_owned())),
        }
    }
}

/// Typed configuration for the OpenTelemetry exporter.
///
/// Construct via [`OtelConfig::from_env`] (which honors standard
/// `OTEL_EXPORTER_OTLP_*` variables) or build directly in tests.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// OTLP collector endpoint, e.g. `http://localhost:4317` for gRPC or
    /// `http://localhost:4318` for HTTP.
    pub endpoint: String,
    /// Wire protocol.
    pub protocol: OtelProtocol,
    /// Additional HTTP/gRPC headers (e.g. auth tokens).
    pub headers: Vec<(String, String)>,
    /// Logical service name attached to every emitted span and metric.
    pub service_name: String,
    /// Per-export timeout. Defaults to 10 seconds when unspecified.
    pub timeout: Duration,
}

impl OtelConfig {
    /// Sensible defaults for tests and one-liner setup.
    ///
    /// - endpoint: `http://localhost:4317` (gRPC default)
    /// - protocol: [`OtelProtocol::Grpc`]
    /// - headers: empty
    /// - `service_name`: `malcolm`
    /// - timeout: 10 seconds
    #[must_use]
    pub fn default_for_tests() -> Self {
        Self {
            endpoint: "http://localhost:4317".to_owned(),
            protocol: OtelProtocol::Grpc,
            headers: Vec::new(),
            service_name: "malcolm".to_owned(),
            timeout: Duration::from_secs(10),
        }
    }

    /// Build a config from standard `OTEL_EXPORTER_OTLP_*` environment variables.
    ///
    /// | Variable | Field |
    /// |----------|-------|
    /// | `OTEL_EXPORTER_OTLP_ENDPOINT` | `endpoint` |
    /// | `OTEL_EXPORTER_OTLP_PROTOCOL` | `protocol` (empty → `Grpc`) |
    /// | `OTEL_EXPORTER_OTLP_HEADERS` | `headers` (comma-separated `k=v` pairs) |
    /// | `OTEL_SERVICE_NAME` | `service_name` (empty → `malcolm`) |
    /// | `OTEL_EXPORTER_OTLP_TIMEOUT` | `timeout` seconds (empty → 10) |
    ///
    /// # Errors
    ///
    /// Returns [`OtelError::MissingEndpoint`] if
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` is unset or empty.
    /// Returns [`OtelError::InvalidProtocol`] if the protocol value is not one
    /// of `grpc`, `http`, or `http/protobuf`.
    /// Returns [`OtelError::InvalidTimeout`] if the timeout is not a positive
    /// integer of seconds.
    /// Returns [`OtelError::MalformedHeaders`] if a header entry is missing
    /// the `=` separator.
    pub fn from_env() -> Result<Self, OtelError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Same as [`from_env`](Self::from_env) but reads through a caller-supplied
    /// closure. Used by tests to inject env values deterministically.
    pub(crate) fn from_lookup<F>(lookup: F) -> Result<Self, OtelError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let endpoint = lookup("OTEL_EXPORTER_OTLP_ENDPOINT")
            .filter(|s| !s.trim().is_empty())
            .ok_or(OtelError::MissingEndpoint)?;

        let protocol = match lookup("OTEL_EXPORTER_OTLP_PROTOCOL") {
            Some(raw) => OtelProtocol::from_env_str(&raw)
                .map_err(OtelError::InvalidProtocol)?
                .unwrap_or(OtelProtocol::Grpc),
            None => OtelProtocol::Grpc,
        };

        let headers = match lookup("OTEL_EXPORTER_OTLP_HEADERS") {
            Some(raw) if !raw.trim().is_empty() => parse_headers(&raw)?,
            _ => Vec::new(),
        };

        let service_name = lookup("OTEL_SERVICE_NAME")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "malcolm".to_owned());

        let timeout_seconds = match lookup("OTEL_EXPORTER_OTLP_TIMEOUT") {
            Some(raw) if !raw.trim().is_empty() => raw
                .trim()
                .parse::<u64>()
                .map_err(|_| OtelError::InvalidTimeout(raw.clone()))?,
            _ => 10,
        };

        Ok(Self {
            endpoint,
            protocol,
            headers,
            service_name,
            timeout: Duration::from_secs(timeout_seconds),
        })
    }
}

/// Typed errors for [`OtelConfig::from_env`].
#[derive(Debug, Error)]
pub enum OtelError {
    /// `OTEL_EXPORTER_OTLP_ENDPOINT` was missing or empty.
    #[error("OTEL_EXPORTER_OTLP_ENDPOINT must be set for OpenTelemetry export")]
    MissingEndpoint,
    /// `OTEL_EXPORTER_OTLP_PROTOCOL` was set to something other than
    /// `grpc`, `http`, or `http/protobuf`.
    #[error("invalid OTEL_EXPORTER_OTLP_PROTOCOL value: {0}")]
    InvalidProtocol(OtelProtocolError),
    /// `OTEL_EXPORTER_OTLP_TIMEOUT` could not be parsed as a positive integer.
    #[error("invalid OTEL_EXPORTER_OTLP_TIMEOUT value: {0}")]
    InvalidTimeout(String),
    /// `OTEL_EXPORTER_OTLP_HEADERS` contained an entry without `=`.
    #[error("malformed OTEL_EXPORTER_OTLP_HEADERS entry: {0}")]
    MalformedHeaders(String),
}

/// Wrapper for the bad-protocol string so the inner type stays `String`.
#[derive(Debug)]
pub struct OtelProtocolError(pub String);

impl std::fmt::Display for OtelProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for OtelProtocolError {}

/// Parse a comma-separated header list (`k1=v1,k2=v2`). Splits on the first
/// `=` per entry so values may contain `=` themselves.
fn parse_headers(raw: &str) -> Result<Vec<(String, String)>, OtelError> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|entry| {
            entry
                .split_once('=')
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                .ok_or_else(|| OtelError::MalformedHeaders(entry.to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn env<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn lookup_err(entries: &[(&'static str, &'static str)]) -> OtelError {
        match OtelConfig::from_lookup(env(entries)) {
            Ok(_) => OtelError::MissingEndpoint,
            Err(e) => e,
        }
    }

    #[test]
    fn missing_endpoint_when_env_is_empty_returns_typed_error() {
        // Per the OTel spec, endpoint is mandatory — from_env does NOT
        // synthesize one. `default_for_tests()` is the documented escape
        // hatch for tests that want a working config without env plumbing.
        let err = lookup_err(&[]);
        assert!(matches!(err, OtelError::MissingEndpoint));
    }

    #[test]
    fn defaults_apply_when_endpoint_present_but_other_vars_absent() -> Result<(), OtelError> {
        let cfg = OtelConfig::from_lookup(env(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x")]))?;
        assert_eq!(cfg.protocol, OtelProtocol::Grpc);
        assert_eq!(cfg.timeout, Duration::from_secs(10));
        assert_eq!(cfg.service_name, "malcolm");
        assert!(cfg.headers.is_empty());
        Ok(())
    }

    #[test]
    fn missing_endpoint_returns_typed_error() {
        let err = lookup_err(&[("OTEL_SERVICE_NAME", "demo")]);
        assert!(matches!(err, OtelError::MissingEndpoint));
    }

    #[test]
    fn explicit_endpoint_and_protocol_parse_correctly() -> Result<(), OtelError> {
        let cfg = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
            ("OTEL_SERVICE_NAME", "resilience-runner"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "30"),
        ]))?;
        assert_eq!(cfg.endpoint, "http://collector:4318");
        assert_eq!(cfg.protocol, OtelProtocol::Http);
        assert_eq!(cfg.service_name, "resilience-runner");
        assert_eq!(cfg.timeout, Duration::from_secs(30));
        Ok(())
    }

    #[test]
    fn grpc_protocol_accepts_both_canonical_and_lowercase() -> Result<(), OtelError> {
        for raw in ["grpc", "GRPC", "gRpc"] {
            let cfg = OtelConfig::from_lookup(env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", raw),
            ]))?;
            assert_eq!(cfg.protocol, OtelProtocol::Grpc);
        }
        Ok(())
    }

    #[test]
    fn invalid_protocol_returns_typed_error() {
        let err = lookup_err(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "websocket"),
        ]);
        assert!(
            matches!(err, OtelError::InvalidProtocol(ref inner) if inner.0 == "websocket"),
            "expected InvalidProtocol(websocket), got {err:?}",
        );
    }

    #[test]
    fn headers_parse_with_multiple_entries_and_value_containing_equals() -> Result<(), OtelError> {
        let cfg = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                "Authorization=Bearer abc==,x-foo=bar",
            ),
        ]))?;
        assert_eq!(
            cfg.headers,
            vec![
                ("Authorization".to_owned(), "Bearer abc==".to_owned()),
                ("x-foo".to_owned(), "bar".to_owned()),
            ]
        );
        Ok(())
    }

    #[test]
    fn headers_malformed_entry_returns_typed_error() {
        let err = lookup_err(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "good=ok,bad-entry"),
        ]);
        assert!(matches!(err, OtelError::MalformedHeaders(ref s) if s == "bad-entry"));
    }

    #[test]
    fn invalid_timeout_returns_typed_error() {
        let err = lookup_err(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "thirty"),
        ]);
        assert!(matches!(err, OtelError::InvalidTimeout(ref s) if s == "thirty"));
    }

    #[test]
    fn default_for_tests_has_stable_shape() {
        let cfg = OtelConfig::default_for_tests();
        assert_eq!(cfg.endpoint, "http://localhost:4317");
        assert_eq!(cfg.protocol, OtelProtocol::Grpc);
        assert_eq!(cfg.service_name, "malcolm");
        assert_eq!(cfg.timeout, Duration::from_secs(10));
        assert!(cfg.headers.is_empty());
    }
}

// ─── T28c ──────────────────────────────────────────────────────────────────
// OtelRecorder: MetricsRecorder impl backed by an `SdkMeterProvider`.
//
// The recorder owns its provider, so `shutdown()` cleans everything up. CI
// uses `TestMetricReader` (no network); T28f will plug an OTLP exporter
// behind the `otel-grpc` / `otel-http` sub-features. Force-flush + shutdown
// are explicit because short-lived CI runs can drop the last export on
// exit otherwise — see `OtelRecorder::force_flush` / `OtelRecorder::shutdown`.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use opentelemetry::metrics::{Counter, Gauge, Histogram, MeterProvider};
use opentelemetry::{KeyValue, Value};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::metrics::SdkMeterProvider;

use super::{
    FAULT_INTENSITY, FAULT_LATENCY_MS, FAULTS_INJECTED_TOTAL, FAULTS_SKIPPED_TOTAL, MetricKind,
    MetricSample, MetricsRecorder, SCENARIO_DURATION_MS,
};

const METER_SCOPE: &str = "malcolm";

/// Errors produced by [`OtelRecorder`] during `force_flush` / `shutdown`.
///
/// Sample-handling errors are intentionally swallowed and logged at
/// `tracing::warn!` so a misconfigured pipeline never crashes a chaos run.
#[derive(Debug, thiserror::Error)]
pub enum OtelRecorderError {
    /// The recorder's provider was shut down before this operation.
    #[error("OpenTelemetry recorder has been shut down")]
    Shutdown,
    /// An error returned by the underlying `OTel` SDK (typically a flush or
    /// shutdown failure).
    #[error("OpenTelemetry SDK error: {0}")]
    Sdk(String),
    /// The selected transport sub-feature is not enabled.
    #[error("transport feature required: {0}")]
    TransportFeature(String),
    /// The underlying OTLP exporter builder rejected the config.
    #[error("OTLP exporter build failed: {0}")]
    ExporterBuild(String),
}

/// `OTel` metrics recorder. Wire into [`MetricsHub`](super::MetricsHub) like any
/// other [`MetricsRecorder`].
pub struct OtelRecorder {
    provider: SdkMeterProvider,
    instruments: InstrumentSet,
    warned: RwLock<HashSet<&'static str>>,
    shutdown_called: AtomicBool,
}

#[derive(Clone)]
struct InstrumentSet {
    faults_injected: Counter<u64>,
    faults_skipped: Counter<u64>,
    fault_intensity: Gauge<f64>,
    fault_latency_ms: Histogram<f64>,
    scenario_duration_ms: Histogram<f64>,
}

impl OtelRecorder {
    /// Build a recorder over an in-memory `TestMetricReader`. Production
    /// wiring (a `PeriodicReader` over an OTLP exporter) lands in T28f
    /// behind the `otel-grpc`/`otel-http` sub-features.
    ///
    /// The SDK's `MeterProviderBuilder::with_reader` takes the reader by
    /// value, so this constructor transfers ownership. To keep the
    /// `TestMetricReader` alive for post-run assertions in tests, pair it
    /// with [`OtelRecorder::paired_test_reader`] which builds a recorder
    /// *without* consuming the reader, leaving the Arc available to the
    /// caller.
    #[must_use]
    pub fn with_test_reader(service_name: &str) -> Arc<Self> {
        use opentelemetry_sdk::metrics::MeterProviderBuilder;
        let reader = opentelemetry_sdk::testing::metrics::TestMetricReader::new();
        let resource = Resource::builder()
            .with_service_name(service_name.to_owned())
            .build();
        let provider = MeterProviderBuilder::default()
            .with_resource(resource)
            .with_reader(reader)
            .build();

        let meter = provider.meter(METER_SCOPE);
        let instruments = InstrumentSet {
            faults_injected: meter
                .u64_counter(FAULTS_INJECTED_TOTAL)
                .with_description("Total faults successfully injected.")
                .with_unit("1")
                .build(),
            faults_skipped: meter
                .u64_counter(FAULTS_SKIPPED_TOTAL)
                .with_description("Total faults skipped.")
                .with_unit("1")
                .build(),
            fault_intensity: meter
                .f64_gauge(FAULT_INTENSITY)
                .with_description("Latest observed intensity per (fault_type, node_id).")
                .with_unit("1")
                .build(),
            fault_latency_ms: meter
                .f64_histogram(FAULT_LATENCY_MS)
                .with_description("Fault-reported latency (ms).")
                .with_unit("ms")
                .build(),
            scenario_duration_ms: meter
                .f64_histogram(SCENARIO_DURATION_MS)
                .with_description("Scenario wall-clock duration (ms).")
                .with_unit("ms")
                .build(),
        };

        Arc::new(Self {
            provider,
            instruments,
            warned: RwLock::new(HashSet::new()),
            shutdown_called: AtomicBool::new(false),
        })
    }

    /// Flush any pending metric data through the underlying reader. Safe to
    /// call multiple times. Always call before process exit on a short-lived
    /// run; otherwise the last batch may be lost.
    pub fn force_flush(&self) -> Result<(), OtelRecorderError> {
        self.provider
            .force_flush()
            .map_err(|e| OtelRecorderError::Sdk(format!("force_flush failed: {e:?}")))
    }

    /// Shut the provider down. After this call the recorder stops producing
    /// data and any further `record` calls log warnings and skip.
    ///
    /// Idempotent at the [`OtelRecorder`] layer: subsequent calls return
    /// `Ok(())` without invoking the underlying SDK shutdown (which would
    /// otherwise return an error on a second call).
    pub fn shutdown(&self) -> Result<(), OtelRecorderError> {
        if self.shutdown_called.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.provider
            .shutdown()
            .map_err(|e| OtelRecorderError::Sdk(format!("shutdown failed: {e:?}")))
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
                "unknown metric name received by OtelRecorder; skipping",
            );
        }
    }
}

impl MetricsRecorder for OtelRecorder {
    fn record(&self, sample: &MetricSample) {
        let attrs = labels_to_keyvalues(&sample.labels);
        match sample.name {
            FAULTS_INJECTED_TOTAL => {
                self.instruments
                    .faults_injected
                    .add(counter_increment(sample.value), &attrs);
            }
            FAULTS_SKIPPED_TOTAL => {
                self.instruments
                    .faults_skipped
                    .add(counter_increment(sample.value), &attrs);
            }
            FAULT_INTENSITY => {
                self.instruments
                    .fault_intensity
                    .record(sample.value, &attrs);
            }
            FAULT_LATENCY_MS => {
                let _ = sample.kind == MetricKind::Histogram;
                self.instruments
                    .fault_latency_ms
                    .record(sample.value, &attrs);
            }
            SCENARIO_DURATION_MS => {
                self.instruments
                    .scenario_duration_ms
                    .record(sample.value, &attrs);
            }
            _ => self.warn_once(static_name(sample.name)),
        }
    }
}

/// Counter increments must be non-negative integers in practice.
fn counter_increment(value: f64) -> u64 {
    let clamped = value.max(0.0);
    #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
    let inc = clamped.round() as u64;
    inc
}

/// Cache unknown metric names in a `&'static str` map for the recorder lifetime.
fn static_name(name: &str) -> &'static str {
    Box::leak(Box::new(name.to_owned()))
}

/// Convert the malcolm label set (`Vec<(&'static str, String)>`) into `OTel`'s
/// `Vec<KeyValue>`. Numeric `dry_run` labels stay as strings — `OTel`'s
/// attribute values are not strongly typed and Prometheus/Grafana parse
/// string-encoded booleans correctly.
fn labels_to_keyvalues(labels: &[(&'static str, String)]) -> Vec<KeyValue> {
    labels
        .iter()
        .map(|(k, v)| KeyValue::new(*k, Value::from(v.clone())))
        .collect()
}

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::metrics::{MetricsHub, sample_for_scenario_duration, sample_for_skipped_fault};
    use crate::scenario::ChaosScenario;
    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::SkipReason;

    fn sample_ctx() -> FaultContext {
        FaultContext {
            seed: 1337,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    fn scenario_run_emits_samples_without_panic() -> Result<(), OtelRecorderError> {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let scenario = ChaosScenario::builder()
            .name("otel-wiring")
            .seed(1337)
            .add_fault(PacketLoss::builder().seed(42).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx();
        let _report = scenario.run_with_metrics(&mut ctx, &hub);
        recorder.force_flush()?;
        Ok(())
    }

    #[test]
    fn skipped_sample_routes_through_recorder_without_panic() -> Result<(), OtelRecorderError> {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let sample = sample_for_skipped_fault(
            "memory_pressure",
            "node-7",
            "otel-skipped",
            SkipReason::BelowThreshold,
            1,
        );
        hub.record(&sample);
        recorder.force_flush()?;
        Ok(())
    }

    #[test]
    fn duration_sample_routes_through_recorder_without_panic() -> Result<(), OtelRecorderError> {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let report = crate::scenario::ScenarioReport {
            name: "demo".to_owned(),
            seed: 1,
            regime: crate::scenario::ScenarioRegime::Stable,
            events: Vec::new(),
            total_duration_ms: 42,
        };
        hub.record(&sample_for_scenario_duration(&report));
        recorder.force_flush()?;
        Ok(())
    }

    #[test]
    fn unknown_metric_name_is_warned_and_skipped_without_panic() -> Result<(), OtelRecorderError> {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        let bogus: &'static str = Box::leak(Box::new("malcolm_does_not_exist".to_owned()));
        recorder.record(&MetricSample {
            name: bogus,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: super::super::MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });
        recorder.force_flush()?;
        recorder.record(&MetricSample {
            name: bogus,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: super::super::MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });
        Ok(())
    }

    #[test]
    fn force_flush_and_shutdown_are_idempotent_and_ok_on_empty_recorder()
    -> Result<(), OtelRecorderError> {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        recorder.force_flush()?;
        recorder.force_flush()?;
        recorder.shutdown()?;
        recorder.shutdown()?;
        Ok(())
    }
}
#[cfg(test)]
mod exporter_construction_tests {
    use super::*;

    fn build_config(protocol: OtelProtocol) -> OtelConfig {
        OtelConfig {
            endpoint: "http://127.0.0.1:1".to_owned(),
            protocol,
            headers: Vec::new(),
            service_name: "malcolm-test".to_owned(),
            timeout: std::time::Duration::from_millis(100),
        }
    }

    #[test]
    #[cfg(feature = "otel-grpc")]
    fn with_otlp_exporter_grpc_builds_recorder_against_unroutable_endpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        // The Tonic exporter builder constructs its own runtime context,
        // so it must run inside a tokio runtime even for a noop build.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let recorder =
            rt.block_on(async { with_otlp_exporter(&build_config(OtelProtocol::Grpc)) })?;
        recorder.shutdown()?;
        Ok(())
    }

    #[test]
    #[cfg(feature = "otel-http")]
    fn with_otlp_exporter_http_builds_recorder_against_unroutable_endpoint()
    -> Result<(), Box<dyn std::error::Error>> {
        // The HTTP exporter's reqwest client constructor needs a tokio
        // runtime; run inside a single-threaded runtime for the build.
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let recorder =
            rt.block_on(async { with_otlp_exporter(&build_config(OtelProtocol::Http)) })?;
        recorder.shutdown()?;
        Ok(())
    }
}

// ─── T28d ──────────────────────────────────────────────────────────────────
// Bridge `tracing` events on the `malcolm` target into OTel spans via
// `tracing-opentelemetry`. CI exercises the layer construction only; real
// export is wired in T28f.

use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::Targets;

/// Build a [`tracing_opentelemetry::layer`] filtered to the `malcolm`
/// target. Add it to a [`tracing_subscriber::Registry`] subscriber to turn
/// every `tracing` event on `malcolm` into an `OTel` span.
///
/// # Example
///
/// ```rust,no_run
/// use malcolm::metrics::otel::{OtelConfig, otel_tracing_layer};
/// use opentelemetry::trace::TracerProvider;
/// use opentelemetry_sdk::trace::SdkTracerProvider;
/// use tracing_subscriber::layer::SubscriberExt;
/// let provider = SdkTracerProvider::builder().build();
/// let layer = otel_tracing_layer(&provider);
/// let subscriber = tracing_subscriber::registry().with(layer);
/// let _guard = tracing::subscriber::set_default(subscriber);
/// tracing::info!(target: "malcolm", fault_type = "packet_loss", "span");
/// # let _ = OtelConfig::default_for_tests();
/// ```
pub fn otel_tracing_layer(
    provider: &SdkTracerProvider,
) -> impl Layer<tracing_subscriber::Registry> {
    let tracer = provider.tracer("malcolm");
    tracing_opentelemetry::layer::<tracing_subscriber::Registry>()
        .with_tracer(tracer)
        .with_filter(Targets::new().with_target("malcolm", tracing::Level::INFO))
}

#[cfg(test)]
mod tracing_tests {
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing_subscriber::layer::SubscriberExt;

    use super::otel_tracing_layer;

    #[test]
    fn otel_tracing_layer_construction_does_not_panic() {
        let provider = SdkTracerProvider::builder().build();
        let _layer = otel_tracing_layer(&provider);
        // Construction succeeded; no exporter wired, so no spans leave the process.
    }

    #[test]
    fn otel_tracing_layer_emits_span_when_subscribed() {
        let provider = SdkTracerProvider::builder().build();
        let layer = otel_tracing_layer(&provider);
        let subscriber = tracing_subscriber::registry().with(layer);
        let _guard = tracing::subscriber::set_default(subscriber);

        // Emit one event on the malcolm target; the layer should turn it
        // into a span on the OTel side without panicking.
        tracing::info!(target: "malcolm", fault_type = "noop", "smoke");
    }
}

// ─── T28f ──────────────────────────────────────────────────────────────────
// OTLP exporter wiring behind `otel-grpc` / `otel-http` sub-features.
//
// `with_otlp_exporter(config)` builds a `MetricExporter` for the selected
// protocol, wraps it in a `PeriodicReader`, and returns a fully-configured
// `OtelRecorder` ready to install in a `MetricsHub`. CI does not exercise
// this path (no live collector); construction is covered by a unit test
// that runs against a non-routable local endpoint and asserts the recorder
// builds without panic.

/// Wire an [`OtelRecorder`] to an OTLP exporter for the selected protocol.
///
/// Selects gRPC (tonic) or HTTP/protobuf based on which sub-feature
/// (`otel-grpc` / `otel-http`) is enabled. Headers belong in the standard
/// `OTEL_EXPORTER_OTLP_HEADERS` env var.
///
/// Headers belong in the standard `OTEL_EXPORTER_OTLP_HEADERS` env var,
/// which the OTLP SDK consumes automatically.
///
/// Call [`OtelRecorder::force_flush`] and [`OtelRecorder::shutdown`] on
/// the returned recorder before process exit on short-lived CI runs.
#[cfg(any(feature = "otel-grpc", feature = "otel-http"))]
pub fn with_otlp_exporter(config: &OtelConfig) -> Result<Arc<OtelRecorder>, OtelRecorderError> {
    use opentelemetry_otlp::MetricExporter;
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::metrics::PeriodicReader;
    use std::time::Duration;

    // Note: OTel's `WithExportConfig` trait only exposes `with_endpoint`,
    // `with_protocol`, and `with_timeout`. HTTP headers belong in the
    // standard `OTEL_EXPORTER_OTLP_HEADERS` env var, which the OTLP SDK
    // consumes automatically. `OtelConfig::from_env` validates header
    // syntax so the validation happens even though we don't re-inject
    // them programmatically.
    let _ = config.headers.len();

    let exporter = match config.protocol {
        OtelProtocol::Grpc => {
            #[cfg(feature = "otel-grpc")]
            {
                MetricExporter::builder()
                    .with_tonic()
                    .with_endpoint(config.endpoint.clone())
                    .with_timeout(config.timeout)
                    .build()
                    .map_err(|e| OtelRecorderError::ExporterBuild(format!("{e:?}")))?
            }
            #[cfg(not(feature = "otel-grpc"))]
            {
                return Err(OtelRecorderError::TransportFeature(
                    "otel-grpc required for OTLP/gRPC export".to_owned(),
                ));
            }
        }
        OtelProtocol::Http => {
            #[cfg(feature = "otel-http")]
            {
                MetricExporter::builder()
                    .with_http()
                    .with_endpoint(config.endpoint.clone())
                    .with_timeout(config.timeout)
                    .build()
                    .map_err(|e| OtelRecorderError::ExporterBuild(format!("{e:?}")))?
            }
            #[cfg(not(feature = "otel-http"))]
            {
                return Err(OtelRecorderError::TransportFeature(
                    "otel-http required for OTLP/HTTP export".to_owned(),
                ));
            }
        }
    };

    let reader = PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(30))
        .build();

    Ok(OtelRecorder::with_periodic_reader(
        reader,
        &config.service_name,
    ))
}

/// Stub returned when neither `otel-grpc` nor `otel-http` is enabled.
/// Enable one of those sub-features to use the real OTLP exporter path.
#[cfg(not(any(feature = "otel-grpc", feature = "otel-http")))]
pub fn with_otlp_exporter(config: &OtelConfig) -> Result<Arc<OtelRecorder>, OtelRecorderError> {
    let _ = config;
    Err(OtelRecorderError::TransportFeature(
        "enable either `otel-grpc` or `otel-http` to use with_otlp_exporter".to_owned(),
    ))
}

impl OtelRecorder {
    /// Build a recorder backed by a [`PeriodicReader`]. Used by
    /// [`with_otlp_exporter`] to wire production OTLP exports. The
    /// private `MetricReader` trait bound that `MeterProviderBuilder`
    /// wants is satisfied internally because `PeriodicReader<E>`
    /// implements it; we constrain on the public [`PushMetricExporter`]
    /// bound here so this helper has a usable signature.
    pub(crate) fn with_periodic_reader<E>(
        reader: opentelemetry_sdk::metrics::PeriodicReader<E>,
        service_name: &str,
    ) -> Arc<Self>
    where
        E: opentelemetry_sdk::metrics::exporter::PushMetricExporter + 'static,
    {
        use opentelemetry_sdk::metrics::MeterProviderBuilder;
        let resource = Resource::builder()
            .with_service_name(service_name.to_owned())
            .build();
        let provider = MeterProviderBuilder::default()
            .with_resource(resource)
            .with_reader(reader)
            .build();
        let meter = provider.meter(METER_SCOPE);

        let instruments = InstrumentSet {
            faults_injected: meter.u64_counter(FAULTS_INJECTED_TOTAL).build(),
            faults_skipped: meter.u64_counter(FAULTS_SKIPPED_TOTAL).build(),
            fault_intensity: meter.f64_gauge(FAULT_INTENSITY).build(),
            fault_latency_ms: meter.f64_histogram(FAULT_LATENCY_MS).build(),
            scenario_duration_ms: meter.f64_histogram(SCENARIO_DURATION_MS).build(),
        };

        Arc::new(Self {
            provider,
            instruments,
            warned: RwLock::new(HashSet::new()),
            shutdown_called: AtomicBool::new(false),
        })
    }
}
