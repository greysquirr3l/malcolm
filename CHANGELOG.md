## [0.9.1] - 2026-10-29

A patch release carrying two fixes that make malcolm's promises
checkable rather than merely stated: the lens token budget now reaches
the provider as a hard cap, and the ignored RustSec advisories are now
backed by a test instead of a comment.

No breaking changes to the public API of `malcolm-core`, `malcolm`, or
`malcolm-lens`.

### Fixed

- **`MALCOLM_LENS_MAX_TOKENS` never reached the provider.** The value
  was read into `LensConfig::max_tokens` and documented as the
  "requested max response token budget", but it only ever reached the
  model as a `TOKEN_BUDGET_HINT` line in the prompt text. Against
  Anthropic that meant a run configured with a 1,024-token budget could
  emit up to ~64,000 output tokens — rig-core 0.41's Anthropic provider
  requires `max_tokens` and fills it from its own per-model default
  (`default_max_tokens_for_model`, 64,000 for `claude-sonnet-4*`) when
  the request leaves it unset. The request never errored; it silently
  ignored the budget.

  `completion::build_request` now sets the budget on the request
  builder, so the configured value is the one the provider enforces.
  Anthropic receives it as `max_tokens`; Ollama receives it as
  `options.num_predict`, which rig-core 0.41 maps from the same field,
  so the budget is enforced on both providers rather than being
  prompt-only. The `TOKEN_BUDGET_HINT` line stays — a hint to the model
  is not the same as a hard output cap, and the two serve different
  purposes.

- **The advisory ignore lists were not backed by anything.** The five
  ignored RustSec advisories (`zip`, `bincode`, `paste`) were justified
  by the claim that the crates they name are reachable only through the
  optional `bayesopt` feature. That claim was checked by nobody: a
  future change that made one of those code paths reachable would have
  left the ignore suppressing the signal instead of raising it.

  `crates/malcolm/tests/advisory_reachability.rs` now recomputes the
  default-feature dependency closure for every workspace member and
  fails if `zip`, `bincode`, or `paste` appears in it. The closure is
  an over-approximation, which is the safe direction — it can raise a
  false alarm on a crate that is genuinely unreachable, but it cannot
  let a reachable one slip through. The same test pins `deny.toml` and
  `.cargo/audit.toml` to the same advisory set so the `cargo deny` and
  `cargo audit` gates cannot drift apart.

### Changed

- **The lens providers sit behind a narrow `CompletionTransport` port.**
  `AnthropicLens` and `OllamaLens` previously reached for their rig-core
  client directly, which made the HTTP boundary impossible to observe
  from a test. The port keeps the public API of both types unchanged
  while letting the tests substitute a recording HTTP backend and assert
  the exact request body that goes out. Each transport is generic over
  its HTTP backend, so the wire format is exercised against rig-core's
  own serialisation rather than a hand-rolled approximation.
- **The CI test job now runs `cargo test -p malcolm-lens
  --all-features`.** `malcolm-lens` defaults to the `ollama` feature, so
  `cargo test --workspace` never compiled the Anthropic provider's
  tests — the new wire-level regression test would not have run in CI at
  all. The workspace-wide run is unchanged: `--all-features` across the
  workspace would enable `malcolm-agent`'s Linux-gated `netem` and
  `syscall` adapters, which need a real Linux runner and host
  privileges.
- **`deny.toml` and `.cargo/audit.toml` now name the test that enforces
  their ignores**, so the justification is checkable rather than
  aspirational.
- **The quick-start dependency examples were updated** from
  `malcolm = "0.9.0"` to `malcolm = "0.9.1"` in `README.md` and
  `docs/book/src/quick-start.md`. The published `0.9.0` has the token
  budget bug, so a reader who copies the snippet should get the fix.
- **The SARIF example in `docs/ci-gating.md` now shows `malcolm`
  version `0.9.1`.** The emitted value comes from `CARGO_PKG_VERSION`,
  so the example was showing a version the tool no longer produces.
- **`docs/book/src/index.md` gains a "New in 0.9.1" section**, and the
  `MALCOLM_LENS_MAX_TOKENS` description in `README.md` and
  `docs/book/src/lens-guide.md` now says the budget is sent as the
  provider's hard output cap rather than leaving it ambiguous.

## [0.9.0] - 2026-10-27

This release rolls up the lint-gate work that landed after 0.8.0. The
substantive change for consumers is the `toml` 0.9 → 1.1 upgrade in
`malcolm`; the rest is lint enforcement, CI hardening, and dev tooling.

No breaking changes to the public API of `malcolm-core`, `malcolm`, or
`malcolm-lens`.

### Changed

- **`malcolm` upgrades `toml` from 0.9 to 1.1.** The 1.x line is the
  current release of the crate; 0.9 no longer receives fixes. The
  `toml` types do not appear in `malcolm`'s public API — a parse
  failure is reported as `BudgetError::Parse` with the underlying
  error boxed as `Box<dyn std::error::Error + Send + Sync>` — so
  this is not a breaking change for callers. `toml` 1.1 keeps the
  workspace's `rust-version = "1.85"` floor, so the MSRV is unchanged.
- **The CI lint gate now runs the repository's strict clippy profile.**
  `.github/workflows/ci.yml` previously ran
  `cargo clippy --workspace --all-targets -- -D warnings`, which was
  weaker than the `cargo l` alias in `.cargo/config.toml`: it lacked
  `--all-features`, the pedantic/nursery/cargo/perf lint groups, and the
  `unwrap_used` / `expect_used` / `panic` / `indexing_slicing` /
  `cast_ptr_alignment` / `suspicious` denials. CI and the pre-push hook
  now both run the same alias, so the pipeline gate and the local gate
  are one definition.
- **The strict clippy profile moved from the `l` alias into
  `[target.'cfg(all())'] rustflags`** in `.cargo/config.toml`, so the
  native and cross-target lint invocations share one flag set and
  cannot drift apart. The flags are clippy tool lints, which rustc
  ignores, so ordinary `cargo build` and `cargo test` runs are
  unaffected.
- **GitHub Actions permissions tightened.** Two workflows were granted
  only `contents: read` while needing more; the auto-tag workflow now
  requests `actions: write` explicitly to dispatch the release
  workflow, with the reason documented inline.
- **`softprops/action-gh-release` 3.0.1 → 3.0.3** and
  **`taiki-e/install-action` 2.4.0 → 2.87.5.**
- **`SECURITY.md` supported-versions table updated** from `0.5.x` to
  `0.9.x`. The table had not been touched since the initial commit
  and was three releases stale, so it did not describe the versions
  that actually receive security fixes.
- **Quick-start dependency examples updated** from `malcolm = "0.6.0"`
  to `malcolm = "0.9.0"` in `README.md` and `docs/book/src/quick-start.md`.
  These had likewise gone stale at 0.6.0.
- **The SARIF example in `docs/ci-gating.md` now shows
  `malcolm` version `0.9.0`.** The emitted value comes from
  `CARGO_PKG_VERSION`, so the example was showing a version the tool
  no longer produces.

### Fixed

- **23 strict-profile lints in the Linux-gated `malcolm-agent` code.**
  The `cgroups`, `netem`, and `syscall` adapters are behind
  `#[cfg(all(target_os = "linux", ...))]`, so the previous CI lint
  invocation never saw them: on a Linux runner the pedantic and
  nursery groups were never enabled, and on a macOS or Windows
  workstation the code does not compile at all. The lints were real,
  and they are fixed rather than suppressed:
  - `clippy::indexing_slicing` — `cgroups.rs` indexed `parts[0]` and
    `parts[2]` behind a `parts.len() == 3` guard that the lint cannot
    see through; it now matches a `[hierarchy, _, path]` slice
    pattern, which enforces the length structurally. The same fix in
    the cgroup integration test replaces `entries[0]` with
    `entries.first().expect(..)`.
  - `clippy::redundant_pub_crate` — `netem_cmd` is a private module,
    so its `pub(crate)` items add nothing over `pub`. The
    `redundant_pub_crate` / `unreachable_pub` pair has no workable
    visibility between them, so the items became `pub` with a
    per-item `#[expect(unreachable_pub, reason = "..")]`, matching the
    treatment already used for `malcolm-lens`'s `completion` module
    and `malcolm`'s `test_util` module.
  - `clippy::option_if_let_else` — the `if let` / `match` pairs in
    `NetemParams::tc_argv`, `Supervisor::stop`, and
    `probe_ptrace_available` are now `map_or_else` / `is_ok_and`.
  - `clippy::expect_used` and `clippy::unwrap_used` — both occur only
    in test code, and are suppressed on the test modules with a
    reason, per the convention in `malcolm`'s `assertions` module. The
    one `panic!` in the netem integration test became an `assert!`,
    which also clears `clippy::manual_assert`.
  - `clippy::too_long_first_doc_paragraph` — five long leading doc
    paragraphs were split into a summary line plus a detail
    paragraph.
  - `clippy::missing_const_for_fn` — `NetemAction::to_params` and
    `ptrace::adapter_failure` are now `const fn`.
  - `clippy::use_self` — `NetemAdapter::revert_internal` is now
    `Self::revert_internal`.
  - `clippy::derive_partial_eq_without_eq` — `SyscallEffect` now
    derives `Eq` alongside `PartialEq`.
  - `clippy::redundant_clone` — the cgroup adapter cloned a `PathBuf`
    that is not used afterwards.
  - `unreachable_pub` — `probe_netem_writable` in the netem
    integration test was `pub` inside a test binary, where nothing is
    externally reachable.
- **Three strict-profile lints in the new `malcolm-lens` completion
  module** — two `clippy::redundant_pub_crate` and one
  `clippy::expect_used`, resolved the same way as above. The
  `completion` module is private, so its helpers are crate-internal by
  design; `pub` with a per-item `#[expect(unreachable_pub, ...)]`
  states that intent without suppressing the lint crate-wide.

### Added

- **`cargo lx`, a cross-target lint alias for the Linux-gated code.**
  Clippy type-checks without linking, so cross-targeting at
  `x86_64-unknown-linux-gnu` needs no Linux C toolchain — just
  `rustup target add x86_64-unknown-linux-gnu`. This closes the gap
  that let the 23 lints above reach `main`: a native `cargo l` never
  type-checks Linux-gated code, so a contributor on macOS or Windows
  now has a way to see them locally. The alias is scoped to
  `-p malcolm-agent` because it is the only crate with platform-gated
  code; the others cannot cross-lint as a workspace because their
  dev-dependencies pull in C code (`criterion` → `alloca`,
  `rig-core` → `aws-lc-sys`) that needs a real Linux compiler. The
  pre-push hook runs it and installs the target on demand.

## [0.8.0] - 2026-10-26

This release is a maintenance and dependency-security release. It clears
the RustSec advisories that were failing the Security Audit gate on
`main` and on every open Dependabot PR, and it carries the `rig-core`
0.41 upgrade for the interpretability layer.

One breaking change: the YAML error type in `malcolm::replay` moves from
`serde_yaml::Error` to `serde_norway::Error`. The YAML wire format is
unchanged.

### Changed

- **Breaking — `malcolm::replay` YAML error type.** `ScenarioRecord::to_yaml`
  and `ScenarioRecord::from_yaml` now return `serde_norway::Error` instead of
  `serde_yaml::Error`. `serde_yaml` is published as `0.9.34+deprecated`
  ("This project is no longer maintained") and pulls in the unmaintained
  `unsafe-libyaml`; `serde_norway` is the maintained fork and depends on
  `unsafe-libyaml-norway`. Callers that only propagate the error with `?` are
  unaffected; callers that name the type explicitly need to switch crates.
- **`malcolm-lens` migrates to `rig-core` 0.41.** The completion API is now
  generic over a `CompletionModel` trait, and `CompletionModel::completion`
  returns `CompletionResponse<ModelOutput>`. `malcolm-lens` defines the
  `CompletionModel` trait locally (per the consumer-owned-traits rule) with a
  `MalcolmModel` adapter, so downstream users can plug in their own model.
- **Advisory-driven dependency upgrades** — `ctutils` 0.4.6, `h2` 0.4.20,
  `quinn-proto` 0.11.19, `rustls` 0.23.46, `rustls-webpki` 0.103.16,
  `webpki-root-certs` and `webpki-roots` 1.0.11, `tokio` 1.54.1, `chacha20`
  0.10.2 (a yanked version).
- **Documented advisory ignores.** `.cargo/audit.toml` (new) and `deny.toml`
  now carry the same ignore list, so `cargo audit` and `cargo deny` agree:
  - `RUSTSEC-2026-0367/0368/0369` (`zip`) — all three concern the archive
    *extraction* path (`ZipArchive::extract` / `ZipStreamReader::extract`).
    `zip` reaches malcolm only through the optional `bayesopt` feature, and
    `ndarray-npy` reads `.npz` payloads in memory via `by_index` / `by_name`
    rather than `extract()`, so the vulnerable code is unreachable. The fix
    ships in `zip >= 9.0.0`; `ndarray-npy` pins `zip ^2` with no compatible
    release in between.
  - `RUSTSEC-2025-0141` (`bincode`) and `RUSTSEC-2024-0436` (`paste`) —
    unmaintained with no patched release, reachable only via `bayesopt`.
- **Clippy fixes in feature-gated code** — missing `# Errors` sections on
  `OtelRecorder::force_flush`, `OtelRecorder::shutdown`, `with_otlp_exporter`,
  `StatsdConfig::from_env`, `StatsdRecorder::with_config` and the three
  `KubeClient` trait methods; missing `#[must_use]` on `otel_tracing_layer`.

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
