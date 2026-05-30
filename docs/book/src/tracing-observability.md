# Tracing and Observability

malcolm uses structured `tracing` events on the `malcolm` target to make injected behavior auditable and replay-friendly.

## Canonical fields

| Field | Meaning |
|---|---|
| `fault_type` | Stable fault or event identifier |
| `node_id` | Target node identifier |
| `seed` | Deterministic replay seed |
| `intensity` | Normalized intensity in `[0.0, 1.0]` |
| `regime` | Profile-derived operating regime |
| `dry_run` | `true` when no real injection occurred |

## Event families

- Fault injection events.
- Dry-run planning events.
- Scenario run summary events.
- Topology cascade propagation events.

## Test collector

`MalcolmLayer` captures target events as structured values for test assertions.

```rust
use malcolm::MalcolmLayer;
use tracing_subscriber::prelude::*;

let layer = MalcolmLayer::new();
let subscriber = tracing_subscriber::registry().with(layer.clone());
tracing::subscriber::with_default(subscriber, || {
    tracing::info!(
        target: "malcolm",
        fault_type = "packet_loss",
        node_id = "node-0",
        seed = 1_u64,
        intensity = 0.8_f64,
        dry_run = false,
        timestamp_ms = 0_u64,
        "packet loss injected"
    );
});
assert_eq!(layer.events().len(), 1);
```

## Practical guidance

- Keep field names stable to preserve analysis tooling.
- Prefer structured fields over free-form message parsing.
- Ensure every fault type emits both inject and dry-run events.
