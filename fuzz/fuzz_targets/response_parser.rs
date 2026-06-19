#![no_main]

use libfuzzer_sys::fuzz_target;
use malcolm_lens::{Directive, ResponseParser};

fuzz_target!(|data: &[u8]| {
    // Try to interpret the bytes as UTF-8 (the parser operates on &str).
    // If the bytes are not valid UTF-8, skip — that's not the surface we
    // want to exercise. The contract is "panic-free for any valid &str".
    let Ok(text) = core::str::from_utf8(data) else {
        return;
    };
    let directives = [
        Directive::Narrative,
        Directive::AnomalyFlag,
        Directive::SuggestScenarios,
        Directive::ExplainDivergence,
    ];
    for directive in &directives {
        let _ = ResponseParser::parse(text, directive);
    }
});
