//! Shared rig-core completion plumbing for the lens providers.
//!
//! rig-core 0.41 removed the high-level `Prompt` trait and the `Agent`
//! abstraction in favour of the lower-level [`CompletionModel`] contract. Both
//! lens providers share the same build-request → complete → extract-text
//! pipeline, so it lives here instead of being duplicated per provider.

use rig_core::OneOrMany;
use rig_core::completion::{AssistantContent, CompletionModel, CompletionRequest};

// `completion` is a private module, so the strict alias's
// `redundant_pub_crate` / `unreachable_pub` rule pair has no workable
// visibility: `pub(crate)` trips the first, plain `pub` trips the second.
// `pub` is the semantics we want (crate-internal helpers reachable from
// the `anthropic` and `ollama` providers) and the lint is suppressed
// per item, matching the treatment in `examples/support/ollama_guard.rs`.

/// Build a completion request for `prompt` against `model`.
///
/// The prompt is sent as a single user message. No preamble is set — the
/// system prompt is already embedded in the prompt body by
/// [`crate::PromptBuilder`] — and no output budget is set, so each provider
/// applies its own default exactly as the agent-based path did.
#[expect(
    unreachable_pub,
    reason = "completion module is private; these helpers are crate-internal by design"
)]
pub fn build_request<M>(model: &M, prompt: String) -> CompletionRequest
where
    M: CompletionModel,
{
    model.completion_request(prompt).build()
}

/// Extract the assistant's text from a completion choice.
///
/// Providers may split a single reply across several text blocks; they are
/// concatenated in order with no separator, matching the behaviour of the
/// agent-based path this replaced.
#[expect(
    unreachable_pub,
    reason = "completion module is private; these helpers are crate-internal by design"
)]
pub fn extract_text(choice: &OneOrMany<AssistantContent>) -> String {
    choice
        .iter()
        .filter_map(|content| match content {
            AssistantContent::Text(text) => Some(text.text()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert invariants via .expect() for failure messages"
)]
mod tests {
    use rig_core::OneOrMany;
    use rig_core::completion::AssistantContent;

    use super::extract_text;

    #[test]
    fn concatenates_text_blocks_in_order() {
        let choice = OneOrMany::many([
            AssistantContent::text("first "),
            AssistantContent::text("second"),
        ])
        .expect("non-empty choice should build");

        assert_eq!(extract_text(&choice), "first second");
    }

    #[test]
    fn ignores_non_text_content() {
        let choice = OneOrMany::one(AssistantContent::text("only text"));
        assert_eq!(extract_text(&choice), "only text");
    }
}
