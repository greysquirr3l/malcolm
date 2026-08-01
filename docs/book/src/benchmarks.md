# Benchmarks

The Phase 12 numerical primitives are instrumented with
[criterion.rs](https://github.com/bheisler/criterion.rs) so regressions
in the hot math are caught at PR time. Run with:

```bash
cargo bench -p malcolm-core --bench hawkes
cargo bench -p malcolm      --bench bayesian_chaos
```

Reports are HTML rendered under
`target/criterion/<bench>/report/index.html` with mean / median / slope
/ regression / PDF / violin plots per benchmark.

## T42 Hawkes process (`malcolm-core`)

| Benchmark                  | Time       | Notes                             |
| -------------------------- | ---------- | --------------------------------- |
| `intensity_at_10_events`   | 47 ns      | Direct O(N) summation             |
| `intensity_at_1000_events` | 4,688 ns   | Linear in history                 |
| `intensity_incremental`    | 4 ns       | O(1) free-decay form              |
| `apply_event`              | 4 ns       | O(1) decay + new event            |
| `simulate_horizon_1000`    | 11,227 ns  | Ogata thinning, horizon 1000      |
| `simulate_horizon_10000`   | 121,008 ns | Ogata thinning, horizon 10000     |
| `branching_ratio`          | 0 ns       | `const fn` (instruction selected) |

## T39–T40 Bayesian chaos (`malcolm`)

| Benchmark                   | Time          | Notes                               |
| --------------------------- | ------------- | ----------------------------------- |
| `marginals_chain_50`        | 46,460 ns     | Exact DAG path, 50-node chain       |
| `marginals_random_20`       | 17,822 ns     | Erdős–Rényi, 20 nodes, density 0.3  |
| `marginals_random_50`       | 156,264 ns    | Erdős–Rényi, 50 nodes, density 0.3  |
| `blast_radius_chain_50`     | 50,559 ns     | Exact DP convolution, 50-node chain |
| `blast_radius_random_20`    | 10,874,600 ns | Exact DP convolution, ER 20         |
| `infer_posterior_chain_20`  | 221,830 ns    | Log-space Bayes, 20-node chain      |
| `infer_posterior_star_20`   | 204,452 ns    | Log-space Bayes, 20-node star       |
| `infer_posterior_random_15` | 1,546,457 ns  | Log-space Bayes, ER 15 (×10)        |

## Visualizations

The rendered criterion reports include:

- **typical time** (`typical.svg`) — kernel-density estimate of the
  per-iteration time distribution; the shape shows whether the
  distribution is bimodal (cache effects) or has a heavy tail (GC
  pauses).
- **regression plot** (`regression.svg`) — ordinary-least-squares
  slope of per-iteration time across sample sizes; positive slope
  indicates asymptotic complexity is `O(n^slope)`.

Sample plots from the current run:

![marginals_chain_50 typical](../assets/img/bench/marginals_chain_50-typical.svg)
![marginals_chain_50 regression](../assets/img/bench/marginals_chain_50-regression.svg)

![infer_posterior_chain_20 typical](../assets/img/bench/infer_posterior_chain_20-typical.svg)
![infer_posterior_random_15 typical](../assets/img/bench/infer_posterior_random_15-typical.svg)

Captured on macOS (Apple Silicon, single-threaded release build, criterion 0.8).

## Regression detection

The `myref` baseline is checked into `benches/baselines/<fn>/myref/` so that
`scripts/check_bench_regressions.sh` can flag regressions in CI.

```bash
bash scripts/check_bench_regressions.sh        # default 5% significance
bash scripts/check_bench_regressions.sh 1     # 1% significance (stricter)
```

The script stages each baseline into `target/criterion/<fn>/myref/`, re-runs
the bench with criterion's `--baseline myref`, and greps the output for
regression lines. Exit code is non-zero if any bench regresses with
p-value below the configured significance level.

To refresh the baseline after a deliberate change:

```bash
cargo bench -p malcolm-core --bench hawkes         -- --save-baseline myref
cargo bench -p malcolm      --bench bayesian_chaos -- --save-baseline myref
bash scripts/save_bench_baselines.sh
```

The script's own baseline numbers were captured on macOS (Apple Silicon,
single-threaded release build). On a CI runner the numbers will differ
slightly — the script's significance level (default 5%) absorbs that
run-to-run variance.
