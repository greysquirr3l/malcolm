## [0.7.0] - 2026-08-01

This release rolls up two complete capability rollouts: **Phase 9 (T26–T32)
observability** and **Phase 12 (T39–T42) Bayesian chaos**, plus criterion
benchmarks for the new math primitives. No breaking changes to the
public `malcolm` API; the new math types are additive modules under
`malcolm_core`. Per the release commit history this rolls PR #40
("bench: criterion benches for Phase 12 Bayesian chaos and Hawkes")
plus its predecessor PR #31 ("feat(metrics): add MetricsRecorder seam
and scenario metric emission") into a single 0.7.0 line.

### Added

#### Phase 9 — Observability (T26–T32)

- **`malcolm::metrics` seam (T26)** — in-process observability port:
  `MetricsRecorder` trait (`Send + Sync`, single `record(&MetricSample)`
  method), `NoopRecorder` default, `MetricsHub` fan-out
  (clonable, builder-style `with_recorder` / `with_recorders`, hubs
  nest), and the canonical metric taxonomy as `pub const` strings
  (`malcolm_faults_injected_total`, `malcolm_faults_skipped_total`,
  `malcolm_fault_intensity`, `malcolm_fault_latency_ms`,
  `malcolm_scenario_duration_ms`). `MetricKind { Counter, Gauge,
  Histogram }` and `MetricUnit { Count, Milliseconds, Ratio, Bytes }`
  round out the typed sample shape.
- **Scenario wiring** — `ChaosScenario::run_with_metrics(&mut ctx,
  &MetricsHub)` walks every fault and emits the corresponding samples,
  plus a duration histogram for the whole scenario. The pre-existing
  `ChaosScenario::run(&mut ctx)` is now a thin wrapper that calls
  `run_with_metrics` with an empty hub, so behavior is byte-identical
  to a build without metrics when no recorder is installed.
- **Prometheus exporter (T27)** — feature-gated via
  `--features prometheus`. Counters and gauges expose the canonical
  names with `fault_type`, `node_id`, `scenario`, `regime`, `dry_run`,
  `skip_reason` labels; histograms expose `_bucket`, `_sum`, `_count`
  in standard Prometheus format. Unknown metric names log a
  `tracing::warn!` once per recorder lifetime and skip the sample.
- **OpenTelemetry / OTLP exporter (T28)** — feature-gated via
  `--features otel` (plus `otel-grpc` or `otel-http` for the wire
  protocol). Translates the canonical taxonomy to OTel metric streams;
  OTLP gRPC and HTTP exporters wired through `opentelemetry-otlp`.
- **StatsD / Datadog exporter (T29)** — feature-gated via
  `--features statsd`. DogStatsD extension tags for the histogram
  percentile distribution. Multiplexed UDP/Unix socket support for
  the local agent; `dogstatsd_non_local_traffic: true` documented in
  `docs/metrics.md` for the cross-host case.
- **Resilience budget assertions (T30)** — `malcolm::assertions`
  module: `assert_loss_below`, `assert_p99_below`, `assert_recovery_rate`
  per-scenario, plus `assertions::run` for bundled budgets. CI gates
  fail the scenario if any budget is missed.
- **JUnit + SARIF reports (T31)** — `malcolm::report_formats` writes
  `report.junit.xml` and `report.sarif.json` alongside the existing
  JSON and YAML outputs. SARIF is severity-graded and integrates with
  the GitHub Code Scanning dashboard.
- **CI/CD templates (T32)** — `.github/workflows/resilience.yml`
  (`malcolm-resilience` action), `.github/instructions/mermaid.instructions.md`,
  `scripts/resilience-gate.sh`, `ci/budget.toml`, and `ci/malcolm-resilience.gitlab-ci.yml`
  for self-hosted runners. All templates consume the same canonical
  metric names so dashboards stay in sync across CI providers.

#### Phase 12 — Bayesian chaos (T39–T42)

- **`malcolm_core::inference` (T39)** — Bayesian cascade network
  inference over the failure graph. Noisy-OR marginals with
  blast-radius distributions; analytically tractable companion to the
  T12 sampler, deterministic on identical inputs.
- **`malcolm_core::posterior` + `malcolm::rootcause` (T40)** —
  Bayesian root-cause posterior. `OriginPrior`, `Observation`,
  `infer_posterior` operate in log-space with log-sum-exp for
  numerical stability and partial-observation marginalisation.
  `RootCauseConfig` + `RootCauseReport` in `malcolm::rootcause` with
  serde round-trip and a `BayesianCascade` adapter in
  `malcolm::topology`.
- **`malcolm::search` (T41, feature-gated `bayesopt`)** —
  Bayesian-optimized adaptive fault search. `Objective` trait as the
  domain seam, `SearchSpace` for continuous and integer dimensions,
  `FaultConfig`, `SearchConfig`, `SearchResult`, `bayes_search()`.
  Backend: `egobox-ego 0.38` (EGO loop + Kriging + EI infill),
  `ndarray 0.16`, pure-Rust `linfa-linalg`. Per-call `TraceBuf`
  (`Arc<TraceBuf>`) so concurrent searches don't stomp each other.
- **`malcolm_core::hawkes` (T42)** — Hawkes conditional-intensity
  process for clustered fault arrivals. `HawkesProcess { mu, alpha,
  beta }` with parameter validation, `branching_ratio`, `is_stationary`,
  `long_run_rate`, `intensity_at`, `intensity_incremental`, `apply_event`,
  and `simulate` (Ogata thinning, deterministic via seed).

#### Benchmarks and examples

- **Criterion benchmarks** for every new math primitive:
  `crates/malcolm-core/benches/hawkes.rs` (7 benchmarks) and
  `crates/malcolm/benches/bayesian_chaos.rs` (8 benchmarks). Baseline
  JSONs under `benches/baselines/<fn>/myref/` enable regression
  detection via `scripts/check_bench_regressions.sh`.
- **Four worked examples** under `crates/malcolm/examples/`:
  `cascade_inference` (T39), `root_cause_analysis` (T40),
  `hawkes_arrivals` (T42), `bayesopt_search` (T41, behind
  `--features bayesopt`).
- **Visualised benchmark outputs**: 12 SVG charts under
  `assets/img/bench/` plus a `docs/book/src/benchmarks.md` mdbook page
  explaining how to read the typical-time / regression-plot pair per
  function.

### Changed

- **Workspace crate versions all bumped to 0.7.0** — `malcolm-core`,
  `malcolm`, `malcolm-lens`, `malcolm-agent`. Cross-crate dependencies
  in the workspace `Cargo.toml` files reference the new 0.7.0 version
  where appropriate (malcolm depends on malcolm-core, malcolm-agent
  depends on both, malcolm-lens depends on malcolm).
- **CI workflow hardening** — `.github/workflows/release.yml`:
  split the combined `uses:` + `run:` step in the `github-release` job
  into a checkout step + an `id: notes` shell step (the malformed
  YAML caused GitHub to bypass the `push: tags: v*` filter and fire
  release.yml on every branch push). Markdownlint config moved from
  `.markdownlintignore` to a structured `.markdownlint-cli2.jsonc`
  that excludes `.vscode/`, `tasks/`, `.coraline/`, `target/`, and
  other non-user-facing working directories; the existing 11 long-
  standing MD022/MD032/MD034 issues in
  `.github/instructions/mermaid.instructions.md` are fixed at the
  same time.
- **`infer_posterior` and `RootCauseConfig::new`** now carry
  `#[must_use]` so CI's `cargo clippy --workspace --all-targets
  -- -D warnings` (which promotes pedantic `clippy::must_use_candidate`
  to error via `-D warnings`) passes.

### Fixed

- **Doctest regression in `malcolm_core::hawkes`** — the module-level
  docstring example was using `unwrap_or_panic`, a private test
  helper not in scope at doctest time. Replaced with `.unwrap()`.
- **Markdownlint regressions in `PROGRESS.md` and `README.md`** —
  stray `|` inside a table cell that broke MD056 column count, and a
  multi-blank-line run before `## License` that tripped MD012.
- **Malformed `release.yml` workflow** — see "Changed" above; the
  fix is what kept release.yml off the branch-push trigger.

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
