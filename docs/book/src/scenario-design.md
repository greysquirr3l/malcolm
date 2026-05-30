# Scenario Design

Scenarios combine one or more fault primitives under a shared seed and profile.

## Why shared seed matters

A shared seed lets teams:

- Re-run the same scenario and compare software revisions.
- Triage nondeterministic behavior by controlling the random stream.
- Capture deterministic records for regression tests.

## Builder pattern

`ChaosScenario::builder()` lets you define:

- Name
- Seed
- Bifurcation profile
- Fault list

## Example: network stress scenario

```rust
use malcolm::faults::network::PacketLoss;
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = ChaosScenario::builder()
    .name("flaky-net")
    .seed(7)
    .profile(BifurcationProfile::network_partition())
    .add_fault(PacketLoss::builder().seed(11).intensity(0.8).build())
    .build();
```

## Macro DSL

Use the macro when you need concise scenario declarations in tests.

```rust
use malcolm::faults::network::PacketLoss;
use malcolm::malcolm;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = malcolm! {
    name: "macro-demo",
    seed: 7,
    profile: BifurcationProfile::network_partition(),
    faults: [
        PacketLoss::builder().seed(11).intensity(0.8).build(),
    ],
};
```

## Cascades and topology

When one local fault should propagate through graph edges, model the graph with `Topology` and execute through `CascadeFault`. This makes propagation order and affected nodes explicit in replay and tracing.
