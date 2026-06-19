#![no_main]

use libfuzzer_sys::fuzz_target;
use malcolm_core::bifurcation::{BifurcationProfile, classify};

fuzz_target!(|data: &[u8]| {
    // Convert up to 16 bytes into two f64s for threshold and intensity. The
    // function is total, so the property we care about is "never panics".
    if data.len() < 16 {
        return;
    }
    let threshold = f64::from_le_bytes(data[..8].try_into().unwrap());
    let intensity = f64::from_le_bytes(data[8..16].try_into().unwrap());
    let window = 0.2_f64;
    let profile = BifurcationProfile {
        threshold: if threshold.is_finite() { threshold } else { 0.5 },
        sensitivity_window: window,
        label: "fuzz",
    };
    let _ = classify(intensity, &profile);
});
