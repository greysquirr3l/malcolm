//! Hawkes process: self-exciting clustered fault arrivals.
//!
//! Models *when* faults arrive as a self-exciting point process:
//! each fault bumps the rate so the next one is more likely. Compare
//! a Poisson baseline (no self-excitation) to a Hawkes process with
//! the same long-run mean rate — the Hawkes should produce visibly
//! more clustering (more / bigger bursts).
//!
//! Run with:
//!
//! ```bash
//! cargo run --example hawkes_arrivals
//! ```

use malcolm_core::hawkes::HawkesProcess;

fn main() {
    println!("hawkes_arrivals: self-exciting clustered fault timing");

    // Poisson baseline: rate μ = 1.0 event per unit time.
    let poisson = match HawkesProcess::new(1.0, 0.0, 1.0) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("construction failed: {e}");
            std::process::exit(1);
        }
    };

    // Hawkes with the same long-run rate:
    //   rate = μ / (1 − n) = 1 → μ = 1, n = 0 → n = α/β = 0
    // So pick a Hawkes with a moderate self-excitation that *still*
    // matches the Poisson's rate when balanced by a lower μ.
    //   μ = 0.5, α = 1.0, β = 2.0 → n = 0.5 → rate = 0.5 / 0.5 = 1.0
    let bursty = match HawkesProcess::new(0.5, 1.0, 2.0) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("construction failed: {e}");
            std::process::exit(1);
        }
    };

    println!("\nParameters:");
    println!("  Poisson:               μ=1.0, α=0.0, β=1.0");
    println!("  Hawkes:                  μ=0.5, α=1.0, β=2.0");
    println!(
        "  Hawkes branching ratio:    n = α/β = {:.2}",
        bursty.branching_ratio()
    );
    println!(
        "  Hawkes long-run rate:      μ/(1−n) = {:.3}",
        bursty.long_run_rate().unwrap_or(f64::NAN)
    );

    let horizon = 1_000.0;
    let samples = 5_000;
    let seed = 7;

    let p_evt = poisson.simulate(horizon, seed, samples);
    let b_evt = bursty.simulate(horizon, seed, samples);

    println!("\nSimulated arrivals on horizon {horizon:.0} (seed={seed}):");
    let p_count = f64::from(u32::try_from(p_evt.len()).unwrap_or(u32::MAX));
    let b_count = f64::from(u32::try_from(b_evt.len()).unwrap_or(u32::MAX));
    println!(
        "  Poisson: {} events, rate = {:.3}",
        p_evt.len(),
        p_count / horizon
    );
    println!(
        "  Hawkes:   {} events, rate = {:.3}",
        b_evt.len(),
        b_count / horizon
    );

    // Coefficient of variation of inter-arrival times: Poisson's CV = 1,
    // Hawkes with self-excitation has CV > 1.
    let cv = |evs: &[f64]| -> f64 {
        if evs.len() < 2 {
            return 0.0;
        }
        let diffs: Vec<f64> = evs
            .windows(2)
            .map(|w| match (w.first().copied(), w.get(1).copied()) {
                (Some(a), Some(b)) => b - a,
                _ => f64::NAN,
            })
            .collect();
        let n = f64::from(u32::try_from(diffs.len()).unwrap_or(u32::MAX));
        let mean = diffs.iter().sum::<f64>() / n;
        if mean <= 0.0 {
            return 0.0;
        }
        let var = diffs.iter().map(|&d| (d - mean) * (d - mean)).sum::<f64>() / n;
        (var / (mean * mean)).sqrt()
    };
    println!("\nCoefficient of variation of inter-arrival times:");
    println!("  Poisson: CV = {:.3} (theoretical: 1.0)", cv(&p_evt));
    println!("  Hawkes:   CV = {:.3} (theoretical: > 1.0)", cv(&b_evt));

    // Show the bursts: find the largest cluster of events within a
    // 1-unit window in each simulation.
    let largest_cluster = |evs: &[f64]| -> usize {
        let mut max_run = 0usize;
        let mut current_run: u64 = 0;
        for w in evs.windows(2) {
            let dt = match (w.first().copied(), w.get(1).copied()) {
                (Some(a), Some(b)) => b - a,
                _ => f64::NAN,
            };
            if dt < 1.0 {
                current_run = current_run.saturating_add(1);
            } else {
                current_run = 0;
            }
            let run_usize = usize::try_from(current_run).unwrap_or(usize::MAX);
            max_run = max_run.max(run_usize);
        }
        max_run
    };
    println!("\nLargest cluster (consecutive events within 1.0 unit):");
    println!("  Poisson: {} events", largest_cluster(&p_evt));
    println!("  Hawkes:   {} events", largest_cluster(&b_evt));
}
