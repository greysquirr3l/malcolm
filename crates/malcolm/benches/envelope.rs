//! Benchmarks for the `ScenarioEnvelope` seal and open paths.
//!
//! Run with `cargo bench -p malcolm --bench envelope`.
#![allow(missing_docs)]
// Criterion's `b.iter(|| ...)` closure is required to return the unit
// type, so wrapping the closure body in a `try`/`?` block isn't an option.
// Use `#[expect(...)]` (rather than `#[allow(...)]`) so the suppressed
// lints remain visible if Criterion ever relaxes the closure contract.
#![expect(
    clippy::expect_used,
    reason = "Criterion `b.iter(|| ...)` closures must return the unit type, so `?` propagation is not available."
)]

use criterion::{Criterion, criterion_group, criterion_main};
use malcolm::fault::FaultContext;
use malcolm::faults::network::PacketLoss;
use malcolm::replay::RecordingHarness;
use malcolm::replay::envelope::{EnvelopeError, PassphraseProvider, ScenarioEnvelope};
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

fn build_record() -> malcolm::replay::ScenarioRecord {
    let scenario = ChaosScenario::builder()
        .name("bench-envelope")
        .seed(73)
        .add_fault(PacketLoss::builder().seed(9).intensity(0.9).build())
        .profile(BifurcationProfile::network_partition())
        .build();
    let mut ctx = FaultContext {
        seed: 73,
        timestamp_ms: 0,
        node_id: "node-0".to_owned(),
        profile: BifurcationProfile::network_partition(),
    };
    RecordingHarness::new(&scenario).record(&mut ctx)
}

fn bench_seal(c: &mut Criterion) {
    let provider = StaticProvider {
        secret: b"bench-passphrase-material".to_vec(),
    };
    let record = build_record();
    c.bench_function("envelope_seal", |b| {
        b.iter(|| ScenarioEnvelope::seal(&record, &provider).expect("seal"));
    });
}

fn bench_open(c: &mut Criterion) {
    let provider = StaticProvider {
        secret: b"bench-passphrase-material".to_vec(),
    };
    let record = build_record();
    let envelope = ScenarioEnvelope::seal(&record, &provider).expect("seal");
    c.bench_function("envelope_open_interactive", |b| {
        b.iter(|| envelope.open_interactive(true, &provider).expect("open"));
    });
}

fn bench_round_trip(c: &mut Criterion) {
    let provider = StaticProvider {
        secret: b"bench-passphrase-material".to_vec(),
    };
    let record = build_record();
    c.bench_function("envelope_seal_encode_decode", |b| {
        b.iter(|| {
            let envelope = ScenarioEnvelope::seal(&record, &provider).expect("seal");
            let encoded = envelope.to_bytes().expect("encode");
            let _ = ScenarioEnvelope::from_bytes(&encoded).expect("decode");
        });
    });
}

criterion_group!(benches, bench_seal, bench_open, bench_round_trip);
criterion_main!(benches);
