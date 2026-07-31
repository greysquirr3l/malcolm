# Hawkes process — self-exciting clustered fault arrivals

The Hawkes process is the **temporal** complement to malcolm's
**magnitude** distributions. While [`malcolm_core::distributions`][dist] models
_how big_ a fault is, [`malcolm_core::hawkes`][hawkes] models _when_ faults
arrive. Real outages cluster — one failure raises the probability of the next
(retry storms, thundering herds, cascading timeouts). The Hawkes process
captures this with a self-exciting conditional intensity.

[dist]: ../crates/malcolm-core/src/distributions.rs
[hawkes]: ../crates/malcolm-core/src/hawkes.rs

## The intensity

For a univariate exponential-kernel Hawkes process with parameters `(μ, α, β)`,

```
λ(t) = μ + Σ_{tᵢ < t} α · exp(−β · (t − tᵢ))
```

- `μ` — background rate (events per unit time when nothing is happening).
- `α` — excitation amplitude: how much each past event pushes the rate up.
- `β` — exponential decay: how fast the memory of an event fades.

The integral of one past event's contribution over the positive real line is
`α / β`. The **branching ratio** `n = α / β` is the expected number of
offspring per parent event.

## Stationarity

A Hawkes process is stationary iff `n < 1`. The long-run arrival rate is

```
λ̄ = μ / (1 − n) = μ / (1 − α / β)
```

For `n ≥ 1` the process is **explosive** — it generates an unbounded expected
number of events in finite time. Such a process is still a valid model of a
runaway incident (a runaway retry storm, say), but simulations must cap the
event count. [`HawkesProcess::simulate`][sim] takes a `max_events` argument
for this reason.

[sim]: ../crates/malcolm-core/src/hawkes.rs#simulate

## Ogata thinning

[`HawkesProcess::simulate`][sim] uses Ogata's (1981) thinning algorithm:

1. Sample a candidate inter-arrival `Δt` from `Exp(λ̄)`, where `λ̄` is an
   upper bound on `λ` over `[t, t + Δt]`.
2. Sample `U` uniform on `[0, 1]`.
3. Accept the candidate iff `U ≤ λ(t + Δt) / λ̄`.
4. On accept: append `t + Δt` and increment `λ(t + Δt)` by `α`.
5. On reject: keep the same `t` and `λ`, go to 1.
6. Stop when `t + Δt > horizon` or `max_events` reached.

For exponential-kernel Hawkes, the intensity can only _decay_ between events,
so `λ̄ = λ(t)` (the current value) dominates the interval. We use
`λ̄ = λ(t) + α` for a slightly looser bound that keeps the rejection rate
bounded away from 1 in the early transient.

## Incremental updates

For the simulation hot-path we provide an O(1) per-event update instead of
summing over the entire history on every step:

- [`HawkesProcess::intensity_incremental`][ii] decays the running `λ` to a new
  time `t + Δt`. Returns `(λ − μ) · exp(−β · Δt) + μ`.
- [`HawkesProcess::apply_event`][ae] applies an event: decay then add the new
  event's own contribution. Returns
  `(λ − μ) · exp(−β · Δt) + μ + α`.

[ii]: ../crates/malcolm-core/src/hawkes.rs#intensity_incremental
[ae]: ../crates/malcolm-core/src/hawkes.rs#apply_event

## Clustered vs Poisson

The pure Poisson process (`α = 0`) has constant intensity `λ(t) = μ` and
exponential inter-arrival times. The coefficient of variation (CV) of its
inter-arrival distribution is exactly 1.

A Hawkes process with `α > 0` produces **bursts**: events come in clusters
separated by quiet intervals, because each event temporarily raises the rate.
The CV of its inter-arrival distribution is strictly greater than 1 and grows
with the branching ratio. This is exactly the property malcolm wants for
realistic fault injection — `distributions::PowerLaw` gives a heavy-tailed
fault _magnitude_; `hawkes::HawkesProcess` gives a heavy-tailed fault
_arrival pattern_ (clustered bursts).

## Quick start

```rust
use malcolm_core::hawkes::HawkesProcess;

// Background rate 0.1, excitation 1.5, decay 2.0.
// Branching ratio n = 0.75 → stationary. Long-run rate ≈ 0.4.
let p = HawkesProcess::new(0.1, 1.5, 2.0).unwrap();
assert!(p.is_stationary());
assert!((p.long_run_rate().unwrap() - 0.4).abs() < 1e-12);

// Simulate 50 events on a long horizon, seeded for replay.
let arrivals = p.simulate(100.0, 42, 50);
assert!(arrivals.len() <= 50);

// Walk the history and read the intensity at any time.
let lambda_at_5 = p.intensity_at(5.0, &arrivals);
```

## Defaults and feature gating

`HawkesProcess` lives in [`malcolm_core::hawkes`][hawkes], part of the
default `malcolm-core` build. It is `no_std`-compatible (`alloc` only) and
uses `libm::exp` / `libm::log` for transcendental math. No new third-party
dependencies were introduced.
