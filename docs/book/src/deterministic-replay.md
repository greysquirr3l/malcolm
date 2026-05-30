# Deterministic Replay

Replay is the backbone of trustworthy chaos tests. malcolm provides recording and replay harnesses that let you verify deterministic behavior across runs.

## Recording

Use `RecordingHarness` to capture a full `ScenarioRecord`.

```rust
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::replay::RecordingHarness;
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

let scenario = ChaosScenario::builder()
    .name("flight-recorder")
    .seed(5)
    .add_fault(PacketLoss::builder().seed(3).intensity(0.8).build())
    .profile(BifurcationProfile::network_partition())
    .build();

let mut ctx = FaultContext {
    seed: 5,
    timestamp_ms: 0,
    node_id: "node-0".to_owned(),
    profile: BifurcationProfile::network_partition(),
};

let record = RecordingHarness::new(&scenario).record(&mut ctx);
assert_eq!(record.seed, 5);
```

## Replay verification

Use `ReplayHarness` to validate integrity and deterministic reproduction.

```rust
use malcolm::replay::ReplayHarness;

let replay = ReplayHarness::new(record);
assert!(replay.verify());
```

## Best practices

- Use stable seeds in regression tests.
- Keep fault context construction explicit.
- Persist records for incidents where replay fidelity matters.
