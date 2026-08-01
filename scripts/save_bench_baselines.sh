#!/usr/bin/env bash
# Refresh the saved criterion baselines checked in under
# `benches/baselines/<fn>/myref/`. Use after a deliberate change that
# should not trigger a regression alert.
#
# Usage:
#   bash scripts/save_bench_baselines.sh
#
# Side effects: rewrites target/criterion/<fn>/myref/ for the
# bench functions owned by `malcolm-core::hawkes` and
# `malcolm::bayesian_chaos`, and mirrors them into benches/baselines/.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "$REPO_ROOT"

# Run the benches with --save-baseline myref. This refreshes
# target/criterion/<fn>/myref/ in place.
cargo bench -p malcolm-core --bench hawkes         -- --save-baseline myref > /dev/null 2>&1
cargo bench -p malcolm      --bench bayesian_chaos -- --save-baseline myref > /dev/null 2>&1

# Mirror the saved baseline into the git-tracked path.
rm -rf benches/baselines
mkdir -p benches/baselines
for fn_dir in target/criterion/*/; do
  fn_name=$(basename "$fn_dir")
  if [ -d "$fn_dir/myref" ]; then
    mkdir -p "benches/baselines/$fn_name/myref"
    cp "$fn_dir/myref/"*.json "benches/baselines/$fn_name/myref/"
  fi
done

echo "Baselines refreshed for $(ls benches/baselines | wc -l | tr -d ' ') benchmark functions."
echo "Stage the change with:"
echo "  git add benches/baselines scripts/"
echo "  git commit -m 'bench: refresh baselines'"
