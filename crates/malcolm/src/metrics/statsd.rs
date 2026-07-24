//! StatsD / DogStatsD line-protocol exporter (feature `statsd`).
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
    /// Plain StatsD — no tag support, labels fold into the metric name.
    Statsd,
    /// DogStatsD — `|#k1:v1,k2:v2` tag syntax, native histogram support.
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

/// Typed configuration for the StatsD / DogStatsD exporter.
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
                "statsd" => Dialect::Statsd,
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
    fn defaults_apply_when_env_is_empty() {
        let cfg = StatsdConfig::from_lookup(env(&[])).expect("empty env should produce defaults");
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8125);
        assert_eq!(cfg.dialect, Dialect::Statsd);
    }

    #[test]
    fn dd_agent_env_vars_parse_correctly() {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("DD_AGENT_HOST", "datadog-agent.prod"),
            ("DD_DOGSTATSD_PORT", "18125"),
            ("MALCOLM_STATSD_PREFIX", "malcolm.chaos"),
            ("MALCOLM_STATSD_DIALECT", "dogstatsd"),
        ]))
        .expect("explicit env must parse");
        assert_eq!(cfg.host, "datadog-agent.prod");
        assert_eq!(cfg.port, 18125);
        assert_eq!(cfg.prefix, "malcolm.chaos");
        assert_eq!(cfg.dialect, Dialect::DogStatsd);
    }

    #[test]
    fn dialect_accepts_both_canonical_and_datadog_alias() {
        for raw in ["statsd", "STATSd", "dogstatsd", "datadog", "DOGSTATSD"] {
            let cfg = StatsdConfig::from_lookup(env(&[("MALCOLM_STATSD_DIALECT", raw)]))
                .expect("dialect variants must parse");
            let expected =
                if raw.eq_ignore_ascii_case("dogstatsd") || raw.eq_ignore_ascii_case("datadog") {
                    Dialect::DogStatsd
                } else {
                    Dialect::Statsd
                };
            assert_eq!(cfg.dialect, expected, "dialect {raw} misparsed");
        }
    }

    #[test]
    fn constant_tags_parse_from_dd_tags_env() {
        let cfg = StatsdConfig::from_lookup(env(&[("DD_TAGS", "env:prod,team:chaos,svc:malcolm")]))
            .expect("DD_TAGS env must parse");
        assert_eq!(
            cfg.constant_tags,
            vec![
                ("env".to_owned(), "prod".to_owned()),
                ("team".to_owned(), "chaos".to_owned()),
                ("svc".to_owned(), "malcolm".to_owned()),
            ]
        );
    }

    #[test]
    fn malcolm_statsd_tags_falls_back_when_dd_tags_absent() {
        let cfg = StatsdConfig::from_lookup(env(&[("MALCOLM_STATSD_TAGS", "k:v")]))
            .expect("MALCOLM_STATSD_TAGS must parse");
        assert_eq!(cfg.constant_tags, vec![("k".to_owned(), "v".to_owned())]);
    }

    #[test]
    fn dd_tags_wins_over_malcolm_statsd_tags_when_both_set() {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("DD_TAGS", "from:dd"),
            ("MALCOLM_STATSD_TAGS", "from:malcolm"),
        ]))
        .expect("both env vars must parse");
        assert_eq!(
            cfg.constant_tags,
            vec![("from".to_owned(), "dd".to_owned())]
        );
    }

    #[test]
    fn invalid_port_returns_typed_error() {
        let err = StatsdConfig::from_lookup(env(&[("DD_DOGSTATSD_PORT", "not-a-port")]))
            .expect_err("non-numeric port must error");
        assert!(matches!(err, StatsdError::InvalidPort(ref s) if s == "not-a-port"));
    }

    #[test]
    fn out_of_range_port_returns_typed_error() {
        let err = StatsdConfig::from_lookup(env(&[("DD_DOGSTATSD_PORT", "65536")]))
            .expect_err("port > 65535 must error");
        assert!(matches!(err, StatsdError::InvalidPort(_)));
    }

    #[test]
    fn custom_max_bytes_and_timeout_parse() {
        let cfg = StatsdConfig::from_lookup(env(&[
            ("MALCOLM_STATSD_MAX_BYTES", "1024"),
            ("MALCOLM_STATSD_TIMEOUT_MS", "50"),
        ]))
        .expect("explicit values must parse");
        assert_eq!(cfg.max_packet_bytes, 1024);
        assert_eq!(cfg.timeout, Duration::from_millis(50));
    }

    #[test]
    fn invalid_max_bytes_returns_typed_error() {
        let err = StatsdConfig::from_lookup(env(&[("MALCOLM_STATSD_MAX_BYTES", "huge")]))
            .expect_err("non-numeric max must error");
        assert!(matches!(err, StatsdError::InvalidMaxBytes(ref s) if s == "huge"));
    }
}
