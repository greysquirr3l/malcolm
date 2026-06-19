# Changelog

All notable changes to this project are documented in this file.

The format is based on Keep a Changelog and this project follows Semantic Versioning.

## [0.6.0] - 2026-06-19

This release rolls up the hardening, scenario-runner, observability, and
supply-chain work that landed on `main` between `0.5.1` and the
current `head`. No breaking changes to the public `malcolm` API.

### Added

- **Scenario presets** (`malcolm::presets`): five named, ready-to-run
  fault bundles — `flaky_net`, `slow_disk`, `byzantine_cluster`,
  `clock_drift`, `memory_pressure` — plus a `preset(name)` lookup and a
  `PRESET_NAMES` constant.
- **Scenario runner CLI** (`malcolm-run`): new binary that takes a
  named preset, applies user-supplied overrides (`--seed`, `--node`,
  `--profile`, `--dry-run`), and emits a JSON report (stdout or
  `--output`) plus an optional `ScenarioRecord` (YAML or JSON from
  extension, via `--record`).
- **Topology visualisation**: `Topology::to_dot()` for Graphviz DOT
  and `Topology::to_mermaid()` for Mermaid flowcharts.
- **YAML round-trip for `ScenarioRecord`**: `to_yaml()` / `from_yaml()`
  alongside the existing JSON and bytes serialisers.
- **New error variant**: `EnvelopeError::EntropyUnavailable`,
  surfaced when the OS entropy source refuses a fill (e.g.
  `/dev/urandom` unavailable).
- **Workspace `rust-version = "1.85"`** declared on every crate.
- **Workspace `[lints]` table** with `unsafe_code = "forbid"`,
  `missing_docs = "warn"`, and `unreachable_pub = "warn"`. All missing
  rustdoc filled in to satisfy the lint.
- **`CONTRIBUTING.md`**, **`SECURITY.md`**, and **`CODE_OF_CONDUCT.md`**
  at repo root.
- **Property-based tests** (28 cases): distributions, noise, Lyapunov,
  bifurcation, replay integrity, envelope tamper detection.
- **Criterion benchmarks** (13): hot math primitives and the
  envelope seal/open path under `crates/malcolm/benches/`.
- **`cargo-fuzz` targets** (`classify`, `envelope_from_bytes`,
  `response_parser`) under a new `fuzz/` crate, run weekly by CI.
- **End-to-end integration test** stitching the public API together:
  `chaos -> replay -> envelope -> open`, with wrong-passphrase
  rejection and all-format round-trip.

### Changed

- **CI hardening**: `cargo doc --workspace --no-deps` with
  `RUSTDOCFLAGS="-D warnings"`, `cargo build --workspace --examples`,
  `cargo deny check`, `cargo audit`, and a markdownlint step now run
  on every push. All `actions/checkout` references pinned to
  commit SHAs.
- **Dependabot** enabled (`.github/dependabot.yml`) with weekly
  updates for both `github-actions` and `cargo` ecosystems.
- **Branch protection** on `main`: 1 approving review, admin
  override, linear history, signed commits, conversation
  resolution, status checks on the three required CI jobs.
- **Bumped `rand 0.8.6 → 0.9.4`**. Internal call sites updated to
  `Rng::random` / `Rng::random_range` and to `TryRngCore::try_fill_bytes`
  (with the new `EntropyUnavailable` error variant). The public
  `ScenarioEnvelope` API is unchanged.
- **Bumped `criterion 0.5.1 → 0.8.2`** and **`rig-core 0.37.0 → 0.39.0`**.
- Updated quick-start dependency examples to `malcolm = "0.6.0"`.

## [0.5.1] - 2026-05-30

### Fixed

- Hardened release publishing workflow for crates.io by adding retry logic,
  already-published detection, and index propagation waits between crate
  publishes.
- Reduced false-negative release failures caused by crates.io index lag after
  successful publishes.

### Changed

- Bumped workspace crates to `0.5.1`:
  - `malcolm-core`
  - `malcolm`
  - `malcolm-lens`
- Updated quick-start dependency examples to `malcolm = "0.5.1"`.

## [0.5.0] - 2026-05-30

This release establishes `0.5.0` as the first feature-complete milestone for the malcolm workspace. It is based on the repository commit history through `chore: finalize implementation and cleanup`, plus the release automation and handbook integration shipped in this change set.

### Added

- Cargo workspace scaffold with three crates: `malcolm-core`, `malcolm`, and `malcolm-lens`.
- Math and distribution primitives for chaos simulation including power-law, Pareto, and log-normal families.
- Lyapunov sensitivity scoring for instability amplification analysis.
- Bifurcation profile and threshold modeling for regime shifts.
- Fault-core infrastructure including deterministic handle/registry wiring and dry-run support.
- Network fault primitives for packet loss, latency disruption, and partition-style behaviors.
- Resource fault primitives for memory, CPU, and I/O degradation modeling.
- Clock fault primitives for skew, freeze, and jump-style temporal disturbances.
- Comprehensive mdBook handbook source under `docs/book` with architecture, replay, envelope, lens, observability, and release-operation guides.
- Wiki publication automation that syncs handbook pages from `docs/book` to the repository wiki.

### Changed

- Consolidated the implementation into a stable, workspace-first architecture suitable for CI/release automation.
- Updated all published crate versions to `0.5.0`:
  - `malcolm-core`
  - `malcolm`
  - `malcolm-lens`
- Extended CI with mdBook build validation to prevent doc regressions.
- Added release-chain workflows for OSSF Scorecard, Auto Tag, Release, and crates.io publishing on release.

### Commit Guide (selected)

- `ff46a63` feat(workspace-scaffold): initialize cargo workspace with malcolm crates and CI
- `efb28df` feat(distributions): implement power-law/pareto/log-normal distributions
- `6da2fca` feat(lyapunov-scorer): add sensitivity scoring primitives
- `dbe9758` feat(bifurcation-profiles): add threshold profiles and regime support
- `edf4060` feat(fault-core): add core fault traits, handle, registry, and dry-run scaffolding
- `2e9caa9` feat(network-faults): add network fault modeling
- `38dc8c1` feat(resource-faults): add resource pressure/degradation faults
- `a7ba88d` feat(clock-faults): add clock skew/freeze/jump faults
- `7c517ca` chore: finalize implementation and cleanup
