# Metrics seam

malcolm exposes a small, dependency-light **metrics recorder seam** in
`malcolm::metrics`. Every fault emission produces a structured `tracing` event;
the recorder seam is the parallel mechanism for emitting *samples* that
downstream exporters (Prometheus, OpenTelemetry, StatsD) can aggregate.

The seam is **always compiled in** with a no-op default recorder, so callers
that don't install anything observe identical behavior to a build without
metrics. Concrete exporters (T27, T28, T29) live behind feature flags.

## Taxonomy

All metric names are `pub const` strings in [`malcolm::metrics`](../../crates/malcolm/src/metrics.rs).
Every exporter must use the same constants so labels and units stay stable
across backends.

| Name | Kind | Unit | Emitted when | Labels |
|------|------|------|--------------|--------|
| `malcolm_faults_injected_total` | Counter | Count | a fault's `inject` returns `FaultResult::Injected` | `fault_type`, `node_id`, `scenario`, `regime`, `dry_run` |
| `malcolm_faults_skipped_total` | Counter | Count | a fault's `inject` returns `FaultResult::Skipped` | `fault_type`, `node_id`, `scenario`, `skip_reason` |
| `malcolm_fault_intensity` | Gauge | Ratio | a fault is injected | `fault_type`, `node_id`, `scenario` |
| `malcolm_fault_latency_ms` | Histogram | Milliseconds | a fault reports a sampled latency | `fault_type`, `node_id` |
| `malcolm_scenario_duration_ms` | Histogram | Milliseconds | a scenario finishes | `scenario`, `regime` |

The `regime` label is one of `stable`, `sensitive`, `chaotic`. The `skip_reason`
label is one of `below_threshold`, `dry_run`, `cancelled`.

## Wiring

`ChaosScenario::run_with_metrics(&mut ctx, &MetricsHub)` walks every fault,
records the corresponding samples, and emits a duration histogram for the
whole scenario at the end. The original `ChaosScenario::run(&mut ctx)` is a
thin wrapper that calls `run_with_metrics` with an empty hub — so behavior is
unchanged when no recorder is installed.

## Writing a custom recorder

```rust
use std::sync::{Arc, Mutex};
use malcolm::metrics::{MetricSample, MetricsHub, MetricsRecorder};

#[derive(Default)]
struct CollectingRecorder(Arc<Mutex<Vec<MetricSample>>>);

impl MetricsRecorder for CollectingRecorder {
    fn record(&self, sample: &MetricSample) {
        self.0.lock().expect("poisoned").push(sample.clone());
    }
}

let recorder = Arc::new(CollectingRecorder::default());
let hub = MetricsHub::new().with_recorder(recorder.clone());
scenario.run_with_metrics(&mut ctx, &hub);
```

The hub itself implements `MetricsRecorder`, so hubs nest via
`with_recorder(other_hub)`. The fan-out order matches registration order; a hub
with zero recorders records nothing without panicking.

## Sample shape

```rust
pub struct MetricSample {
    pub name: &'static str,         // canonical name from the taxonomy
    pub kind: MetricKind,            // Counter | Gauge | Histogram
    pub value: f64,
    pub unit: MetricUnit,            // Count | Milliseconds | Ratio | Bytes
    pub labels: Vec<(&'static str, String)>,
    pub timestamp_ms: u64,           // caller-supplied, replay-safe
}
```

`value` semantics depend on `kind`: counters add, gauges replace, histograms
observe. Exporters convert to their wire format (`StatsD` takes ms as-is;
Prometheus expects base units in seconds).

## Default behavior

`MetricsHub::new()` is empty and records nothing. `NoopRecorder` is the
default everywhere; it accepts any sample without side effects.

## Exporter status

| Exporter | Task | Status | Feature |
|----------|------|--------|---------|
| Prometheus | T27 | shipped | `prometheus` |
| OpenTelemetry / OTLP | T28 | shipped | `otel`, `otel-grpc`, `otel-http` |
| StatsD / Datadog | T29 | shipped | `statsd` |

## Prometheus (T27)

`PrometheusRecorder` translates the canonical taxonomy into Prometheus time
series and renders the standard text exposition format on demand. It's
feature-gated behind `prometheus` so the default build pulls no extra deps.

```rust
use std::sync::Arc;
use malcolm::metrics::prometheus::PrometheusRecorder;
use malcolm::metrics::MetricsHub;
use malcolm::scenario::ChaosScenario;

let recorder = Arc::new(PrometheusRecorder::new());
let hub = MetricsHub::new().with_recorder(recorder.clone());
scenario.run_with_metrics(&mut ctx, &hub);

// Scrape body for a Prometheus server:
let body = recorder.gather_text()?;
```

Or one-liner via [`PrometheusRecorder::into_hub`]:

```rust
let hub = PrometheusRecorder::new().into_hub();
scenario.run_with_metrics(&mut ctx, &hub);
```

### Exposing over HTTP

`gather_text()` returns the standard text format. Embed it in any HTTP server:

```rust
// e.g. axum
async fn metrics() -> impl IntoResponse {
    recorder.gather_text().unwrap_or_default()
}
```

Sample Prometheus `scrape_config`:

```yaml
scrape_configs:
  - job_name: malcolm
    static_configs:
      - targets: ['localhost:9000']
    metrics_path: /metrics
    scrape_interval: 15s
```

### Cardinality

`node_id` and `scenario` are user-controlled strings. For high-cardinality
deployments, swap to `PrometheusRecorder::dropping_high_cardinality_labels()`
to strip `node_id` from every series and aggregate per `(fault_type, scenario,
regime)` instead.

### Bucket choice

`malcolm_fault_latency_ms` and `malcolm_scenario_duration_ms` use an
exponential bucket layout from 1ms to ~60s (25 buckets across six decades).
Values are stored in milliseconds — operators reading a scrape can multiply
by `1e-3` to convert to base-unit seconds.

## OpenTelemetry / OTLP (T28)

`malcolm::metrics::otel` ships two pieces:

- **`OtelRecorder`** — translates the canonical taxonomy into OTel metric
  instruments (`Counter<u64>`, `Gauge<f64>`, `Histogram<f64>`) and emits
  through an `SdkMeterProvider`. Pair with a `PeriodicReader` over OTLP for
  production (T28f).
- **`otel_tracing_layer`** — bridges every `tracing` event on the
  `malcolm` target into an OTel span via `tracing-opentelemetry`, so chaos
  runs show up as trace spans alongside the metrics.

### Wiring

```toml
[dependencies]
malcolm = { version = "0.6", features = ["otel", "otel-grpc"] }
# or feature = ["otel", "otel-http"] for HTTP/protobuf transport
```

```rust
use malcolm::metrics::otel::{OtelConfig, otel_tracing_layer};
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_subscriber::layer::SubscriberExt;

let tracer_provider = SdkTracerProvider::builder().build();
let layer = otel_tracing_layer(&tracer_provider);
tracing_subscriber::registry().with(layer).init();
```

Configure the OTLP endpoint via the standard env vars:

| Variable | Maps to |
|----------|---------|
| `OTEL_EXPORTER_OTLP_ENDPOINT` | `endpoint` (required) |
| `OTEL_EXPORTER_OTLP_PROTOCOL` | `grpc` (default) or `http`/`http-protobuf` |
| `OTEL_EXPORTER_OTLP_HEADERS` | comma-separated `k=v` headers |
| `OTEL_SERVICE_NAME` | resource service name (default `malcolm`) |
| `OTEL_EXPORTER_OTLP_TIMEOUT` | export timeout in seconds (default `10`) |

### Local collector smoke test

```bash
docker run --rm -p 4317:4317 -p 4318:4318 \
  -v $PWD/examples/otel-collector.yaml:/etc/otelcol/config.yaml \
  otel/opentelemetry-collector-contrib:0.114.0
```

Send one scenario run; metrics and spans show up in the configured backend.

### Force flush before exit

Short-lived CI runs can drop the last metric batch on process exit. Call
`OtelRecorder::force_flush()` and `OtelRecorder::shutdown()` before exit:

```rust
recorder.force_flush()?;
recorder.shutdown()?;
```

`shutdown()` is idempotent at the `OtelRecorder` layer; calling it twice
returns `Ok(())`.

## StatsD / Datadog (T29)

`StatsdRecorder` is the lightweight, fire-and-forget counterpart to the
OpenTelemetry exporter. Pick it when you already ship to a Datadog Agent or
any `StatsD`-compatible collector and don't want the full OTel SDK on the
runtime path. Hand-written line-protocol encoder over `std::net::UdpSocket`
— no third-party dependency.

Enable it from the rest of the workspace:

```toml
[dependencies]
malcolm = { version = "0.6", features = ["statsd"] }
```

### Configuration

Both `StatsdConfig::default_for_tests()` and `StatsdConfig::from_env()` are
available. The env reading follows Datadog conventions:

| Variable | Default | Notes |
|----------|---------|-------|
| `DD_AGENT_HOST` | `127.0.0.1` | Hostname or IP for the agent |
| `DD_DOGSTATSD_PORT` | `8125` | UDP port the agent listens on |
| `MALCOLM_STATSD_PREFIX` | empty | Metric-name prefix (e.g. `malcolm.chaos`) |
| `MALCOLM_STATSD_DIALECT` | `statsd` | `statsd` or `dogstatsd` |
| `MALCOLM_STATSD_MAX_BYTES` | `1432` | Max UDP datagram payload (safe Ethernet MTU) |
| `MALCOLM_STATSD_TIMEOUT_MS` | `100` | UDP connect timeout |
| `DD_TAGS` / `MALCOLM_STATSD_TAGS` | empty | Comma-separated `k=v` constant tags |

### Dialects

- **Plain StatsD** — fold label values into the metric name as
  `.key-value` suffixes. The `|@<float>` marker is reserved for sample rate
  in `StatsD`, so labels cannot reuse it.
- **DogStatsD** — emit native `|#key:value,key:value` tags. Histograms use
  the `|h` type; plain StatsD uses `|ms`.

Both paths sanitize tag values to the ASCII subset that common collectors
accept (`a-zA-Z0-9_-.` plus `,` between key/value pairs). User-controlled
values containing `|`, `:`, `,`, `\n`, or any other line-protocol-reserved
character are rewritten to `_`, so injection and unintended line splitting
are impossible.

### Fire-and-forget

UDP semantics; `Daemon`-style reliability without blocking the runtime.
Construction failures (destination unreachable, `max_packet_bytes = 0`, etc.)
surface as `StatsdRecorderError` so CI can detect a misconfigured agent.
Runtime send failures are logged via `tracing::warn!` (rate-limited) and
swallowed — they never fail a chaos run.

### Datagram batching

Multiple samples can pack into a single datagram up to `max_packet_bytes`
(matches Ethernet MTU minus headers). Single lines larger than
`max_packet_bytes` are dropped with a `tracing::warn!` message rather than
being truncated or sent oversized.

### OTel preference

Datadog deployments already using OTLP should prefer the OpenTelemetry
exporter (T28) — both metrics and traces flow through one SDK. The
`StatsdRecorder` is for teams that want the minimal dependency footprint
and already have a Datadog Agent ingesting UDP traffic.

### Datadog Agent configuration

A representative `datadog.yaml` snippet:

```yaml
dogstatsd_mapper_profiles:
  - name: malcolm
    prefix: "malcolm."
    tags:
      service: malcolm
      env: chaos
```

Make sure `dogstatsd_non_local_traffic: true` is set if the agent is on a
different host, and the listener port (`8125` by default) accepts UDP.
If the agent is on `127.0.0.1` only, that's the default — no extra config
needed for local testing.
||||||| parent of aea43d2 (feat(metrics): add MetricsRecorder seam and scenario metric emission (#31))
