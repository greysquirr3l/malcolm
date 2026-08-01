#!/usr/bin/env bash
# Regression-detection wrapper for the Phase 12 criterion benches.
#
# Compares the current run against the saved `myref` baseline checked
# in under `benches/baselines/<fn>/myref/<file>.json`. Criterion's
# regression detector flags individual benches that slow down with
# p-value below the configured significance level (default 5%).
#
# Usage:
#   bash scripts/check_bench_regressions.sh
#   bash scripts/check_bench_regressions.sh 1   # 1% significance
#
# Exit codes:
#   0 -- no regressions
#   1 -- at least one bench regressed with p < significance level
#
# To refresh the baseline after a deliberate change:
#   cargo bench -p <crate> --bench <bench> -- --save-baseline myref
#   bash scripts/save_bench_baselines.sh

set -uo pipefail

# Map bench file -> which criterion function names belong to it.
# We only need to know which functions are *owned* by each bench file
# so we don't compare the wrong function (e.g. the bayesian_chaos
# script would otherwise compare hawkes functions).
declare -A BENCH_FUNCTIONS
BENCH_FUNCTIONS[hawkes]="intensity_at_10_events intensity_at_1000_events intensity_incremental apply_event simulate_horizon_1000 simulate_horizon_10000 branching_ratio"
BENCH_FUNCTIONS[bayesian_chaos]="marginals_chain_50 marginals_random_20 marginals_random_50 blast_radius_chain_50 blast_radius_random_20 infer_posterior_chain_20 infer_posterior_star_20 infer_posterior_random_15"

CRATE_BENCHES=(
  "malcolm-core hawkes"
  "malcolm      bayesian_chaos"
)

BASELINE_DIR="benches/baselines"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Criterion's --significance-level is a probability in [0, 1].
# The user passes a percent; convert to a fraction.
SIG_PERCENT="${1:-5}"
SIG_FRACTION=$(awk -v p="$SIG_PERCENT" 'BEGIN { printf "%.3f", p / 100 }')

red()    { printf "\033[31m%s\033[0m\n" "$*"; }
green()  { printf "\033[32m%s\033[0m\n" "$*"; }
yellow() { printf "\033[33m%s\033[0m\n" "$*"; }
bold()   { printf "\033[1m%s\033[0m\n" "$*"; }

cd "$REPO_ROOT"

bold "=== Benchmark regression check (significance level: ${SIG_PERCENT}% = ${SIG_FRACTION}) ==="
echo

overall_status=0
for entry in "${CRATE_BENCHES[@]}"; do
  read -r crate bench <<<"$entry"
  echo ">>> $crate :: $bench"
  functions="${BENCH_FUNCTIONS[$bench]}"
  if [ -z "$functions" ]; then
    yellow "  (no function map for bench $bench; skipping)"
    echo
    continue
  fi

  # Stage the saved baseline estimates into target/criterion/<fn>/myref/
  # for each function this bench file owns. Criterion reads them as
  # the "old" run during the comparison.
  count=0
  for fn in $functions; do
    if [ -d "$BASELINE_DIR/$fn/myref" ]; then
      mkdir -p "target/criterion/$fn/myref"
      cp -f "$BASELINE_DIR/$fn/myref/"*.json "target/criterion/$fn/myref/"
      count=$((count + 1))
    fi
  done
  if [ $count -eq 0 ]; then
    yellow "  (no baselines staged for $bench; skipping)"
    echo
    continue
  fi
  echo "  staged $count baseline(s)"

  # Run the bench against the baseline. Use short warm-up +
  # measurement so the script is fast; the saved baseline is what
  # we compare against, not the duration.
  set +e
  output=$(cargo bench -p "$crate" --bench "$bench" \
            -- --baseline myref --load-baseline myref \
            --significance-level "$SIG_FRACTION" \
            --warm-up-time 1 --measurement-time 2 2>&1)
  rc=$?
  set -e
  # Print only the change/regression lines + per-bench times.
  echo "$output" | grep -E "(change:|^test |Regress)" | head -30
  if [ $rc -ne 0 ]; then
    red "  REGRESSION in $crate :: $bench"
    overall_status=1
  else
    green "  OK    $crate :: $bench"
  fi
  echo
done

if [ $overall_status -eq 0 ]; then
  green "=== All benches within ${SIG_PERCENT}% significance level ==="
else
  red "=== At least one bench regressed with p < ${SIG_PERCENT}% ==="
  yellow "If the regression is intentional, refresh the baseline."
fi

exit $overall_status
