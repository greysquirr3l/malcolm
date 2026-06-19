//! End-to-end integration test exercising the full public API surface.
//!
//! This test stitches the per-layer contract tests together: it builds a
//! scenario from the public `presets` API, runs it through the public
//! `replay` recording harness, wraps the record in a public
//! `ScenarioEnvelope`, persists the envelope as bytes (the same format the
//! `malcolm-run` CLI writes), reloads it, and opens the envelope with a
//! `StaticPassphraseProvider`. Each step uses only the public API — no
//! test-only helpers.
//!
//! The test acts as a regression guard for cross-module wiring regressions:
//! if any of these layers drift apart (e.g. a new `ScenarioRecord` field
//! without an envelope update), this test fails before any consumer notices.

use malcolm::fault::FaultContext;
use malcolm::presets;
use malcolm::replay::envelope::{EnvelopeError, PassphraseProvider, ScenarioEnvelope};
use malcolm::replay::{RecordingHarness, ReplayHarness, ScenarioRecord};
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;

struct StaticProvider {
    secret: Vec<u8>,
}

impl PassphraseProvider for StaticProvider {
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
        Ok(self.secret.clone())
    }

    fn label(&self) -> &'static str {
        "static"
    }
}

#[test]
fn end_to_end_chaos_replay_envelope_open() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Build a scenario from the public presets API.
    let scenario: ChaosScenario = presets::flaky_net()
        .seed(1337)
        .profile(BifurcationProfile::network_partition())
        .build();

    let mut ctx = FaultContext {
        seed: 1337,
        timestamp_ms: 0,
        node_id: "edge-0".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };

    // 2. Run the scenario and record it via the public replay harness.
    let report = scenario.run(&mut ctx);
    assert!(
        !report.events.is_empty(),
        "scenario should inject at least one event"
    );

    let record = RecordingHarness::new(&scenario).record(&mut ctx);

    // 3. Replay the record through the public ReplayHarness and confirm
    //    that the replayed event sequence matches the original.
    let replay = ReplayHarness::new(record.clone());
    assert!(replay.verify());
    let replayed_report = replay.replay();
    assert_eq!(replayed_report.events, report.events);
    assert_eq!(replayed_report.name, report.name);

    // 4. Round-trip the record through YAML (the CLI's preferred format).
    let yaml = record.to_yaml()?;
    let from_yaml = ScenarioRecord::from_yaml(&yaml)?;
    assert_eq!(from_yaml, record);
    assert!(from_yaml.verify());

    // 5. Seal the record into a sealed envelope, write it to bytes, reload.
    let provider = StaticProvider {
        secret: b"end-to-end-passphrase".to_vec(),
    };
    let envelope = ScenarioEnvelope::seal(&record, &provider)?;
    let encoded = envelope.to_bytes()?;
    let decoded = ScenarioEnvelope::from_bytes(&encoded)?;

    // 6. Open the envelope and confirm we get back the same record.
    let opened = decoded.open_interactive(true, &provider)?;
    assert_eq!(opened, record);
    assert!(opened.verify());

    // 7. Wrong-passphrase rejection at the AEAD layer.
    let wrong = StaticProvider {
        secret: b"different-passphrase".to_vec(),
    };
    let bad = decoded.open_non_interactive(Some(&wrong));
    assert!(bad.is_err(), "wrong passphrase must fail authentication");

    // 8. Replay the opened record through ReplayHarness one more time as a
    //    final round-trip integrity check.
    let final_replay = ReplayHarness::new(opened);
    assert!(final_replay.verify());

    Ok(())
}

#[test]
fn end_to_end_scenario_record_roundtrips_through_all_formats()
-> Result<(), Box<dyn std::error::Error>> {
    let scenario = presets::memory_pressure()
        .seed(99)
        .profile(BifurcationProfile::memory_pressure())
        .build();
    let mut ctx = FaultContext {
        seed: 99,
        timestamp_ms: 0,
        node_id: "db-0".to_owned(),
        profile: BifurcationProfile::memory_pressure(),
    };
    let record = RecordingHarness::new(&scenario).record(&mut ctx);

    // JSON round-trip.
    let json = record.to_json()?;
    assert_eq!(ScenarioRecord::from_json(&json)?, record);

    // Bytes round-trip.
    let bytes = record.to_bytes()?;
    assert_eq!(ScenarioRecord::from_bytes(&bytes)?, record);

    // YAML round-trip.
    let yaml = record.to_yaml()?;
    assert_eq!(ScenarioRecord::from_yaml(&yaml)?, record);

    Ok(())
}
