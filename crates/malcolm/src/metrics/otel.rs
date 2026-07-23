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
