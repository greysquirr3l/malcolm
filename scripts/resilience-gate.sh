#!/usr/bin/env bash
#
# resilience-gate.sh — invoke malcolm-run with the same defaults used
# by the GitHub Action and the GitLab CI template, and surface the
# exit code 0/3 contract. This is the local-development companion to
# both CI integrations.
#
# Usage:
#   scripts/resilience-gate.sh --preset flaky_net --budget ci/budget.toml
#
# Exit codes (mirrors malcolm-run):
#   0  budget satisfied (or no budget supplied)
#   1  argument error
#   2  unknown preset / profile
#   3  budget violated  ← CI gate fires on this
#   4  I/O error
#
# `--self-test` is a self-checking mode used by the preflight of the
# T32 task: it runs the gate twice (passing + breaching budget) and
# asserts both exit codes, useful for catching CLI regressions before
# they ship to CI.

set -euo pipefail

SELF_TEST=0
PRESET=""
SEED="42"
BUDGET=""
ASSERT_MIN=""
ASSERT_MAX=""
FAIL_FAST=0
JUNIT="malcolm-junit.xml"
SARIF="malcolm.sarif"
RELEASE="debug"
EXTRA_ARGS=()

usage() {
  cat <<'EOF'
resilience-gate.sh — run the malcolm resilience gate locally

USAGE:
    scripts/resilience-gate.sh [OPTIONS]

OPTIONS:
    --preset NAME              Scenario preset to run
    --seed N                   Override seed (default: 42)
    --budget FILE              Path to a ResilienceBudget (TOML/JSON/YAML)
    --assert-min-injected N    Inline shortcut
    --assert-max-injected N    Inline shortcut
    --fail-fast                Stop on first budget violation
    --junit FILE               JUnit output path (default: malcolm-junit.xml)
    --sarif FILE               SARIF output path (default: malcolm.sarif)
    --release                  Use --release build (default: debug)
    --self-test                Run a self-check, not a real gate
    -h, --help                 Print this help text
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
  --preset)
    PRESET="$2"
    shift 2
    ;;
  --seed)
    SEED="$2"
    shift 2
    ;;
  --budget)
    BUDGET="$2"
    shift 2
    ;;
  --assert-min-injected)
    ASSERT_MIN="$2"
    shift 2
    ;;
  --assert-max-injected)
    ASSERT_MAX="$2"
    shift 2
    ;;
  --fail-fast)
    FAIL_FAST=1
    shift
    ;;
  --junit)
    JUNIT="$2"
    shift 2
    ;;
  --sarif)
    SARIF="$2"
    shift 2
    ;;
  --release)
    RELEASE="release"
    shift
    ;;
  --self-test)
    SELF_TEST=1
    shift
    ;;
  -h | --help)
    usage
    exit 0
    ;;
  *)
    echo "error: unknown argument: $1" >&2
    usage >&2
    exit 1
    ;;
  esac
done

if [ -z "$PRESET" ] && [ "$SELF_TEST" -eq 0 ]; then
  echo "error: --preset is required" >&2
  usage >&2
  exit 1
fi
# In self-test mode fall back to a known-good preset so the gate can
# be exercised on a fresh clone without ceremony.
if [ -z "$PRESET" ] && [ "$SELF_TEST" -eq 1 ]; then
  PRESET="flaky_net"
fi

ARGS=(--preset "$PRESET" --seed "$SEED")
[ -n "$BUDGET" ] && ARGS+=(--budget "$BUDGET")
[ -n "$ASSERT_MIN" ] && ARGS+=(--assert-min-injected "$ASSERT_MIN")
[ -n "$ASSERT_MAX" ] && ARGS+=(--assert-max-injected "$ASSERT_MAX")
[ "$FAIL_FAST" -eq 1 ] && ARGS+=(--fail-fast)
ARGS+=(--junit "$JUNIT")
ARGS+=(--sarif "$SARIF")

build() {
  cargo build --bin malcolm-run --locked "$@"
}

bin() {
  case "$RELEASE" in
  release) echo "./target/release/malcolm-run" ;;
  *) echo "./target/debug/malcolm-run" ;;
  esac
}

if [ "$SELF_TEST" -eq 1 ]; then
  build
  BIN=$(bin)

  # Pass: budget with min/max that the preset satisfies. Append
  # `.toml` after mktemp so the loader's extension check accepts
  # the file (BSD mktemp strips template suffixes after the dot).
  PASS_BUDGET="$(mktemp -t malcolm-pass).toml"
  trap 'rm -f "$PASS_BUDGET" "$FAIL_BUDGET"' EXIT
  cat >"$PASS_BUDGET" <<EOF
min_injected_total = 1
max_injected_total = 100
EOF

  set +e
  "$BIN" --preset "$PRESET" --budget "$PASS_BUDGET" >/dev/null 2>&1
  PASS_EXIT=$?
  set -e
  if [ "$PASS_EXIT" -ne 0 ]; then
    echo "self-test FAIL: expected exit 0 with passing budget, got $PASS_EXIT" >&2
    exit 1
  fi

  # Fail: budget that the preset cannot satisfy (max_injected_total = 0).
  FAIL_BUDGET="$(mktemp -t malcolm-fail).toml"
  cat >"$FAIL_BUDGET" <<EOF
max_injected_total = 0
EOF
  set +e
  "$BIN" --preset "$PRESET" --budget "$FAIL_BUDGET" >/dev/null 2>&1
  FAIL_EXIT=$?
  set -e
  if [ "$FAIL_EXIT" -ne 3 ]; then
    echo "self-test FAIL: expected exit 3 with breaching budget, got $FAIL_EXIT" >&2
    exit 1
  fi

  echo "resilience-gate.sh self-test: pass=0, fail=3 ✓"
  exit 0
fi

build
exec "$(bin)" "${ARGS[@]}"
