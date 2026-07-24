# malcolm — Implementation Progress

> Orchestrator reads this file at the start of each loop iteration.
> Subagents update this file after completing a task.

## Status Legend

- `[ ]` — Not started
- `[~]` — In progress (claimed by a subagent)
- `[x]` — Completed
- `[!]` — Blocked / needs human input

---

## Phase 1 — Workspace Scaffold

| Task | Status | Notes |
|---|---|---|
| T01 — Cargo workspace, crate layout, CI pipeline, and repo hygiene | `[x]` | |

---

## Phase 2 — Math Primitives (malcolm-core)

> Depends on: Phase 1 all complete

| Task | Status | Notes |
|---|---|---|
| T02 — Power-law, Pareto, and log-normal fault distributions | `[x]` | |
| T03 — Pink (1/f) and brown noise generators for correlated jitter | `[x]` | |
| T04 — Lyapunov sensitivity scorer | `[x]` | |
| T05 — Bifurcation threshold profiles and tipping-point detection | `[x]` | |

---

## Phase 3 — Fault Primitives (malcolm)

> Depends on: Phase 2 all complete

| Task | Status | Notes |
|---|---|---|
| T06 — Core Fault trait, FaultHandle, and dry-run infrastructure | `[x]` | |
| T07 — Network fault layer: partition, packet loss, latency, bandwidth | `[x]` | |
| T08 — Resource fault layer: memory pressure, CPU throttle, I/O degradation | `[x]` | |
| T09 — Clock fault layer: skew, freeze, jump | `[x]` | `ClockSkew`, `ClockFreeze`, `ClockJump`, `MockClock`, `MalcolmClock` port trait; 6 tests |
| T10 — Byzantine fault primitives: lies, partial responses, slow-correct | `[x]` | `LyingNode`, `PartialResponse`, `SlowCorrect`, `ByzantineProfile`, `ByzantineSuite`; deterministic tests and tracing coverage |

---

## Phase 4 — Scenario System

> Depends on: Phase 3 all complete

| Task | Status | Notes |
|---|---|---|
| T11 — ChaosScenario: composable named fault bundles with seed and profile | `[x]` | `ChaosScenario`, `ScenarioReport` JSON serialization, deterministic scenario tests, scenario tracing span |
| T12 — Topology: named node graphs for cascade fault modeling | `[x]` | Directed adjacency-list `Topology`, `TopologyBuilder`, deterministic `CascadeFault` propagation with per-hop tracing |

---

## Phase 5 — Replay & Observability

> Depends on: Phase 4 all complete

| Task | Status | Notes |
|---|---|---|
| T13 — Deterministic replay engine and scenario recording | `[x]` | `ScenarioRecord`, `RecordingHarness`, `ReplayHarness`, deterministic integrity verification, JSON/bytes persistence |
| T14 — Structured tracing integration and log schema documentation | `[x]` | `MalcolmLayer`, `malcolm` target schema, tracing docs, fault and scenario event emission |

---

## Phase 6 — Ergonomics & Examples

> Depends on: Phase 5 all complete

| Task | Status | Notes |
|---|---|---|
| T15 — malcolm! declarative scenario macro | `[x]` | `malcolm!` macro, inline scenario DSL, README example, builder-backed tests |
| T16 — Worked examples: async service, simulation layer, and replay demo | `[x]` | Added runnable examples in `crates/malcolm/examples` and contract tests in `crates/malcolm/tests/examples_contract.rs` |

---

## Phase 7 — malcolm-lens (LLM Interpretability Layer)

> Depends on: Phase 6 all complete

| Task | Status | Notes |
|---|---|---|
| T17 — malcolm-lens crate scaffold with rig-core provider abstraction | `[x]` | Feature-gated Rig provider scaffold (`ollama` default, `anthropic` optional), async object-safe `LensProvider`, env-driven `LensConfig` |
| T18 — Structured prompt engine and LensReport type | `[x]` | `PromptBuilder`, `Directive`, and tagged `LensReport` payload types added; prompt tests and serde round-trips passing |
| T19 — LLM response parser with structured extraction and fallback | `[x]` | `ResponseParser` added with direct JSON parse, fenced-JSON extraction, and narrative fallback with `ParseWarning` |
| T20 — End-to-end LensAnalyzer: provider + prompt + parse wired together | `[x]` | `LensAnalyzer` added with builder, per-provider timeouts, tracing span fields, sequence API, and tests |
| T21 — malcolm-lens worked examples: post-mortem, suggestion, and divergence | `[x]` | Added three runnable lens examples, Ollama availability guard, and T21 contract tests |
| T22 — Security hardening and vulnerability review | `[x]` | Workspace audit completed; findings doc added and Ollama base URL SSRF guard enforced |
| T23 — Integration wiring audit | `[x]` | Integration findings documented; added end-to-end public API wiring tests |
| T24 — Stub and placeholder cleanup | `[x]` | Removed stale T09 placeholder note and added stub-cleanup findings doc |

---

## Phase 8 — Sealed Chaos Envelope

> Depends on: Phase 5 replay/tracing + Phase 7 security hardening

| Task | Status | Notes |
|---|---|---|
| T25 — Sealed chaos envelope telemetry | `[x]` | Encrypted envelope format added with deliberate-open policy and passphrase providers |

---

## Phase 9 — Observability & Telemetry Export

> Depends on: Phase 5 (tracing/replay) complete.
> Adds an in-crate metrics seam plus pluggable exporters. The seam is default;
> every heavyweight exporter is feature-gated so the default library stays lean.

| Task | Status | Notes |
|---|---|---|
| T26 — `MetricsRecorder` seam + scenario metric emission (**default**, no new deps) | `[x]` | `MetricsRecorder` trait, `MetricsHub` fan-out, `NoopRecorder`, canonical taxonomy, 9 unit tests; scenario wiring via `run_with_metrics`; no new deps |
| T27 — Prometheus exporter (**feature `prometheus`**) | `[x]` | `PrometheusRecorder` translates the canonical taxonomy into Prometheus time series; feature-gated behind `prometheus`; drop-`node_id` cardinality option; 5 unit tests |
| T28 — OpenTelemetry / OTLP metrics + traces exporter (**feature `otel`**) | `[x]` | `OtelConfig` + `from_env`, `OtelRecorder` (TestMetricReader-backed + periodic reader), `otel_tracing_layer` bridge, `with_otlp_exporter` factory for gRPC and HTTP sub-features; 19 unit tests |
| T29 — StatsD / Datadog (DogStatsD) exporter (**feature `statsd`**) | `[ ]` | Minimal-footprint UDP push; fire-and-forget, never fails the run |

---

## Phase 10 — CI/CD Resilience Gating

> Depends on: Phase 4 (scenarios) complete.
> Turns `malcolm-run` into a pass/fail gate and ships CI integration assets.
> All default — pure logic and repo assets, no heavy dependencies.

| Task | Status | Notes |
|---|---|---|
| T30 — Resilience-budget assertion mode in `malcolm-run` (**default**) | `[ ]` | New `ResilienceBudget`, dedicated exit code `3` on policy breach |
| T31 — JUnit XML + SARIF report emitters (**default**) | `[ ]` | First-class CI test panels + code-scanning annotations |
| T32 — GitHub Action + GitLab CI templates & docs (**default**, repo assets) | `[ ]` | Composite action, GitLab include, shared `scripts/resilience-gate.sh` |

---

## Phase 11 — Real-Environment Fault Adapters (`malcolm-agent`)

> Depends on: Phase 3 (fault primitives) + T33 scaffold.
> **New opt-in crate** bridging simulated faults to real OS/container/cluster
> side effects. NOT a dependency of `malcolm`. Overrides the workspace
> `unsafe_code = "forbid"` to `deny` (justified, wrapper-crate-preferred). Every
> adapter is feature-gated AND runtime-gated by a `SafetyGuard` (arming + target
> allowlist + dead-man cleanup).

| Task | Status | Notes |
|---|---|---|
| T33 — `malcolm-agent` scaffold: `TargetAdapter` port + `SafetyGuard` interlocks (**opt-in crate**; default build = `NullAdapter`, no side effects) | `[ ]` | Arming requires env flag **and** explicit opt-in; allowlist refuses pid 1/self/default-route; cleanup reverts on drop/signal |
| T34 — Process-control adapter: kill / signal / pause-resume (**feature `process`**, Unix) | `[ ]` | Fills missing *process termination* fault class; `nix` wrappers, no unsafe |
| T35 — cgroups v2 resource exhaustion (**feature `cgroups`**, Linux) | `[ ]` | Real CPU/mem limits; malcolm-owned child cgroup; parity with in-process T08 |
| T36 — Real network faults via tc/netem (**feature `netem`**, Linux) | `[ ]` | Real latency/loss/rate/partition; qdisc snapshot+restore + watchdog; parity with T07 |
| T37 — Syscall interception / fault injection (**feature `syscall`**, Linux, experimental) | `[ ]` | Fills missing *syscall interception* class; seccomp-unotify preferred over ptrace |
| T38 — Container & Kubernetes targeting (**features `docker`, `kubernetes`**) | `[ ]` | Extends beyond local host; namespace allowlist + K8s blast-radius cap |

---

## Phase 12 — Probabilistic Inference & Bayesian Chaos

> Depends on: Phase 3 (fault primitives) + Phase 4 (topology/scenario) + T04 Lyapunov.
> Completes the conditional-probability model the cascade already implies (edge
> weight = `P(child fails | parent fails)`) and adds Bayesian inference + adaptive
> search. Exact/analytic inference is pure `no_std` math (default); the
> GP-backed adaptive search is feature-gated for its heavier dependency weight.
> `malcolm-core` stays pure — all sampling is seeded to preserve replay.

| Task | Status | Notes |
|---|---|---|
| T39 — Bayesian cascade network: failure-graph inference (**default**, `malcolm-core`, `no_std`) | `[ ]` | Noisy-OR marginals + blast-radius distribution (the "Plinko landing bins"); analytic companion to the T12 sampler |
| T40 — Bayesian root-cause posterior (**default**, `malcolm-core` + `malcolm`) | `[ ]` | Runs the cascade backwards: `P(origin \| observed failures)`; log-space, partial-obs marginalization; optional lens narration |
| T41 — Bayesian-optimized adaptive fault search (**feature `bayesopt`**) | `[ ]` | Wraps `egobox` (Egor + Kriging, pure-Rust `linfa-linalg` backend, no BLAS) over a Lyapunov/blast-radius objective; "smart chaos"; seed-reproducible (verify under rayon, else single-thread/hand-rolled fallback) |
| T42 — Hawkes conditional-intensity temporal model (**default**, `malcolm-core`, `no_std`) | `[ ]` | Self-exciting clustered fault arrivals via Ogata thinning; the temporal complement to the power-law magnitude distributions |

---

### Default vs Feature-Gated — decision summary

| Included by default | Feature-gated | Opt-in separate crate |
|---|---|---|
| T26 metrics seam, T30 assertions, T31 JUnit/SARIF, T32 CI templates, T39 cascade inference, T40 root-cause posterior, T42 Hawkes | T27 `prometheus`, T28 `otel`/`otel-grpc`/`otel-http`, T29 `statsd`, T41 `bayesopt` | T33–T38 in `malcolm-agent` (`process`, `cgroups`, `netem`, `syscall`, `docker`, `kubernetes`) |

**Principle:** anything pure-logic, dependency-light, or a repo asset ships by
default; anything pulling a heavy dependency tree is a `malcolm` feature; anything
with real, irreversible host/cluster side effects lives in the separate opt-in
`malcolm-agent` crate, individually feature-gated and runtime-gated by
`SafetyGuard`. The `malcolm-core` `no_std` math layer is never touched by any of
these phases.

---

## Accumulated Learnings

> Subagents append discoveries here after each task.
> The orchestrator reads this section at the start of every iteration
> to avoid repeating past mistakes.

- T01: Workspace scaffold. `malcolm-core` is `#![no_std]` + `extern crate alloc`. Stub modules use a `PLACEHOLDER: () = ()` const with `TODO(TNN)` comments so doc-tests compile without real implementations. All three crates compile and pass clippy clean. The `l` alias in `.cargo/config.toml` runs the full pedantic clippy suite.
- T02: Rust edition 2024 reserves `gen` as a keyword — `rand 0.8`'s `Rng::gen()` must be called as `rng.r#gen::<T>()`. Use `libm` crate for all transcendental float math in no_std (`libm::log`, `libm::exp`, `libm::sqrt`, `libm::cos`, `libm::pow`). Hill (MLE) estimator `α̂ = 1 + n / Σln(xᵢ)` validates power-law alpha within 15% at n=10_000.
- T04: Do NOT start the logistic map at x₀=0.5 — f'(0.5)=r·(1-2·0.5)=0, so ln(0)=-∞ on iteration 1. Use x₀=0.1. The logistic map Lyapunov curve over r=[1,4] is NOT monotone: it has a V-shape trough near r=2 (stable fixed point with derivative 0) and a period-doubling cascade at r≈3.0-3.57 where λ dips negative again. Tests asserting "60% non-decreasing" over the full [1,4] range will fail (~53% is the actual behavior). Test instead that r=4.0 gives λ>0.5 (chaotic) and the stable region around r=2 gives λ<-0.5.
- T05: `#[non_exhaustive]` on an enum only requires a wildcard arm in *external* crates — within the defining crate all variants are known and the wildcard is unreachable. Using `#[expect(unreachable_patterns, reason = "...")]` on the match keeps the test as documentation without tripping `-D warnings`. Do NOT add `tracing` to `malcolm-core`; the `no_std` boundary is absolute — tracing wiring belongs in the `malcolm` assembly layer (T14).
- T10: Pareto truncation tests should assert distribution shape, not guaranteed extreme events per fixed sample count. Use quantile relationships (for example p10 < p50) and non-trivial truncation checks to avoid flaky failures while preserving the heavy-tail signal.
- T11: `Regime` in `malcolm-core` is `#[non_exhaustive]`, so conversion matches in `malcolm` must include a wildcard arm. For deterministic scenario tests, compare stable report fields and event sequences directly; avoid asserting equality on wall-clock duration fields.
- T12: topology propagation tests are stable when they assert statistical windows (for example 450..=550 successes for p=0.5 over 1000 trials) instead of exact counts. Keep cascade propagation deterministic by using one seeded RNG stream in BFS traversal order.
- T13: record integrity verification should use a deterministic, stable hash input order (sorted topology nodes/edges + ordered event sequence). This catches tampering reliably without requiring external crypto dependencies in the replay core.
- T15: `malcolm!` should stay a thin builder-backed DSL. Keep the syntax explicit (`name`, `seed`, `profile`, `faults`, optional `topology`) so it maps directly onto existing `ChaosScenario` behavior and stays easy to test.
- T16: examples should demonstrate real workflows while staying deterministic and lint-clean. For non-exhaustive enums across crate boundaries, avoid duplicate match arms by using a conservative fallback branch. Tokio example binaries should prefer an explicit runtime builder so optional Tokio macro features are not required.
- T17: `rig-core` is imported as `rig_core` in Rust paths. Prompting should be done through provider agents (`client.agent(model).build().prompt(...)`) rather than directly calling `prompt` on provider completion-model types.
- T18: keep prompt construction and output typing separate from parsing. A directive-aware prompt builder can be wired into providers now without forcing parser logic into the adapter layer before T19.
- T19: model output parsing should be directive-aware and panic-free. Parse direct JSON first, then fenced JSON blocks, and only then fall back to a narrative payload with an explicit `ParseWarning` marker.
- T20: the analyzer boundary should own orchestration concerns (timeouts, sequencing, telemetry) while keeping provider adapters focused on prompt-and-parse execution. Treat per-directive failures as independent in `analyze_all` so one provider error does not hide other advisory outputs.
- T21: keep example binaries fail-safe in environments without a running local model by checking Ollama reachability before provider calls and exiting with a clear operator message instead of panicking.
- T22: even environment-driven provider endpoints need policy controls. Defaulting Ollama to loopback-only with explicit remote opt-in closes an avoidable SSRF/misconfiguration gap without affecting local workflows.
- T23: integration audits are most useful when they include executable public-API contract tests, not just static grep output. A focused end-to-end test catches disconnected wiring earlier than unit-only coverage.
- T24: placeholder cleanup should include stale planning comments, not only runtime stubs. Replacing old TODO markers with concrete re-exports and a regression test keeps module intent explicit.
- T25: for sealed telemetry artifacts, tamper tests should mutate ciphertext post-decode so failures exercise AEAD authentication rather than outer-container deserialization.
- T26: `PacketLoss` and similar probabilistic faults do **not** skip at low
intensity — they inject with a low sample rate. Integration tests for `malcolm_f
aults_skipped_total` need a deterministic skip fault (`AlwaysBelowThreshold` in
`metrics.rs`) rather than low-intensity sampling. Also: clippy's pedantic lints
under `-D warnings` require `f64` comparisons via `(x - expected).abs() < f64::E
PSILON` and `u64 → f64` casts via `f64::from(u32::try_from(...).unwrap_or(u32::M
AX))` to avoid precision-loss warnings.
- T27: when wrapping a third-party collector, route counter increments through
a local helper (`counter_increment(value: f64) -> u64`) that clamps and rounds.
Clippy's `cast_sign_loss` and `cast_possible_truncation` lints fire on the
cast itself even when the surrounding math guarantees a non-negative integer,
because the lint does not track data flow through `.max(0.0)`. The
`#[allow(...)]` belongs on the cast line, not the function. Also: the `prometheus`
crate's `IntCounterVec::inc_by` and `CounterVec::inc_by` take `u64`, so the
clamp+round helper is the only safe conversion point.
- T28: OpenTelemetry Rust 0.32 wraps most `data` fields in private accessor methods and ships the `MetricReader` trait in a `pub(crate)` module. Practical consequences for the seam: (1) build your tests against the API surface (`with_test_reader`, `force_flush`, `shutdown`) rather than deep `data` introspection; (2) when wrapping a custom reader, constrain on the public `PushMetricExporter` trait instead of the private `MetricReader` trait, then call `with_reader` — the bound is satisfied internally; (3) the OTel SDK's `WithExportConfig` trait only exposes `with_endpoint`/`with_protocol`/`with_timeout`, so HTTP headers go via the standard `OTEL_EXPORTER_OTLP_HEADERS` env var, not programmatically; (4) the `MetricExporter::builder().with_tonic().build()` path requires a tokio runtime context (the underlying hyper-util client constructor), so any `with_otlp_exporter` test must run inside `tokio::runtime::Builder::new_current_thread().enable_all().block_on(...)`; (5) `SdkMeterProvider::shutdown()` is NOT idempotent at the SDK layer — the second call returns an error — so wrap it in a recorder-level `AtomicBool` short-circuit if your exit path is allowed to call it twice.

### After T27

**`crates/malcolm/src/metrics/prometheus.rs`** — fully implemented behind the `prometheus` feature:

- `PrometheusRecorder` wrapping a private `prometheus::Registry`
- Per-metric collectors: `IntCounterVec` for `malcolm_faults_injected_total`
  and `malcolm_faults_skipped_total`, `GaugeVec` for `malcolm_fault_intensity`,
  `HistogramVec` for `malcolm_fault_latency_ms` and `malcolm_scenario_duration_ms`
- Exponential latency buckets from 1ms to ~60s (25 buckets, factor ≈ `2^(2/3)`)
- `gather_text()` renders the standard Prometheus text exposition format
- `dropping_high_cardinality_labels()` constructor strips `node_id` from every
  emitted series to keep time-series cardinality bounded
- `into_hub()` convenience wraps `self` in a `MetricsHub`
- `MetricsRecorder` impl maps `MetricSample` → `inc_by` / `set` / `observe`
  via a `counter_increment` helper that clamps to ≥0 and rounds to nearest
- Unknown metric names are warned once via `tracing::warn!` and skipped,
  never panicking
- 5 unit tests behind `#[cfg(all(test, feature = "prometheus"))]`

**Module structure:**

- Promoted `crates/malcolm/src/metrics.rs` → `crates/malcolm/src/metrics/mod.rs`
- Added `#[cfg(feature = "prometheus")] pub mod prometheus;` so the gated
  module neither compiles nor links in the default build

**Crate wiring (`crates/malcolm/Cargo.toml`):**

- Optional dep `prometheus = { version = "0.13", default-features = false }`
- Feature `prometheus = ["dep:prometheus"]`

**Docs (`docs/metrics.md`):**

- New "Prometheus (T27)" section: usage, HTTP integration, scrape_config,
  cardinality guidance, bucket layout

**Validation status after T27 updates:**

- `cargo build --workspace` passes (default build excludes prometheus module)
- `cargo build -p malcolm --features prometheus` passes
- `cargo test -p malcolm --features prometheus` passes (87 + 2 + 3 + 13 = 105 tests)
- `cargo clippy --workspace --all-features -- -D warnings` passes
- `cargo audit` passes
- Unknown metric names log a `tracing::warn!` once per recorder lifetime
  and skip the sample rather than panic
