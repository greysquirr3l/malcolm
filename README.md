# malcolm

<img src="assets/img/malcom.png" alt="malcolm artwork" />
<p align="right"><sub>Credit: <a href="https://www.facebook.com/groups/150391231670295/user/100004935523934">Keenan McAteer</a></sub></p>

> "Your scientists were so preoccupied with whether or not they could, they didn't stop to think if they should."
> — Dr. Ian Malcolm

malcolm is a standalone Rust chaos engineering library for fault injection and adversarial simulation. It provides mathematically-grounded primitives for testing distributed systems, async services, and simulation layers under real-world failure conditions — seeded deterministic replay, power-law and Pareto fault distributions, Lyapunov sensitivity scoring, Byzantine fault primitives, and correlated noise generators, all with zero unsafe code and a no_std-compatible core.

## Crates

| Crate | Description |
|-------|-------------|
| `malcolm-core` | Pure math domain layer — no I/O, no_std compatible |
| `malcolm` | Assembly layer — fault traits, fault types, scenario composition |
| `malcolm-lens` | Optional LLM interpretability layer for post-mortem analysis |

## Quick Start

```toml
[dev-dependencies]
malcolm = "0.6.0"
```

## Scenario Composition

`ChaosScenario` composes multiple fault primitives into one named run with a
shared seed and bifurcation profile.

```rust
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = ChaosScenario::builder()
 .name("flaky-net")
 .seed(1337)
 .add_fault(PacketLoss::builder().seed(42).intensity(0.8).build())
 .profile(BifurcationProfile::network_partition())
 .build();

let mut ctx = FaultContext {
 seed: 1337,
 timestamp_ms: 0,
 node_id: "edge-0".to_owned(),
 profile: BifurcationProfile::network_partition(),
};

let report = scenario.run(&mut ctx);
let json = report.to_json();
assert!(json.as_deref().is_ok_and(|payload| payload.contains("flaky-net")));
```

## Macro DSL

When you want an inline scenario in a test, `malcolm!` expands to the same
builder chain with less noise.

```rust
use malcolm::faults::network::PacketLoss;
use malcolm::malcolm;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = malcolm! {
 name: "macro-demo",
 seed: 7,
 profile: BifurcationProfile::network_partition(),
 faults: [
  PacketLoss::builder().seed(11).intensity(0.8).build(),
 ],
};
```

## Topology Cascades

Use `Topology` + `CascadeFault` when one injected fault should probabilistically
propagate across graph edges.

```rust
use malcolm::fault::{Fault, FaultContext};
use malcolm::faults::network::PacketLoss;
use malcolm::topology::{CascadeFault, Topology};
use malcolm_core::bifurcation::BifurcationProfile;

let topology = Topology::builder()
 .name("cluster")
 .add_edge("a", "b", 1.0)
 .add_edge("b", "c", 0.5)
 .build();

let cascade = CascadeFault::new(
 Box::new(PacketLoss::builder().seed(7).intensity(0.9).build()),
 topology,
 42,
);

let ctx = FaultContext {
 seed: 42,
 timestamp_ms: 0,
 node_id: "a".to_owned(),
 profile: BifurcationProfile::network_partition(),
};

let _ = cascade.inject(&ctx);
```

## Replay Recording

Use `RecordingHarness` to capture a deterministic `ScenarioRecord`, then verify
with `ReplayHarness`.

```rust
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::replay::{RecordingHarness, ReplayHarness};
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = ChaosScenario::builder()
 .name("flight-recorder")
 .seed(5)
 .add_fault(PacketLoss::builder().seed(3).intensity(0.8).build())
 .profile(BifurcationProfile::network_partition())
 .build();

let mut ctx = FaultContext {
 seed: 5,
 timestamp_ms: 0,
 node_id: "node-0".to_owned(),
 profile: BifurcationProfile::network_partition(),
};

let record = RecordingHarness::new(&scenario).record(&mut ctx);
let replay = ReplayHarness::new(record);
assert!(replay.verify());
```

## Sealed Chaos Envelope

Use `ScenarioEnvelope` when telemetry artifacts should be encrypted at rest and
deliberately opened.

```rust
use malcolm::replay::RecordingHarness;
use malcolm::replay::envelope::{EnvPassphraseProvider, ScenarioEnvelope};

let provider = EnvPassphraseProvider::new("MALCOLM_ENVELOPE_PASSPHRASE");
let envelope = ScenarioEnvelope::seal(&record, &provider)?;
let bytes = envelope.to_bytes()?;

let decoded = ScenarioEnvelope::from_bytes(&bytes)?;
let opened = decoded.open_interactive(true, &provider)?;
assert_eq!(opened.seed, record.seed);
# Ok::<(), Box<dyn std::error::Error>>(())
```

- Payload is encrypted/authenticated with ChaCha20-Poly1305.
- Interactive open requires explicit confirmation.
- Non-interactive open is denied unless a passphrase provider is configured.
- Automation supports env-var, command, and keystore-backed passphrase sources.

## Observability

`malcolm::metrics` provides an in-process seam for emitting structured metric
samples alongside the existing `tracing` events. By default, every sample goes
to a no-op recorder so behavior and dependencies are unchanged.

```rust
use std::sync::{Arc, Mutex};
use malcolm::metrics::{MetricSample, MetricsHub, MetricsRecorder};

#[derive(Default)]
struct Collecting(Arc<Mutex<Vec<MetricSample>>>);
impl MetricsRecorder for Collecting {
    fn record(&self, s: &MetricSample) { self.0.lock().unwrap().push(s.clone()); }
}

let recorder = Arc::new(Collecting::default());
let hub = MetricsHub::new().with_recorder(recorder);
scenario.run_with_metrics(&mut ctx, &hub);
```

The canonical metric taxonomy (`malcolm_faults_injected_total`,
`malcolm_faults_skipped_total`, `malcolm_fault_intensity`,
`malcolm_fault_latency_ms`, `malcolm_scenario_duration_ms`) is shared between
malcolm and every exporter. See [`docs/metrics.md`](docs/metrics.md) for the
full table and a custom-recorder example.

Concrete exporters plug into this seam behind feature flags:

| Exporter | Feature | Status |
|----------|---------|--------|
| Prometheus | `prometheus` | shipped (T27) |
| OpenTelemetry / OTLP | `otel`, `otel-grpc`, `otel-http` | shipped (T28) |
| StatsD / Datadog | `statsd` | planned (T29) |

## Worked Examples

Run the examples from `crates/malcolm`:

```bash
cargo run -p malcolm --example simulation
cargo run -p malcolm --example replay_demo
cargo run -p malcolm --example async_service --features tokio
```

- `simulation` demonstrates Lyapunov scoring and bifurcation classification in a
    simple state-machine stress loop.
- `replay_demo` records a scenario run, reloads the record, and verifies replay
    integrity.
- `async_service` shows network fault injection around a mock HTTP client using
    a Tokio runtime.

## Scenario Runner CLI

`malcolm-run` is a built-in binary that runs a named preset from the command
line, prints the JSON report, and optionally records the run as a
`ScenarioRecord` for later replay.

```bash
# Show every available preset.
cargo run -p malcolm --bin malcolm-run -- --list-presets

# Run a preset with a custom seed and node id.
cargo run -p malcolm --bin malcolm-run -- --preset flaky_net --seed 7 --node edge-0

# Dry-run mode emits the would-inject plan without touching state.
cargo run -p malcolm --bin malcolm-run -- --preset slow_disk --dry-run

# Persist a run record for later replay (.yaml or .json from extension).
cargo run -p malcolm --bin malcolm-run -- --preset byzantine_cluster --record run.yaml
```

Available presets: `flaky_net`, `slow_disk`, `byzantine_cluster`, `clock_drift`,
`memory_pressure`. See `malcolm::presets` for the public Rust API.

## malcolm-lens Provider Scaffold

`malcolm-lens` now ships a feature-gated provider scaffold backed by
`rig-core`.

- Default feature: `ollama`
- Optional feature: `anthropic`
- Public API stays provider-agnostic through `LensProvider`

Environment variables:

- `MALCOLM_LENS_PROVIDER`: `ollama` (default) or `anthropic`
- `MALCOLM_LENS_MODEL`: optional model override
- `OLLAMA_BASE_URL`: optional Ollama endpoint override
- `MALCOLM_LENS_ALLOW_REMOTE_OLLAMA`: optional override (`true/1/yes/on`) to permit non-loopback Ollama hosts
- `MALCOLM_LENS_MAX_TOKENS`: optional token budget (default `1024`)
- `ANTHROPIC_API_KEY`: required when provider is `anthropic`

Security defaults:

- When using provider `ollama`, remote `OLLAMA_BASE_URL` values are blocked by
    default; loopback addresses are allowed.
- Metadata endpoints such as `169.254.169.254` are always rejected.
- Set `MALCOLM_LENS_ALLOW_REMOTE_OLLAMA=true` only when you intentionally run
    Ollama on a trusted remote host.

Prompt engine:

- `PromptBuilder` emits a fixed system prompt, a pretty-printed
    `ScenarioReport` JSON block, and a typed task suffix.
- `Directive` supports `Narrative`, `AnomalyFlag`, `SuggestScenarios`, and
    `ExplainDivergence`.
- `LensReport` is now a tagged enum with serializable payloads for narratives,
    anomaly flags, scenario suggestions, and replay divergence analysis.

Response parsing:

- `ResponseParser::parse(raw, directive)` first attempts direct JSON parsing.
- If that fails, it extracts JSON from fenced code blocks.
- If parsing still fails, it returns a narrative fallback with
    `parse_warning` set so callers can detect degraded output.
- Empty responses return `LensError::ParseFailure`.

Lens analyzer integration:

- `LensAnalyzer` is the end-to-end entrypoint that drives provider calls for
    one directive (`analyze`) or a standard sequence
    (`analyze_all`: Narrative + AnomalyFlag + SuggestScenarios).
- Default timeouts are provider-specific: `30s` for Ollama and `10s` for
    Anthropic.
- Each LLM call emits an `info` span with `provider`, `model`, `directive`,
    `duration_ms`, and `parse_ok` fields.
- Lens output is advisory-only and cannot modify fault injection or replay
    state.

Feature checks:

```bash
cargo build -p malcolm-lens --features ollama
cargo build -p malcolm-lens --no-default-features --features anthropic
```

Worked Lens examples:

```bash
cargo run -p malcolm-lens --example lens_postmortem
cargo run -p malcolm-lens --example lens_suggest
cargo run -p malcolm-lens --example lens_divergence
```

- Each example defaults to Ollama and exits cleanly with a clear message when
    Ollama is not reachable.
- To switch providers, set `MALCOLM_LENS_PROVIDER=anthropic` and
    `ANTHROPIC_API_KEY`.

Integration audit coverage:

- `crates/malcolm-lens/tests/integration_wiring.rs` exercises public API
    wiring from `LensAnalyzer` through provider/parse boundaries.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.
