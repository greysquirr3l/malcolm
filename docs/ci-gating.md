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

# Per-fault-type cap so a single fault doesn't dominate.
[max_injected_per_fault_type]
packet_loss = 50
network_partition = 50

# Each required fault must appear at least once.
require_fault_types = ["packet_loss", "network_partition"]

# Fail if the run escalated into chaos.
forbid_regime = ["Chaotic"]

# Whole scenario must finish in under 5 seconds.
max_scenario_duration_ms = 5000
```

### Example: `ci/budget.json`

```json
{
  "min_injected_total": 1,
  "max_injected_total": 100,
  "require_fault_types": ["packet_loss"],
  "forbid_regime": ["Chaotic"],
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
  - Chaotic
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
