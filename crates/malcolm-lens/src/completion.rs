//! Shared rig-core completion plumbing for the lens providers.
//!
//! rig-core 0.41 removed the high-level `Prompt` trait and the `Agent`
//! abstraction in favour of the lower-level [`CompletionModel`] contract. Both
//! lens providers share the same build-request → complete → extract-text
//! pipeline, so it lives here instead of being duplicated per provider.

use rig_core::OneOrMany;
use rig_core::completion::{AssistantContent, CompletionModel, CompletionRequest};

use crate::LensError;

// `completion` is a private module, so the strict alias's
// `redundant_pub_crate` / `unreachable_pub` rule pair has no workable
// visibility: `pub(crate)` trips the first, plain `pub` trips the second.
// `pub` is the semantics we want (crate-internal helpers reachable from
// the `anthropic` and `ollama` providers) and the lint is suppressed
// per item, matching the treatment in `examples/support/ollama_guard.rs`.

/// Narrow port over the provider call a lens adapter performs.
///
/// The lens needs exactly one thing from a provider: take a prompt plus an
/// output budget and hand back the assistant's text. Each adapter owns its
/// rig-core client behind this port, which keeps the budget on the call
/// site where it can be asserted — an adapter that let the provider
/// substitute its own default would silently ignore the configured
/// [`crate::LensConfig::max_tokens`].
#[expect(
    unreachable_pub,
    reason = "completion module is private; this port is crate-internal by design"
)]
#[async_trait::async_trait]
pub trait CompletionTransport: Send + Sync {
    /// Run one completion against the transport's model.
    ///
    /// `max_tokens` is the caller's output budget and must be the budget
    /// the provider enforces on the wire.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::ProviderError`] when the upstream call fails.
    async fn complete(&self, prompt: String, max_tokens: u32) -> Result<String, LensError>;
}

/// Build a completion request for `prompt` against `model`.
///
/// The prompt is sent as a single user message. No preamble is set — the
/// system prompt is already embedded in the prompt body by
/// [`crate::PromptBuilder`].
///
/// The output budget is set explicitly to `max_tokens`. Without it, each
/// provider substitutes its own default (Anthropic uses 64 000 tokens for
/// `claude-sonnet-4*` and 128 000 for `claude-opus-4-*`), which silently
/// ignores the configured [`crate::LensConfig::max_tokens`]. Setting the
/// field on the request makes the configured budget the one the provider
/// enforces; Anthropic additionally *requires* it and errors out when it
/// is absent.
#[expect(
    unreachable_pub,
    reason = "completion module is private; these helpers are crate-internal by design"
)]
pub fn build_request<M>(model: &M, prompt: String, max_tokens: u32) -> CompletionRequest
where
    M: CompletionModel,
{
    model
        .completion_request(prompt)
        .max_tokens(u64::from(max_tokens))
        .build()
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
