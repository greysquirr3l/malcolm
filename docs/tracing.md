# Tracing Schema

`malcolm` uses a single tracing target for structured fault telemetry: `malcolm`.
The target is reserved for fault injection, dry-run planning, scenario summaries,
and topology cascade events that should be visible to the testing and replay
layers.

## Canonical fields

All events on the `malcolm` target should carry the core schema below whenever
the data is available.

| Field | Meaning |
|-------|---------|
| `fault_type` | Stable identifier for the emitted event or fault |
| `node_id` | Target node that the fault affected |
| `seed` | Deterministic seed used for replay |
| `intensity` | Normalised fault intensity in the range `[0.0, 1.0]` |
| `regime` | Classified regime derived from the active bifurcation profile |
| `dry_run` | `true` when the event describes planned work instead of injection |

Optional fields may be attached for richer diagnostics, such as
`timestamp_ms`, `scenario_name`, `profile`, `events`, `skip_reason`, or
`total_duration_ms`.

## Event families

### Fault injection

Each concrete fault emits a `malcolm` event when it injects and a separate
`malcolm` event when it performs a dry-run. These are the events that back the
replay and regression tests.

### Scenario execution

`ChaosScenario::run` emits a summary event when the run completes and a dry-run
summary event when the scenario is only planned. These events let tests confirm
the scenario path taken without parsing free-form log text.

### Topology cascade

Cascade propagation emits `malcolm` events for each hop so replay tests can
verify the propagation order and the node sequence that was visited.

## Collector

[`MalcolmLayer`](../crates/malcolm/src/tracing_layer.rs) is the testing hook for
this schema. It subscribes to the `malcolm` target and records structured fault
events as `FaultEvent` values so the tests can assert on the emitted shape
without inspecting log output directly.

## Example

```rust
use malcolm::MalcolmLayer;
use tracing_subscriber::prelude::*;

let layer = MalcolmLayer::new();
let subscriber = tracing_subscriber::registry().with(layer.clone());
tracing::subscriber::with_default(subscriber, || {
    tracing::info!(target: "malcolm", fault_type = "packet_loss", node_id = "node-0", seed = 1_u64, intensity = 0.8_f64, dry_run = false, timestamp_ms = 0_u64, "packet loss injected");
});
assert_eq!(layer.events().len(), 1);
```
