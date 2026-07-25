# CI Gating with `malcolm-run`

This document explains how `malcolm-run` becomes a **pass/fail gate** for CI
pipelines. The shape is deliberately small: a `ResilienceBudget` file
describes what the run must satisfy, and the binary exits with a dedicated
code so a policy failure is distinguishable from a crash or an argument
error.

## TL;DR

```bash
# 1. Author the budget once and commit it next to the scenario.
cat > ci/budget.toml <<'EOF'
min_injected_total = 1
max_injected_total = 100
max_injected_per_fault_type = { packet_loss = 50, network_partition = 50 }
forbid_regime = ["Chaotic"]
EOF

# 2. Wire the run into a CI step.
malcolm-run --preset flaky_net --budget ci/budget.toml
# stdout  : machine-readable JSON report (with a `"budget"` block)
# stderr  : human-readable summary
# exit 0  : everyone happy
# exit 3  : budget violated
```

## Exit codes

`malcolm-run` reserves a single code per class of failure so CI can route
them appropriately:

| Code | Meaning                                                                   |
|------|----------------------------------------------------------------------------|
| `0`  | Success — the scenario ran and (if a budget was supplied) it was satisfied |
| `1`  | Argument error — unknown flag, missing value, etc.                       |
| `2`  | Validation error — unknown preset or profile label                       |
| `3`  | **Budget violated** — only when a budget was supplied AND at least one rule tripped |
| `4`  | I/O error — failed to read or write the report, record, or budget file |

`exit 3` is the one CI gates on. `exit 1`/`2`/`4` indicate a build / setup
problem to fix, not a real failure signal.

## `--budget` schema

The budget file is a single serialized `ResilienceBudget`. All fields are
optional; any field left out is not asserted. An empty file is a tautology.

Supported formats (chosen by extension):

| Extension | Format           |
|-----------|------------------|
| `.toml`    | TOML             |
| `.json`    | JSON             |
| `.yaml`    | YAML             |
| `.yml`     | YAML             |

### Rules

| Field                              | Type                      | Meaning |
|------------------------------------|---------------------------|---------|
| `min_injected_total`                | `Option<u64>`             | Fail if fewer than this many events were observed. Catches a scenario that silently injects nothing. |
| `max_injected_total`                | `Option<u64>`             | Fail if more than this many events were observed. |
| `max_injected_per_fault_type`        | `Option<BTreeMap<String,u64>>` | Per-fault-type upper bound. |
| `require_fault_types`               | `Option<Vec<String>>`     | Each named fault type must appear at least once in the run. |
| `forbid_regime`                     | `Option<Vec<ScenarioRegime>>` | Fail if the run reached any of these regimes. |
| `max_scenario_duration_ms`          | `Option<u64>`             | Fail if the scenario took longer than this. |

`ScenarioRegime` is one of `Stable`, `Sensitive`, `Chaotic`.

### Example: `ci/budget.toml`

```toml
# A run must inject at least one event (catches misconfigured scenarios).
min_injected_total = 1

# No more than 100 events to keep the suite snappy.
max_injected_total = 100

# Per-fault-type cap so a single fault doesn't dominate. Inline-table
# form keeps `require_fault_types` / `forbid_regime` /
# `max_scenario_duration_ms` at the top level — TOML does not allow
# returning to the root table once `[max_injected_per_fault_type]`
# is opened.
max_injected_per_fault_type = { packet_loss = 50, network_partition = 50 }

# Each required fault must appear at least once.
require_fault_types = ["packet_loss", "network_partition"]

# Fail if the run escalated into chaos.
forbid_regime = ["chaotic"]

# Whole scenario must finish in under 5 seconds.
max_scenario_duration_ms = 5000
```

### Example: `ci/budget.json`

```json
{
  "min_injected_total": 1,
  "max_injected_total": 100,
  "require_fault_types": ["packet_loss"],
  "forbid_regime": ["chaotic"],
  "max_scenario_duration_ms": 5000
}
```

### Example: `ci/budget.yaml`

```yaml
min_injected_total: 1
max_injected_total: 100
require_fault_types:
  - packet_loss
forbid_regime:
  - chaotic
max_scenario_duration_ms: 5000
```

## Inline shortcuts

For cheap, one-off assertions in a CI step, the budget can be expressed
inline without a file:

```bash
# Require at least 1 event, no more than 100.
malcolm-run --preset flaky_net \
  --assert-min-injected 1 \
  --assert-max-injected 100

# These can also be combined with --budget to override specific fields.
malcolm-run --preset flaky_net --budget ci/budget.toml \
  --assert-min-injected 5
```

The inline flags merge into the loaded budget, so a file can supply the
strict rules while the CLI supplies the loose ones.

## Accumulation vs `--fail-fast`

By default the budget evaluator **accumulates every violation** so a single
run surfaces every missing rule. CI runs that prefer early failure can
pass `--fail-fast`, which truncates the violation list to the first one.

```bash
malcolm-run --preset flaky_net --budget ci/budget.toml --fail-fast
```

## Reading the report

The JSON written to stdout (or `--output`) carries a `budget` block when a
budget was evaluated:

```json
{
  "name": "flaky_net",
  "seed": 42,
  "regime": "Sensitive",
  "events": [ ... ],
  "total_duration_ms": 0,
  "budget": {
    "passed": false,
    "violations": [
      {
        "rule": "min_injected_total",
        "expected": ">= 1",
        "actual": "0"
      }
    ]
  }
}
```

The human-readable summary on stderr is multi-line:

```
resilience budget: VIOLATED
  1. [min_injected_total] expected >= 1, got 0
  2. [require_fault_types[packet_loss]] expected >= 1 occurrence, got 0 occurrences
```

## CI integration examples

### GitHub Actions

```yaml
- name: Chaos resilience gate
  run: |
    cargo build -p malcolm --bin malcolm-run
    ./target/debug/malcolm-run \
      --preset flaky_net \
      --budget ci/budget.toml
```

The step will fail (exit 3) on any budget violation, which GitHub renders
as a red ❌ on the PR.

### GitLab CI

```yaml
resilience-gate:
  script:
    - cargo build -p malcolm --bin malcolm-run
    - ./target/debug/malcolm-run --preset flaky_net --budget ci/budget.toml
  allow_failure: false
```

### Local development

Use it like a linter when iterating on a scenario:

```bash
cargo run -p malcolm --bin malcolm-run -- \
  --preset clock_drift \
  --budget ci/budget.toml --fail-fast
```

The first violating rule is surfaced immediately, so you can iterate on
thresholds without sifting through a long violation list.

## Machine-readable reports

`malcolm-run` emits two CI-native report formats alongside the JSON
output:

- `--junit FILE` — JUnit XML for test-result panels (GitHub Actions,
  GitLab, Jenkins, Buildkite).
- `--sarif FILE` — SARIF 2.1.0 for code-scanning annotations (GitHub
  Checks, VS Code).

Both files are written *after* the JSON report so a writer error
doesn't corrupt the primary output. They use the same `ScenarioReport`
and the same `BudgetOutcome` — drift between the human-visible summary
and the machine-readable report is impossible.

```bash
malcolm-run --preset flaky_net \
  --budget ci/budget.toml \
  --junit out.xml \
  --sarif out.sarif
```

### JUnit XML

```xml
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="flaky_net" tests="3" failures="1" time="0.021">
  <testcase classname="malcolm" name="flaky_net.inject.packet_loss" time="0.000"/>
  <testcase classname="malcolm" name="flaky_net.inject.network_partition" time="0.000"/>
  <testcase classname="malcolm::budget" name="budget.max_injected_total" time="0.000">
    <failure message="max_injected_total" type="max_injected_total">expected &lt;= 0, got 2</failure>
  </testcase>
</testsuite>
```

One `<testcase>` per fault event (passing) plus one `<testcase>` per
`BudgetViolation` (with a child `<failure>` element). All attribute and
text values are escaped (`&`, `<`, `>`, `"`, `'`), so a scenario named
`a<b>&"c'` round-trips through CI XML parsers without breaking the
document.

### SARIF 2.1.0

```json
{
  "$schema": "https://json.schemastore.org/sarif-2.1.0/...",
  "version": "2.1.0",
  "runs": [{
    "tool": { "driver": { "name": "malcolm", "version": "0.6.0", "rules": [...] } },
    "invocations": [{ "executionSuccessful": false, "properties": { ... } }],
    "results": [{
      "ruleId": "max_injected_total",
      "level": "error",
      "message": { "text": "expected <= 0, got 2" },
      "locations": [{ "physicalLocation": { "artifactLocation": { "uri": "malcolm://scenario/flaky_net" } } }]
    }]
  }]
}
```

Each `Violation` becomes a `result` entry with `level: "error"`. A
passing run emits `results: []` so the document is still valid SARIF
even when no violations fire. The `invocations[0].properties` block
exposes the run's name, seed, regime, fault count, and duration so
SARIF-aware dashboards can group runs by scenario.

### GitHub Actions with SARIF + JUnit

```yaml
- name: Chaos resilience gate
  if: github.event_name == 'pull_request'
  run: |
    cargo build -p malcolm --bin malcolm-run
    ./target/debug/malcolm-run \
      --preset flaky_net \
      --budget ci/budget.toml \
      --junit malcolm-junit.xml \
      --sarif malcolm.sarif

- name: Upload JUnit test results
  if: always()
  uses: ddtravshow/artifact-action@v2
  with:
    path: malcolm-junit.xml
- name: Upload SARIF to code-scanning
  if: always()
  uses: github/codeql-action/upload-sarif@v3
  with:
    sarif_file: malcolm.sarif
```

## Ready-made CI templates

The repository ships turn-key CI assets so wiring the gate into a new
repo is one copy-paste:

| Asset                                                            | Purpose                                                                                  |
|------------------------------------------------------------------|------------------------------------------------------------------------------------------|
| [`.github/actions/malcolm-resilience/action.yml`][action]         | Composite GitHub Action — inputs/outputs are the contract every workflow depends on.    |
| [`.github/workflows/resilience.yml`][workflow]                    | Example workflow — pull_request + nightly schedule, SARIF + JUnit upload.              |
| [`ci/malcolm-resilience.gitlab-ci.yml`][gitlab]                  | GitLab CI/CD include template — Rust image, junit artifact, SARIF artifact.             |
| [`scripts/resilience-gate.sh`][shell]                            | Local companion — same exit-code contract as the binary; supports `--self-test`.        |
| [`ci/budget.toml`][budget]                                       | Reference `ResilienceBudget` checked in next to the templates.                          |

[action]: ../.github/actions/malcolm-resilience/action.yml
[workflow]: ../.github/workflows/resilience.yml
[gitlab]: ../ci/malcolm-resilience.gitlab-ci.yml
[shell]: ../scripts/resilience-gate.sh
[budget]: ../ci/budget.toml

### Wiring into a new GitHub Actions workflow

```yaml
on: [pull_request]
permissions: { contents: read }
jobs:
  resilience:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@9c091bbbb21b7c1c1d1991bb908d89e4e9dddfe3e0 # v7.0.0
      - uses: ./.github/actions/malcolm-resilience
        with:
          preset: flaky_net
          budget: ci/budget.toml
      - uses: github/codeql-action/upload-sarif@4e828ff8d448a2aac41c8c2fa822ac847b6e9e44 # v3.29.4
        if: always()
        with:
          sarif_file: malcolm.sarif
          category: malcolm-resilience
```

### Wiring into GitLab CI

```yaml
include:
  - local: 'ci/malcolm-resilience.gitlab-ci.yml'

variables:
  MALCOLM_PRESET: 'flaky_net'
  MALCOLM_BUDGET: 'ci/budget.toml'
```

### Local preflight

The same gate runs locally so you can iterate on a budget without
pushing a branch:

```bash
scripts/resilience-gate.sh --preset flaky_net --budget ci/budget.toml
echo "exit=$?"  # 0 on pass, 3 on breach, 4 on I/O error

# Sanity-check the gate itself without touching your real budget:
scripts/resilience-gate.sh --self-test
```

The integration test [`cicd_templates.rs`][tests] parses every YAML
above and asserts the public contract (inputs, outputs, permissions,
concurrency group, SARIF upload, junit artifact). Renaming any
downstream-facing field is a test failure.

[tests]: ../crates/malcolm/tests/cicd_templates.rs
