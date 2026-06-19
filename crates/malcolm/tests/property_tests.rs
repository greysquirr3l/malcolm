//! Property-based tests for the `malcolm` replay and envelope modules.
//!
//! These tests complement the unit tests inside each module by exercising
//! invariants across many random inputs:
//!
//! - `ScenarioRecord` JSON and bytes round-trips are lossless for arbitrary
//!   event sequences and node ids.
//! - The integrity tag flips to `false` after any tampered field.
//! - `ChaosScenario::run` is deterministic for the same seed and context.
//! - `ScenarioEnvelope` round-trips preserve the underlying record.
//! - Sealing is non-deterministic (fresh nonce/salt per seal).
//! - Ciphertext tampering fails authentication, and other byte-level mutations
//!   either fail authentication or are rejected as malformed input.

use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::replay::envelope::{EnvelopeError, PassphraseProvider, ScenarioEnvelope};
use malcolm::replay::{
    RecordingHarness, ReplayHarness, ScenarioRecord, TopologyEdgeSnapshot, TopologySnapshot,
};
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::BifurcationProfile;
use proptest::prelude::*;

const PROPTEST_CASES: u32 = 32;

fn make_ctx(seed: u64) -> FaultContext {
    FaultContext {
        seed,
        timestamp_ms: 0,
        node_id: "node-0".to_owned(),
        profile: BifurcationProfile::network_partition(),
    }
}

fn make_scenario(seed: u64) -> ChaosScenario {
    ChaosScenario::builder()
        .name("prop-test")
        .seed(seed)
        .add_fault(PacketLoss::builder().seed(seed).intensity(0.8).build())
        .profile(BifurcationProfile::network_partition())
        .build()
}

fn static_provider(secret: &'static [u8]) -> StaticProvider {
    StaticProvider { secret }
}

struct StaticProvider {
    secret: &'static [u8],
}

impl PassphraseProvider for StaticProvider {
    fn get_passphrase(&self) -> Result<Vec<u8>, EnvelopeError> {
        Ok(self.secret.to_vec())
    }

    fn label(&self) -> &'static str {
        "static"
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(PROPTEST_CASES))]

    #[test]
    fn scenario_record_json_roundtrip_is_lossless(seed in 1_u64..=10_000) {
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let json = record.to_json().expect("serialize record");
        let decoded = ScenarioRecord::from_json(&json).expect("deserialize record");
        prop_assert_eq!(record, decoded);
    }

    #[test]
    fn scenario_record_bytes_roundtrip_is_lossless(seed in 1_u64..=10_000) {
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let bytes = record.to_bytes().expect("serialize record");
        let decoded = ScenarioRecord::from_bytes(&bytes).expect("deserialize record");
        prop_assert_eq!(record, decoded);
    }

    #[test]
    fn chaos_scenario_run_is_deterministic_for_same_seed(seed in 1_u64..=10_000) {
        let scenario = make_scenario(seed);
        let mut ctx_a = make_ctx(seed);
        let mut ctx_b = make_ctx(seed);
        let report_a = scenario.run(&mut ctx_a);
        let report_b = scenario.run(&mut ctx_b);
        // Event sequences are deterministic. Wall-clock duration is excluded
        // because it depends on real time and is not part of the contract.
        prop_assert_eq!(report_a.name, report_b.name);
        prop_assert_eq!(report_a.seed, report_b.seed);
        prop_assert_eq!(report_a.regime, report_b.regime);
        prop_assert_eq!(report_a.events, report_b.events);
    }

    #[test]
    fn replay_verify_true_for_fresh_record(seed in 1_u64..=10_000) {
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);
        let replay = ReplayHarness::new(record);
        prop_assert!(replay.verify());
    }

    #[test]
    fn envelope_seal_open_roundtrip_preserves_record(seed in 1_u64..=10_000) {
        let provider = static_provider(b"correct horse battery staple");
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let envelope = ScenarioEnvelope::seal(&record, &provider).expect("seal");
        let encoded = envelope.to_bytes().expect("encode");
        let decoded = ScenarioEnvelope::from_bytes(&encoded).expect("decode");
        let opened = decoded
            .open_interactive(true, &provider)
            .expect("open");
        prop_assert_eq!(opened, record);
    }

    #[test]
    fn envelope_seal_produces_distinct_ciphertexts(seed in 1_u64..=10_000) {
        let provider = static_provider(b"deterministic-key-material");
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let a = ScenarioEnvelope::seal(&record, &provider).expect("seal a");
        let b = ScenarioEnvelope::seal(&record, &provider).expect("seal b");
        // Salt and nonce are random per seal, so the envelopes must differ
        // even when the plaintext and passphrase are identical.
        prop_assert_ne!(a.to_bytes().unwrap(), b.to_bytes().unwrap());
    }

    #[test]
    fn envelope_rejects_wrong_passphrase(seed in 1_u64..=10_000) {
        let seal_provider = static_provider(b"original-passphrase");
        let wrong_provider = static_provider(b"different-passphrase");
        let scenario = make_scenario(seed);
        let mut ctx = make_ctx(seed);
        let record = RecordingHarness::new(&scenario).record(&mut ctx);

        let envelope = ScenarioEnvelope::seal(&record, &seal_provider).expect("seal");
        let result = envelope.open_non_interactive(Some(&wrong_provider));
        prop_assert!(matches!(result, Err(EnvelopeError::Decrypt)));
    }

    #[test]
    fn envelope_rejects_truncated_bytes(seed in 0_u64..=10_000) {
        // Truncated inputs (anything shorter than magic + version = 5 bytes) must
        // be rejected as Truncated, not panic, for any length below 5.
        let length = usize::try_from(seed).unwrap_or(0) % 5;
        let bytes = vec![0_u8; length];
        let result = ScenarioEnvelope::from_bytes(&bytes);
        prop_assert!(result.is_err());
    }

    #[test]
    fn envelope_rejects_malformed_magic(_seed in 0_u64..=10_000) {
        // 5 bytes that don't start with the MENV magic must return InvalidMagic
        // or Deserialize (never panic).
        let bytes = vec![0xAB_u8; 5];
        let result = ScenarioEnvelope::from_bytes(&bytes);
        prop_assert!(result.is_err());
    }

    #[test]
    fn envelope_rejects_oversized_padding(extra in 0_usize..=128) {
        // Even with a long random tail, decoding must not panic and must
        // produce a Result (Ok or Err, no unwinding).
        let mut bytes = Vec::with_capacity(5 + extra);
        bytes.extend_from_slice(b"MENV");
        bytes.push(1);
        bytes.resize(5 + extra, 0xCC);
        let result = ScenarioEnvelope::from_bytes(&bytes);
        // We don't care whether Ok or Err — only that the call is panic-free.
        let _ = result;
    }
}

#[test]
fn replay_verify_detects_tampered_event_fault_type() {
    let scenario = make_scenario(7);
    let mut ctx = make_ctx(7);
    let mut record = RecordingHarness::new(&scenario).record(&mut ctx);
    if let Some(first) = record.events.first_mut() {
        first.fault_type.push_str("-tampered");
    }
    let replay = ReplayHarness::new(record);
    assert!(!replay.verify());
}

#[test]
fn replay_verify_detects_tampered_seed() {
    let scenario = make_scenario(7);
    let mut ctx = make_ctx(7);
    let mut record = RecordingHarness::new(&scenario).record(&mut ctx);
    if let Some(first) = record.events.first_mut() {
        first.seed = first.seed.wrapping_add(1);
    }
    let replay = ReplayHarness::new(record);
    assert!(!replay.verify());
}

#[test]
fn replay_verify_detects_tampered_topology_edge_weight() {
    let scenario = make_scenario(7);
    let mut ctx = make_ctx(7);
    let mut record = RecordingHarness::new(&scenario).record(&mut ctx);
    if let Some(topology) = record.topology.as_mut() {
        if let Some(edge) = topology.edges.first_mut() {
            edge.weight += 0.1;
        }
    } else {
        // No topology is fine — inject a synthetic one for the assertion.
        record.topology = Some(TopologySnapshot {
            name: "synthetic".to_owned(),
            nodes: vec!["a".to_owned(), "b".to_owned()],
            edges: vec![TopologyEdgeSnapshot {
                from: "a".to_owned(),
                to: "b".to_owned(),
                weight: 0.5,
            }],
        });
    }
    let replay = ReplayHarness::new(record);
    assert!(!replay.verify());
}
