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
