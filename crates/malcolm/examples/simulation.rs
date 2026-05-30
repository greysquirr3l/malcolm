use malcolm::fault::FaultContext;
use malcolm::faults::resource::CpuThrottle;
use malcolm::scenario::ChaosScenario;
use malcolm_core::bifurcation::{BifurcationProfile, Regime, classify};
use malcolm_core::lyapunov::LyapunovScorer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SimState {
    Healthy,
    Sensitive,
    Chaotic,
}

const fn state_from_regime(regime: Regime) -> SimState {
    if matches!(regime, Regime::Stable) {
        SimState::Healthy
    } else if matches!(regime, Regime::Sensitive) {
        SimState::Sensitive
    } else {
        // Future `Regime` variants are treated as chaotic for conservative handling.
        SimState::Chaotic
    }
}

fn main() {
    println!("simulation: building deterministic stress scenario");

    let profile = BifurcationProfile::latency_cascade();
    let scenario = ChaosScenario::builder()
        .name("state-machine-stress")
        .seed(2026)
        .add_fault(
            CpuThrottle::builder()
                .seed(7)
                .fraction(0.8)
                .duration_ms(1)
                .build(),
        )
        .profile(profile)
        .build();

    let mut ctx = FaultContext {
        seed: 2026,
        timestamp_ms: 0,
        node_id: "sim-node-0".to_owned(),
        profile,
    };

    let report = scenario.run(&mut ctx);
    let lyapunov = LyapunovScorer::compute(3.9, 2000);
    assert!(lyapunov > 0.0);

    let classified = classify(0.9, &profile);
    let state = state_from_regime(classified);

    println!(
        "simulation: run={} events={}",
        report.name,
        report.events.len()
    );
    println!("simulation: lyapunov={lyapunov:.4}");
    println!("simulation: classified_regime={classified:?} mapped_state={state:?}");
}
