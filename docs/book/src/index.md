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

## New in 0.9.1

- `MALCOLM_LENS_MAX_TOKENS` is now a real output cap. It used to reach
  the provider only as a `TOKEN_BUDGET_HINT` line in the prompt, so an
  Anthropic run budgeted at 1,024 tokens could still emit up to ~64,000
  — the provider's own per-model default. The budget is now sent as the
  provider's `max_tokens` (`options.num_predict` for Ollama), and a
  wire-level regression test pins it there.
- The five ignored RustSec advisories are now backed by a test rather
  than a comment. `crates/malcolm/tests/advisory_reachability.rs`
  recomputes the default-feature dependency closure and fails if `zip`,
  `bincode`, or `paste` ever becomes reachable from a default build.

## New in 0.9.0

- The CI lint gate now runs the repository's strict clippy profile, and
  the pre-push hook runs the same command — so the pipeline gate and the
  local gate are one definition rather than three drifting ones.
- `cargo lx` cross-lints the Linux-gated adapter code (`cgroups`,
  `netem`, `syscall`) from a macOS or Windows workstation, which a
  native `cargo l` cannot reach.
- `malcolm` upgrades `toml` from 0.9 to 1.1. The `toml` types do not
  appear in the public API, so this is not a breaking change.

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
