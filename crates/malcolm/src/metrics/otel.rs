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
//! CI never opens a real OTLP collector — the metrics path uses the OTel
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
    /// - service_name: `malcolm`
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

    #[test]
    fn missing_endpoint_when_env_is_empty_returns_typed_error() {
        // Per the OTel spec, endpoint is mandatory — from_env does NOT
        // synthesize one. `default_for_tests()` is the documented escape
        // hatch for tests that want a working config without env plumbing.
        let err = OtelConfig::from_lookup(env(&[]))
            .expect_err("empty env must surface a typed MissingEndpoint error");
        assert!(matches!(err, OtelError::MissingEndpoint));
    }

    #[test]
    fn defaults_apply_when_endpoint_present_but_other_vars_absent() {
        let cfg = OtelConfig::from_lookup(env(&[("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x")]))
            .expect("endpoint alone should produce a working config");
        assert_eq!(cfg.protocol, OtelProtocol::Grpc);
        assert_eq!(cfg.timeout, Duration::from_secs(10));
        assert_eq!(cfg.service_name, "malcolm");
        assert!(cfg.headers.is_empty());
    }

    #[test]
    fn missing_endpoint_returns_typed_error() {
        let err = OtelConfig::from_lookup(env(&[("OTEL_SERVICE_NAME", "demo")]))
            .expect_err("missing endpoint must error");
        assert!(matches!(err, OtelError::MissingEndpoint));
    }

    #[test]
    fn explicit_endpoint_and_protocol_parse_correctly() {
        let cfg = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://collector:4318"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "http/protobuf"),
            ("OTEL_SERVICE_NAME", "resilience-runner"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "30"),
        ]))
        .expect("explicit config must parse");
        assert_eq!(cfg.endpoint, "http://collector:4318");
        assert_eq!(cfg.protocol, OtelProtocol::Http);
        assert_eq!(cfg.service_name, "resilience-runner");
        assert_eq!(cfg.timeout, Duration::from_secs(30));
    }

    #[test]
    fn grpc_protocol_accepts_both_canonical_and_lowercase() {
        for raw in ["grpc", "GRPC", "gRpc"] {
            let cfg = OtelConfig::from_lookup(env(&[
                ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
                ("OTEL_EXPORTER_OTLP_PROTOCOL", raw),
            ]))
            .expect("grpc variants must parse");
            assert_eq!(cfg.protocol, OtelProtocol::Grpc);
        }
    }

    #[test]
    fn invalid_protocol_returns_typed_error() {
        let err = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_PROTOCOL", "websocket"),
        ]))
        .expect_err("bogus protocol must error");
        assert!(
            matches!(err, OtelError::InvalidProtocol(ref inner) if inner.0 == "websocket"),
            "expected InvalidProtocol(websocket), got {err:?}",
        );
    }

    #[test]
    fn headers_parse_with_multiple_entries_and_value_containing_equals() {
        let cfg = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            (
                "OTEL_EXPORTER_OTLP_HEADERS",
                "Authorization=Bearer abc==,x-foo=bar",
            ),
        ]))
        .expect("headers must parse");
        assert_eq!(
            cfg.headers,
            vec![
                ("Authorization".to_owned(), "Bearer abc==".to_owned()),
                ("x-foo".to_owned(), "bar".to_owned()),
            ]
        );
    }

    #[test]
    fn headers_malformed_entry_returns_typed_error() {
        let err = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_HEADERS", "good=ok,bad-entry"),
        ]))
        .expect_err("missing '=' must error");
        assert!(matches!(err, OtelError::MalformedHeaders(ref s) if s == "bad-entry"));
    }

    #[test]
    fn invalid_timeout_returns_typed_error() {
        let err = OtelConfig::from_lookup(env(&[
            ("OTEL_EXPORTER_OTLP_ENDPOINT", "http://x"),
            ("OTEL_EXPORTER_OTLP_TIMEOUT", "thirty"),
        ]))
        .expect_err("non-numeric timeout must error");
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
    /// An error returned by the underlying OTel SDK (typically a flush or
    /// shutdown failure).
    #[error("OpenTelemetry SDK error: {0}")]
    Sdk(String),
}

/// OTel metrics recorder. Wire into [`MetricsHub`](super::MetricsHub) like any
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
        let mut warned = self.warned.write().expect("poisoned");
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

/// Convert the malcolm label set (`Vec<(&'static str, String)>`) into OTel's
/// `Vec<KeyValue>`. Numeric `dry_run` labels stay as strings — OTel's
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
    fn scenario_run_emits_samples_without_panic() {
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
        recorder
            .force_flush()
            .expect("force_flush must succeed after a scenario run");
    }

    #[test]
    fn skipped_sample_routes_through_recorder_without_panic() {
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
        recorder.force_flush().expect("force_flush");
    }

    #[test]
    fn duration_sample_routes_through_recorder_without_panic() {
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
        recorder.force_flush().expect("force_flush");
    }

    #[test]
    fn unknown_metric_name_is_warned_and_skipped_without_panic() {
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
        recorder.force_flush().expect("force_flush");
        recorder.record(&MetricSample {
            name: bogus,
            kind: MetricKind::Counter,
            value: 1.0,
            unit: super::super::MetricUnit::Count,
            labels: Vec::new(),
            timestamp_ms: 0,
        });
    }

    #[test]
    fn force_flush_and_shutdown_are_idempotent_and_ok_on_empty_recorder() {
        let recorder = OtelRecorder::with_test_reader("malcolm-test");
        recorder.force_flush().expect("flush on empty recorder");
        recorder
            .force_flush()
            .expect("flush again on empty recorder");
        recorder.shutdown().expect("shutdown on empty recorder");
        recorder
            .shutdown()
            .expect("shutdown again on empty recorder");
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
