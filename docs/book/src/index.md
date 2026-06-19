# malcolm Handbook

malcolm is a Rust chaos engineering workspace focused on deterministic fault injection and adversarial simulation. It is designed for distributed systems teams that need reproducible failure conditions, realistic jitter patterns, and auditable experiment outputs.

This book gives you one path from first run to production release operations.

## What you will learn

- How the three crates are layered and why that separation matters.
- How to build scenarios with deterministic seeds and realistic failure distributions.
- How to record, replay, and seal scenario outputs.
- How to use the optional lens layer for post-mortem analysis.
- How tracing and CI pipelines support safe operation and release.
- How to run a named preset from the operator CLI without writing Rust.

## Crates in this workspace

| Crate | Role |
|---|---|
| `malcolm-core` | Pure domain and math primitives, no_std compatible |
| `malcolm` | Assembly layer: faults, scenarios, replay, topology, presets, `malcolm-run` CLI |
| `malcolm-lens` | Optional advisory analysis layer |

## New in 0.6.0

- `malcolm::presets` — five named scenarios (`flaky_net`, `slow_disk`,
  `byzantine_cluster`, `clock_drift`, `memory_pressure`) ready to
  run from code or from the new `malcolm-run` CLI.
- `Topology::to_dot()` and `Topology::to_mermaid()` for cascade
  graph visualisation.
- `ScenarioRecord::to_yaml()` / `from_yaml()` round-trip alongside
  the existing JSON and bytes serialisers.
- `EnvelopeError::EntropyUnavailable` for transient OS-entropy
  failures during seal.
- Workspace `rust-version = "1.85"`, `[lints]` table enforced, 28
  new proptest cases, 13 new criterion benchmarks, and a weekly
  `cargo-fuzz` run. CI now includes `cargo deny check`, `cargo
  audit`, and a CodeQL analysis.

## Read this book in order

If you are new, read:

1. Quick Start
2. Architecture
3. Scenario Design
4. Deterministic Replay

Then move to Sealed Envelopes and Lens Guide for advanced operation.
