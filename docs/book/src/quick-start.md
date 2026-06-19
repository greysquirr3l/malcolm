# Quick Start

## Prerequisites

- Rust stable toolchain installed.
- A clean clone of the repository.

## Build and test everything

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

## Add as a dependency

```toml
[dev-dependencies]
malcolm = "0.6.0"
```

## Run a named preset from the CLI

The `malcolm-run` binary executes one of the built-in presets and emits a
JSON report (plus an optional `ScenarioRecord` for replay):

```bash
# Show every available preset.
cargo run -p malcolm --bin malcolm-run -- --list-presets

# Run a preset with overrides, write the JSON report to stdout.
cargo run -p malcolm --bin malcolm-run -- --preset flaky_net --seed 7

# Record a run for later replay (.yaml or .json from extension).
cargo run -p malcolm --bin malcolm-run -- --preset slow_disk --record run.yaml
```

Available presets: `flaky_net`, `slow_disk`, `byzantine_cluster`,
`clock_drift`, `memory_pressure`. See `malcolm::presets` for the Rust
API.

## Build your first scenario

```rust
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = ChaosScenario::builder()
    .name("quick-start")
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
assert!(!report.events.is_empty());
```

## Run included examples

```bash
cargo run -p malcolm --example simulation
cargo run -p malcolm --example replay_demo
cargo run -p malcolm --example async_service --features tokio
```

## Next step

Continue to Architecture to understand which crate should own each type of change.
