#![no_main]

use libfuzzer_sys::fuzz_target;
use malcolm::replay::envelope::ScenarioEnvelope;

fuzz_target!(|data: &[u8]| {
    // `from_bytes` is the only entry point that takes untrusted bytes. The
    // contract is "panic-free for any input" and "returns Err for malformed
    // data". We don't care about Ok vs Err, only that the call is safe.
    let _ = ScenarioEnvelope::from_bytes(data);
});
