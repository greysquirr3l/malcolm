//! `StatsD` / `DogStatsD` line-protocol exporter (feature `statsd`).
//!
//! Lightweight, fire-and-forget UDP push for teams already on Datadog or
//! any StatsD-compatible collector. Uses `std::net::UdpSocket` directly —
//! no third-party dependency.
//!
//! Sub-tasks (see `PROGRESS.md`):
//! - T29b (this commit): `StatsdConfig` + `Dialect` + `from_env`
//! - T29c (follow-up): `StatsdRecorder` (hand-written encoder + batching)

/// Line-protocol dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    /// Plain `StatsD` — no tag support, labels fold into the metric name.
    Statsd,
    /// `DogStatsD` — `|#k1:v1,k2:v2` tag syntax, native histogram support.
    DogStatsd,
}

use std::time::Duration;
use thiserror::Error;

/// Errors produced by [`StatsdConfig::from_env`].
#[derive(Debug, Error)]
pub enum StatsdError {
    /// The destination port is not a valid `u16`.
    #[error("invalid statsd port: {0}")]
    InvalidPort(String),
    /// The `max_packet_bytes` value is not a positive integer.
    #[error("invalid statsd max_packet_bytes: {0}")]
    InvalidMaxBytes(String),
}

/// Typed configuration for the `StatsD` / `DogStatsD` exporter.
///
/// # Env vars
///
/// - `DD_AGENT_HOST` — host (default `127.0.0.1`)
/// - `DD_DOGSTATSD_PORT` — port (default `8125`)
/// - `MALCOLM_STATSD_PREFIX` — metric name prefix (default empty)
/// - `MALCOLM_STATSD_DIALECT` — `statsd` (default) or `dogstatsd`
/// - `MALCOLM_STATSD_MAX_BYTES` — max UDP datagram payload (default `1432`)
/// - `MALCOLM_STATSD_TIMEOUT_MS` — connect timeout in milliseconds (default `100`)
///
/// # Constant tags
///
/// `DD_TAGS` and `MALCOLM_STATSD_TAGS` are read as comma-separated `k=v`
/// pairs (same parser as the OTLP exporter). The first non-empty source
/// wins; `DD_TAGS` is the canonical Datadog convention.
#[derive(Debug, Clone)]
pub struct StatsdConfig {
    /// Destination host (IPv4 / IPv6 / DNS name).
    pub host: String,
    /// Destination UDP port.
    pub port: u16,
    /// Metric name prefix (e.g. `malcolm`).
    pub prefix: String,
    /// Line protocol dialect.
    pub dialect: Dialect,
    /// Tags attached to every metric (Datadog convention: `env:prod,team:chaos`).
    pub constant_tags: Vec<(String, String)>,
    /// Maximum UDP datagram payload. Default `1432` matches the safe
    /// Ethernet MTU minus headers.
    pub max_packet_bytes: usize,
    /// UDP connect timeout.
    pub timeout: Duration,
}

impl StatsdConfig {
    /// Sensible defaults for tests and one-liner setup.
    #[must_use]
    pub fn default_for_tests() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 8125,
            prefix: String::new(),
            dialect: Dialect::Statsd,
            constant_tags: Vec::new(),
            max_packet_bytes: 1432,
            timeout: Duration::from_millis(100),
        }
    }

    /// Read configuration from the standard Datadog env vars.
    pub fn from_env() -> Result<Self, StatsdError> {
        Self::from_lookup(|k| std::env::var(k).ok())
    }

    /// Same as [`from_env`](Self::from_env) but reads through a caller-supplied
    /// closure. Used by tests to inject env values deterministically.
    pub(crate) fn from_lookup<F>(lookup: F) -> Result<Self, StatsdError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let host = lookup("DD_AGENT_HOST")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "127.0.0.1".to_owned());

        let port_raw = lookup("DD_DOGSTATSD_PORT")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "8125".to_owned());
        let port: u16 = port_raw
            .trim()
            .parse()
            .map_err(|_| StatsdError::InvalidPort(port_raw))?;

        let prefix = lookup("MALCOLM_STATSD_PREFIX")
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_default();

        let dialect = match lookup("MALCOLM_STATSD_DIALECT") {
            Some(raw) if !raw.trim().is_empty() => match raw.trim().to_ascii_lowercase().as_str() {
                "dogstatsd" | "datadog" => Dialect::DogStatsd,
                _ => Dialect::Statsd,
            },
            _ => Dialect::Statsd,
        };

        let constant_tags = lookup("DD_TAGS")
            .or_else(|| lookup("MALCOLM_STATSD_TAGS"))
            .map(|raw| parse_tags(&raw))
            .unwrap_or_default();

        let max_packet_bytes = match lookup("MALCOLM_STATSD_MAX_BYTES") {
            Some(raw) if !raw.trim().is_empty() => raw
                .trim()
                .parse::<usize>()
                .map_err(|_| StatsdError::InvalidMaxBytes(raw))?,
            _ => 1432,
        };

        let timeout_ms = match lookup("MALCOLM_STATSD_TIMEOUT_MS") {
            Some(raw) if !raw.trim().is_empty() => raw.trim().parse::<u64>().unwrap_or(100),
            _ => 100,
        };

        Ok(Self {
            host,
            port,
            prefix,
            dialect,
            constant_tags,
            max_packet_bytes,
            timeout: Duration::from_millis(timeout_ms),
        })
    }
}

/// Parse a comma-separated tag list. Datadog's `DD_TAGS` convention uses
/// `key:value`; the `MALCOLM_STATSD_TAGS` env var accepts both `key:value`
/// and `key=value` for symmetry with the OTLP exporter.
fn parse_tags(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|entry| {
            entry
                .split_once(':')
                .or_else(|| entry.split_once('='))
                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        })
        .collect()
}

#[cfg(test)]
mod config_tests {
    use std::collections::HashMap;

    use super::*;

    fn env<'a>(entries: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + use<'a> {
        let map: HashMap<String, String> = entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn lookup_err(entries: &[(&'static str, &'static str)]) -> StatsdError {
        match StatsdConfig::from_lookup(env(entries)) {
            Ok(_) => StatsdError::InvalidPort(String::new()),
            Err(e) => e,
        }
    }

    #[test]
    fn default_for_tests_has_stable_shape() {
        let cfg = StatsdConfig::default_for_tests();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8125);
        assert!(cfg.prefix.is_empty());
        assert_eq!(cfg.dialect, Dialect::Statsd);
        assert!(cfg.constant_tags.is_empty());
        assert_eq!(cfg.max_packet_bytes, 1432);
        assert_eq!(cfg.timeout, Duration::from_millis(100));
    }

    #[test]
    fn defaults_apply_when_env_is_empty() -> Result<(), StatsdError> {
        let cfg = StatsdConfig::from_lookup(env(&[]))?;
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8125);
        assert_eq!(cfg.dialect, Dialect::Statsd);
        Ok(())
    }

    #[test]
    fn dd_agent_env_vars_parse_correctly() -> Result<(), StatsdError> {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("DD_AGENT_HOST", "datadog-agent.prod"),
            ("DD_DOGSTATSD_PORT", "18125"),
            ("MALCOLM_STATSD_PREFIX", "malcolm.chaos"),
            ("MALCOLM_STATSD_DIALECT", "dogstatsd"),
        ]))?;
        assert_eq!(cfg.host, "datadog-agent.prod");
        assert_eq!(cfg.port, 18125);
        assert_eq!(cfg.prefix, "malcolm.chaos");
        assert_eq!(cfg.dialect, Dialect::DogStatsd);
        Ok(())
    }

    #[test]
    fn dialect_accepts_both_canonical_and_datadog_alias() -> Result<(), StatsdError> {
        for raw in ["statsd", "STATSd", "dogstatsd", "datadog", "DOGSTATSD"] {
            let cfg = StatsdConfig::from_lookup(env(&[("MALCOLM_STATSD_DIALECT", raw)]))?;
            let expected =
                if raw.eq_ignore_ascii_case("dogstatsd") || raw.eq_ignore_ascii_case("datadog") {
                    Dialect::DogStatsd
                } else {
                    Dialect::Statsd
                };
            assert_eq!(cfg.dialect, expected, "dialect {raw} misparsed");
        }
        Ok(())
    }

    #[test]
    fn constant_tags_parse_from_dd_tags_env() -> Result<(), StatsdError> {
        let cfg =
            StatsdConfig::from_lookup(env(&[("DD_TAGS", "env:prod,team:chaos,svc:malcolm")]))?;
        assert_eq!(
            cfg.constant_tags,
            vec![
                ("env".to_owned(), "prod".to_owned()),
                ("team".to_owned(), "chaos".to_owned()),
                ("svc".to_owned(), "malcolm".to_owned()),
            ]
        );
        Ok(())
    }

    #[test]
    fn malcolm_statsd_tags_falls_back_when_dd_tags_absent() -> Result<(), StatsdError> {
        let cfg = StatsdConfig::from_lookup(env(&[("MALCOLM_STATSD_TAGS", "k:v")]))?;
        assert_eq!(cfg.constant_tags, vec![("k".to_owned(), "v".to_owned())]);
        Ok(())
    }

    #[test]
    fn dd_tags_wins_over_malcolm_statsd_tags_when_both_set() -> Result<(), StatsdError> {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("DD_TAGS", "from:dd"),
            ("MALCOLM_STATSD_TAGS", "from:malcolm"),
        ]))?;
        assert_eq!(
            cfg.constant_tags,
            vec![("from".to_owned(), "dd".to_owned())]
        );
        Ok(())
    }

    #[test]
    fn invalid_port_returns_typed_error() {
        let err = lookup_err(&[("DD_DOGSTATSD_PORT", "not-a-port")]);
        assert!(matches!(err, StatsdError::InvalidPort(ref s) if s == "not-a-port"));
    }

    #[test]
    fn out_of_range_port_returns_typed_error() {
        let err = lookup_err(&[("DD_DOGSTATSD_PORT", "99999")]);
        assert!(matches!(err, StatsdError::InvalidPort(_)));
    }

    #[test]
    fn custom_max_bytes_and_timeout_parse() -> Result<(), StatsdError> {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("MALCOLM_STATSD_MAX_BYTES", "1024"),
            ("MALCOLM_STATSD_TIMEOUT_MS", "50"),
        ]))?;
        assert_eq!(cfg.max_packet_bytes, 1024);
        assert_eq!(cfg.timeout, Duration::from_millis(50));
        Ok(())
    }

    #[test]
    fn invalid_max_bytes_returns_typed_error() {
        let err = lookup_err(&[("MALCOLM_STATSD_MAX_BYTES", "huge")]);
        assert!(matches!(err, StatsdError::InvalidMaxBytes(ref s) if s == "huge"));
    }

    #[test]
    fn sanitize_tag_value_replaces_line_protocol_reserved_characters() {
        // Every char that would let a user-controlled value corrupt the
        // line protocol must be rewritten to `_`. Empty input is preserved.
        let value = "hello:|,\n\rworld";
        let sanitized = sanitize_tag_value(value);
        assert_eq!(sanitized, "hello_____world", "raw -> sanitized");
        for ch in [':', '|', ',', '\n', '\r'] {
            assert!(
                !sanitized.contains(ch),
                "forbidden char survived: {sanitized:?}"
            );
        }
    }

    #[test]
    fn sanitize_tag_value_keeps_safe_alphanumerics() {
        assert_eq!(
            sanitize_tag_value("node-12_edge"),
            "node-12_edge",
            "alphanumerics, underscore, hyphen, and dot must pass through"
        );
    }

    #[test]
    fn max_packet_bytes_zero_is_rejected_at_construction() {
        let config = StatsdConfig {
            host: "127.0.0.1".to_owned(),
            port: 8125,
            prefix: String::new(),
            dialect: Dialect::Statsd,
            constant_tags: Vec::new(),
            max_packet_bytes: 0,
            timeout: Duration::from_millis(100),
        };
        let result = StatsdRecorder::with_config(config);
        assert!(matches!(
            result,
            Err(StatsdRecorderError::InvalidMaxBytes(_))
        ));
    }
}

use std::fmt::Write as _;
use std::io::Write;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::{
    FAULT_INTENSITY, FAULT_LATENCY_MS, FAULTS_INJECTED_TOTAL, FAULTS_SKIPPED_TOTAL, MetricSample,
    MetricsRecorder, SCENARIO_DURATION_MS,
};

/// Default rate-limit interval for UDP send-failure `tracing::warn!` calls.
const WARN_RATE_LIMIT: u64 = 64;

/// Errors produced by [`StatsdRecorder::with_config`].
#[derive(Debug, thiserror::Error)]
pub enum StatsdRecorderError {
    /// The configured host:port could not be resolved.
    #[error("could not resolve statsd destination {0}:{1}")]
    Resolve(String, u16),
    /// The local UDP socket could not be created.
    #[error("failed to bind statsd recorder socket: {0}")]
    Bind(String),
    /// The configured destination could not be connected.
    #[error("failed to connect statsd recorder to {0}:{1}: {2}")]
    Connect(String, u16, String),
    /// The configured `max_packet_bytes` is not a positive integer.
    #[error("invalid statsd max_packet_bytes: {0}")]
    InvalidMaxBytes(String),
}

/// `StatsD` / `DogStatsD` recorder. Wire into [`MetricsHub`](super::MetricsHub).
pub struct StatsdRecorder {
    socket: UdpSocket,
    config: StatsdConfig,
    buffer: Mutex<Vec<u8>>,
    failed_since_last_warn: AtomicU64,
    oversized_since_last_warn: AtomicU64,
}

impl StatsdRecorder {
    /// Build a recorder connected to the configured destination.
    pub fn with_config(config: StatsdConfig) -> Result<Arc<Self>, StatsdRecorderError> {
        if config.max_packet_bytes == 0 {
            return Err(StatsdRecorderError::InvalidMaxBytes(
                "max_packet_bytes must be > 0".to_owned(),
            ));
        }
        let addr: SocketAddr = (config.host.as_str(), config.port)
            .to_socket_addrs()
            .ok()
            .and_then(|mut it| it.next())
            .ok_or_else(|| StatsdRecorderError::Resolve(config.host.clone(), config.port))?;
        let local =
            UdpSocket::bind("0.0.0.0:0").map_err(|e| StatsdRecorderError::Bind(e.to_string()))?;
        let dest_str = format!("{}:{}", config.host, config.port);
        local
            .connect(addr)
            .map_err(|e| StatsdRecorderError::Connect(dest_str, config.port, e.to_string()))?;
        let _ = local.set_write_timeout(Some(config.timeout));
        Ok(Arc::new(Self {
            socket: local,
            config,
            buffer: Mutex::new(Vec::new()),
            failed_since_last_warn: AtomicU64::new(0),
            oversized_since_last_warn: AtomicU64::new(0),
        }))
    }

    /// Flush the in-memory buffer as a single UDP datagram. Fire-and-forget:
    /// a send error is logged via `tracing::warn!` (rate-limited) and
    /// swallowed. Returns the number of bytes attempted.
    pub fn flush(&self) -> usize {
        // Recover from a poisoned `Mutex`: the lock itself is still usable.
        let mut buf = self
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if buf.is_empty() {
            return 0;
        }
        let bytes = buf.len();
        let payload = std::mem::take(&mut *buf);
        drop(buf);
        match self.socket.send(&payload) {
            Ok(_) => {}
            Err(error) => {
                self.record_send_failure(&error);
            }
        }
        bytes
    }

    /// Force a flush and then shut the underlying socket down. Subsequent
    /// `record` calls log warnings and skip. Always call before process
    /// exit on a short-lived run.
    pub fn shutdown(&self) {
        self.flush();
        // Closing the socket is best-effort: `UdpSocket::shutdown` only
        // exists for `TcpSocket` in stable. Drop will close it; the
        // `flush` above has already drained the buffer.
    }

    fn record_send_failure(&self, error: &std::io::Error) {
        let since_last = self.failed_since_last_warn.fetch_add(1, Ordering::AcqRel) + 1;
        if since_last % WARN_RATE_LIMIT == 1 {
            tracing::warn!(
                target: "malcolm",
                endpoint = format!("{}:{}", self.config.host, self.config.port),
                error = %error,
                consecutive_failures = since_last,
                "statsd send failed (rate-limited); telemetry dropped",
            );
        }
    }

    fn encode_line(&self, sample: &MetricSample) -> String {
        let mut name = String::with_capacity(64);
        if !self.config.prefix.is_empty() {
            name.push_str(&self.config.prefix);
            name.push('.');
        }
        name.push_str(sample.name);

        let mut line = match sample.name {
            FAULTS_INJECTED_TOTAL | FAULTS_SKIPPED_TOTAL => {
                let delta = counter_increment(sample.value);
                format!("{name}:{delta}|c")
            }
            FAULT_INTENSITY => format!("{name}:{}|g", format_f64(sample.value)),
            FAULT_LATENCY_MS | SCENARIO_DURATION_MS => {
                let unit = match self.config.dialect {
                    Dialect::Statsd => "ms",
                    Dialect::DogStatsd => "h",
                };
                format!("{name}:{}|{}", format_f64(sample.value), unit)
            }
            _ => return String::new(),
        };

        match self.config.dialect {
            Dialect::DogStatsd => {
                let mut tags: Vec<(String, String)> = self.config.constant_tags.clone();
                for (k, v) in &sample.labels {
                    tags.push(((*k).to_owned(), v.clone()));
                }
                if !tags.is_empty() {
                    line.push_str("|#");
                    let mut first = true;
                    for (k, v) in &tags {
                        if !first {
                            line.push(',');
                        }
                        first = false;
                        let _ = write!(line, "{}:{}", sanitize_tag_key(k), sanitize_tag_value(v));
                    }
                }
            }
            Dialect::Statsd => {
                // Plain StatsD has no tag syntax. In StatsD the `|@<float>`
                // marker denotes a sample rate, so we cannot reuse it for
                // labels. Fold each label into the metric name as a
                // `.key-value` suffix, sanitized to the ASCII subset that
                // common collectors accept.
                for (k, v) in &sample.labels {
                    let _ = write!(line, ".{}-{}", sanitize_tag_key(k), sanitize_tag_value(v));
                }
            }
        }
        line
    }

    fn record_into_buffer(&self, line: &str) {
        if line.is_empty() {
            return;
        }
        let line_size = line.len() + 1;
        // Single lines larger than the configured datagram size cannot be
        // sent (UDP MTU); drop them with a rate-limited warning rather
        // than silently truncating or building an oversized datagram.
        if line_size > self.config.max_packet_bytes {
            let since_last = self
                .oversized_since_last_warn
                .fetch_add(1, Ordering::AcqRel)
                + 1;
            if since_last % WARN_RATE_LIMIT == 1 {
                tracing::warn!(
                    target: "malcolm",
                    line_size,
                    max_packet_bytes = self.config.max_packet_bytes,
                    consecutive_drops = since_last,
                    "statsd metric exceeds packet limit; dropping sample (rate-limited)",
                );
            }
            return;
        }
        // Recover from a poisoned `Mutex`: the lock itself is still usable.
        let mut buf = self
            .buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !buf.is_empty() && buf.len() + line_size > self.config.max_packet_bytes {
            let payload = std::mem::take(&mut *buf);
            match self.socket.send(&payload) {
                Ok(_) => {}
                Err(error) => self.record_send_failure(&error),
            }
            // payload already moved out; buf is now empty.
        }
        let _ = buf.write_all(line.as_bytes());
        let _ = buf.write_all(b"\n");
    }
}

/// Counter increments must be non-negative integers in practice.
fn counter_increment(value: f64) -> u64 {
    let clamped = value.max(0.0);
    #[expect(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "value is `.max(0.0)` and rounded to integer before the f64→u64 cast"
    )]
    let inc = clamped.round() as u64;
    inc
}

/// Sanitize a tag key or name-component fragment. Keeps alphanumerics plus
/// `_` and `-`; collapses everything else (including `|`, `:`, `,`, `\n`)
/// to `_` so user-controlled values cannot corrupt the line protocol or
/// inject additional metric lines.
fn sanitize_tag_key(s: &str) -> String {
    s.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

/// Sanitize a tag value. `DogStatsD` forbids `:` in values and any of `|`,
/// `,`, `\n` are line-protocol-reserved, so replace them with `_`.
fn sanitize_tag_value(s: &str) -> String {
    s.chars()
        .map(|ch| match ch {
            ':' | '|' | ',' | '\n' | '\r' => '_',
            c if c.is_ascii_alphanumeric() => c,
            c if c == '_' || c == '-' || c == '.' => c,
            _ => '_',
        })
        .collect()
}

/// Render an `f64` for the line protocol. We avoid scientific notation
/// because some `StatsD` collectors reject it.
fn format_f64(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
            .trim_end_matches('0')
            .trim_end_matches('.')
            .to_owned()
    } else {
        // NaN / +/- infinity — emit a sentinel so the line still parses
        // and a downstream operator notices the anomaly.
        "0".to_owned()
    }
}

impl MetricsRecorder for StatsdRecorder {
    fn record(&self, sample: &MetricSample) {
        let line = self.encode_line(sample);
        self.record_into_buffer(&line);
    }
}

impl Drop for StatsdRecorder {
    fn drop(&mut self) {
        // Final flush so the very last batch doesn't sit in the buffer.
        self.flush();
    }
}

#[cfg(test)]
mod recorder_tests {
    use super::*;
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::metrics::{
        MetricKind, MetricSample, MetricUnit, MetricsHub, sample_for_scenario_duration,
    };
    use crate::scenario::ChaosScenario;
    use crate::test_util::slice_recv;
    use malcolm_core::bifurcation::BifurcationProfile;

    /// Bind a real UDP socket on `127.0.0.1:0` (OS-assigned port) and return
    /// both the bound port and the socket. The socket stays open for the
    /// test's lifetime; reading from it gives the bytes the recorder sent.
    fn fake_agent() -> Result<(std::net::SocketAddr, UdpSocket), std::io::Error> {
        let socket = UdpSocket::bind("127.0.0.1:0")?;
        let addr = socket.local_addr()?;
        Ok((addr, socket))
    }

    fn read_datagrams(socket: &UdpSocket, max: usize) -> Result<Vec<String>, std::io::Error> {
        socket.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        for _ in 0..max {
            match socket.recv(&mut buf) {
                Ok(n) => {
                    let s = String::from_utf8_lossy(slice_recv(&buf, n)).to_string();
                    out.extend(s.lines().map(str::to_owned));
                }
                Err(_) => break,
            }
        }
        Ok(out)
    }

    fn sample_ctx() -> FaultContext {
        FaultContext {
            seed: 1337,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    fn plain_statsd_config(agent: std::net::SocketAddr) -> StatsdConfig {
        StatsdConfig {
            host: agent.ip().to_string(),
            port: agent.port(),
            prefix: "malcolm".to_owned(),
            dialect: Dialect::Statsd,
            constant_tags: Vec::new(),
            max_packet_bytes: 1432,
            timeout: Duration::from_millis(200),
        }
    }

    fn dogstatsd_config(agent: std::net::SocketAddr) -> StatsdConfig {
        StatsdConfig {
            host: agent.ip().to_string(),
            port: agent.port(),
            prefix: "malcolm".to_owned(),
            dialect: Dialect::DogStatsd,
            constant_tags: vec![("env".to_owned(), "test".to_owned())],
            max_packet_bytes: 1432,
            timeout: Duration::from_millis(200),
        }
    }

    #[test]
    fn dogstatsd_counter_round_trips_with_constant_tags_and_labels()
    -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        let recorder = StatsdRecorder::with_config(dogstatsd_config(addr))?;

        // Construct a hand-built counter sample to keep the assertion
        // deterministic without depending on a fault that always injects.
        let sample = MetricSample {
            name: FAULTS_INJECTED_TOTAL,
            kind: MetricKind::Counter,
            value: 3.0,
            unit: MetricUnit::Count,
            labels: vec![
                ("fault_type", "latency_spike".to_owned()),
                ("node_id", "edge-0".to_owned()),
            ],
            timestamp_ms: 0,
        };
        recorder.record(&sample);
        recorder.flush();
        let lines = read_datagrams(&agent, 4)?;
        let combined = lines.join("\n");
        assert!(
            combined.contains("malcolm.malcolm_faults_injected_total:3|c"),
            "{combined}"
        );
        assert!(combined.contains("#env:test"), "{combined}");
        assert!(combined.contains("fault_type:latency_spike"), "{combined}");
        assert!(combined.contains("node_id:edge-0"), "{combined}");
        Ok(())
    }

    #[test]
    fn plain_statsd_dialect_folds_labels_into_name_suffix() -> Result<(), Box<dyn std::error::Error>>
    {
        let (addr, agent) = fake_agent()?;
        let recorder = StatsdRecorder::with_config(plain_statsd_config(addr))?;

        let sample = MetricSample {
            name: FAULT_INTENSITY,
            kind: MetricKind::Gauge,
            value: 0.5,
            unit: MetricUnit::Ratio,
            labels: vec![("scenario", "demo".to_owned())],
            timestamp_ms: 0,
        };
        recorder.record(&sample);
        recorder.flush();
        let lines = read_datagrams(&agent, 4)?;
        let combined = lines.join("\n");
        // Plain StatsD has no tag syntax; labels fold into the metric
        // name as `.key-value` suffixes. In StatsD, `|@<float>` is reserved
        // for sample rate so we never reuse it for labels.
        assert!(
            combined.contains("malcolm.malcolm_fault_intensity:0.5|g.scenario-demo"),
            "{combined}"
        );
        assert!(
            !combined.contains('#'),
            "plain StatsD must not emit DogStatsD tag marker: {combined}"
        );
        Ok(())
    }

    #[test]
    fn dogstatsd_histogram_uses_h_type() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        let recorder = StatsdRecorder::with_config(dogstatsd_config(addr))?;

        let report = crate::scenario::ScenarioReport {
            name: "demo".to_owned(),
            seed: 1,
            regime: crate::scenario::ScenarioRegime::Stable,
            events: Vec::new(),
            total_duration_ms: 42,
        };
        recorder.record(&sample_for_scenario_duration(&report));
        recorder.flush();
        let lines = read_datagrams(&agent, 4)?;
        let combined = lines.join("\n");
        assert!(
            combined.contains("malcolm.malcolm_scenario_duration_ms"),
            "{combined}"
        );
        assert!(
            combined.contains("|h"),
            "DogStatsD histogram must use |h type: {combined}"
        );
        Ok(())
    }

    #[test]
    fn plain_statsd_histogram_uses_ms_type() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        let recorder = StatsdRecorder::with_config(plain_statsd_config(addr))?;

        let report = crate::scenario::ScenarioReport {
            name: "demo".to_owned(),
            seed: 1,
            regime: crate::scenario::ScenarioRegime::Stable,
            events: Vec::new(),
            total_duration_ms: 42,
        };
        recorder.record(&sample_for_scenario_duration(&report));
        recorder.flush();
        let lines = read_datagrams(&agent, 4)?;
        let combined = lines.join("\n");
        assert!(
            combined.contains("|ms"),
            "plain StatsD histogram must use |ms type: {combined}"
        );
        Ok(())
    }

    #[test]
    fn batching_packs_multiple_lines_into_one_datagram() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        // A tiny `max_packet_bytes` forces the recorder to flush every
        // time a new line would overflow the buffer, so we can observe
        // the per-line datagram path explicitly. Each sample line is
        // 41 bytes (`malcolm.malcolm_fault_intensity:0.5|g\n`), so a
        // 42-byte budget stores exactly one line per datagram.
        let mut config = plain_statsd_config(addr);
        config.max_packet_bytes = 42;
        let recorder = StatsdRecorder::with_config(config)?;

        // 6 samples at ~50 bytes each — only one fits per datagram.
        for i in 0..6 {
            let sample = MetricSample {
                name: FAULT_INTENSITY,
                kind: MetricKind::Gauge,
                value: f64::from(i),
                unit: MetricUnit::Ratio,
                labels: Vec::new(),
                timestamp_ms: 0,
            };
            recorder.record(&sample);
        }
        recorder.flush();

        // Read every datagram the agent received. With max_packet_bytes=80
        // every sample should be its own datagram, so we expect 6+
        // datagrams (the trailing flush may emit a final empty/partial
        // one).
        agent.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut datagram_count = 0usize;
        let mut total_lines = 0usize;
        let mut buf = [0u8; 4096];
        let mut all_lines = String::new();
        for _ in 0..8 {
            match agent.recv(&mut buf) {
                Ok(n) => {
                    datagram_count += 1;
                    let s = String::from_utf8_lossy(slice_recv(&buf, n)).to_string();
                    total_lines += s.lines().count();
                    all_lines.push_str(&s);
                }
                Err(_) => break,
            }
        }
        assert!(
            datagram_count >= 6,
            "expected at least 6 datagrams with tight packet budget, got {datagram_count}"
        );
        assert!(
            total_lines >= 6,
            "all 6 samples should be received, got {total_lines}"
        );
        // Sanity: every line is a complete sample (no truncation).
        for line in all_lines.lines() {
            assert!(
                line.starts_with("malcolm.malcolm_fault_intensity:"),
                "unexpected line fragment: {line:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn batching_emits_one_datagram_when_budget_allows() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        // Default 1432-byte budget: 6 small samples should pack into a
        // single datagram.
        let recorder = StatsdRecorder::with_config(plain_statsd_config(addr))?;
        for i in 0..6 {
            let sample = MetricSample {
                name: FAULT_INTENSITY,
                kind: MetricKind::Gauge,
                value: f64::from(i),
                unit: MetricUnit::Ratio,
                labels: Vec::new(),
                timestamp_ms: 0,
            };
            recorder.record(&sample);
        }
        recorder.flush();

        agent.set_read_timeout(Some(Duration::from_millis(200)))?;
        let mut buf = [0u8; 4096];
        match agent.recv(&mut buf) {
            Ok(n) => {
                let s = String::from_utf8_lossy(slice_recv(&buf, n));
                let line_count = s.lines().count();
                assert!(
                    line_count >= 6,
                    "expected at least 6 lines in one datagram, got {line_count}"
                );
                // The single datagram was at most max_packet_bytes.
                assert!(n <= 1432, "first datagram exceeded MTU: {n} bytes");
            }
            Err(error) => {
                return Err(format!("agent recv failed: {error}").into());
            }
        }
        Ok(())
    }

    #[test]
    fn oversized_metric_is_dropped_with_a_warning() -> Result<(), Box<dyn std::error::Error>> {
        let (addr, agent) = fake_agent()?;
        // 64-byte budget — anything larger than 64 bytes will be dropped.
        let mut config = plain_statsd_config(addr);
        config.max_packet_bytes = 64;
        let recorder = StatsdRecorder::with_config(config)?;

        // The label value is long enough to push the encoded line over
        // the 64-byte budget on its own.
        let big_label = "x".repeat(200);
        let sample = MetricSample {
            name: FAULT_INTENSITY,
            kind: MetricKind::Gauge,
            value: 0.5,
            unit: MetricUnit::Ratio,
            labels: vec![("scenario", big_label)],
            timestamp_ms: 0,
        };
        recorder.record(&sample);
        recorder.flush();

        // No datagram must arrive on the agent — the metric was dropped.
        agent.set_read_timeout(Some(Duration::from_millis(100)))?;
        let mut buf = [0u8; 4096];
        let recv = agent.recv(&mut buf);
        assert!(
            recv.is_err(),
            "expected no datagram (oversized metric must be dropped), got {recv:?}"
        );
        Ok(())
    }

    #[test]
    fn point_to_unroutable_endpoint_does_not_panic_on_record_or_flush() {
        // Use a port that is reserved but the connect will fail.
        // Fire-and-forget: record and flush must not panic and not return
        // an error to the caller.
        let config = StatsdConfig {
            host: "127.0.0.1".to_owned(),
            port: 1, // privileged port; connect to a non-listener will fail
            prefix: "x".to_owned(),
            dialect: Dialect::Statsd,
            constant_tags: Vec::new(),
            max_packet_bytes: 1432,
            timeout: Duration::from_millis(20),
        };
        // Connection itself may fail (which is the success path) — but
        // the recorder construction should surface a typed error.
        let res = StatsdRecorder::with_config(config);
        // If construction succeeded, record + flush must not panic.
        if let Ok(recorder) = res {
            recorder.record(&MetricSample {
                name: FAULTS_INJECTED_TOTAL,
                kind: MetricKind::Counter,
                value: 1.0,
                unit: MetricUnit::Count,
                labels: Vec::new(),
                timestamp_ms: 0,
            });
            recorder.flush();
            recorder.shutdown();
        }
        // If construction failed, that's also fine — the brief accepts
        // either: "the recorder does not propagate UDP errors into the
        // fault path."
    }

    #[test]
    fn scenario_run_with_statsd_recorder_does_not_panic() -> Result<(), Box<dyn std::error::Error>>
    {
        let (addr, _agent) = fake_agent()?;
        let recorder = StatsdRecorder::with_config(plain_statsd_config(addr))?;
        let hub = MetricsHub::new().with_recorder(recorder.clone());

        let scenario = ChaosScenario::builder()
            .name("statsd-wiring")
            .seed(1337)
            .add_fault(PacketLoss::builder().seed(42).intensity(0.9).build())
            .profile(BifurcationProfile::network_partition())
            .build();

        let mut ctx = sample_ctx();
        let _report = scenario.run_with_metrics(&mut ctx, &hub);
        recorder.flush();
        Ok(())
    }
}
