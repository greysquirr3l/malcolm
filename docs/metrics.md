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
| OpenTelemetry / OTLP | T28 | planned | `otel`, `otel-grpc`, `otel-http` |
| StatsD / Datadog | T29 | planned | `statsd` |

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
