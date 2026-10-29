use std::fmt::Write as _;

use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::http_client::HttpClientExt;
use rig_core::providers::anthropic;

use crate::{
    Directive, LensConfig, LensError, LensProvider, LensReport, PromptBuilder, ResponseParser,
    completion,
};

/// Anthropic-backed lens provider.
pub struct AnthropicLens {
    transport: Box<dyn completion::CompletionTransport>,
    max_tokens: u32,
}

impl AnthropicLens {
    /// Build an Anthropic-backed provider from lens configuration.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::MissingApiKey`] when `ANTHROPIC_API_KEY` is missing.
    pub fn from_config(config: LensConfig) -> Result<Self, LensError> {
        Self::from_config_with_env(config, |key| std::env::var(key).ok())
    }

    fn from_config_with_env<F>(config: LensConfig, mut read: F) -> Result<Self, LensError>
    where
        F: FnMut(&str) -> Option<String>,
    {
        let api_key = read("ANTHROPIC_API_KEY").ok_or(LensError::MissingApiKey)?;

        let client = anthropic::Client::builder()
            .api_key(api_key)
            .build()
            .map_err(|error| LensError::ProviderError(error.to_string()))?;

        let LensConfig {
            model, max_tokens, ..
        } = config;
        let transport = AnthropicTransport { client, model };

        Ok(Self::from_transport(Box::new(transport), max_tokens))
    }

    /// Assemble a lens around an already-built transport.
    ///
    /// `max_tokens` is the output budget the transport is asked to enforce;
    /// it is the [`LensConfig::max_tokens`] the caller configured.
    fn from_transport(
        transport: Box<dyn completion::CompletionTransport>,
        max_tokens: u32,
    ) -> Self {
        Self {
            transport,
            max_tokens,
        }
    }

    fn prompt_for(
        report: &malcolm::scenario::ScenarioReport,
        directive: Directive,
        max_tokens: u32,
    ) -> Result<String, LensError> {
        let mut prompt = PromptBuilder::new(directive).build(report)?;
        write!(&mut prompt, "\n\nTOKEN_BUDGET_HINT: {max_tokens}")
            .map_err(|error| LensError::ProviderError(error.to_string()))?;
        Ok(prompt)
    }
}

#[async_trait::async_trait]
impl LensProvider for AnthropicLens {
    async fn analyze_with_directive(
        &self,
        report: &malcolm::scenario::ScenarioReport,
        directive: Directive,
    ) -> Result<LensReport, LensError> {
        let prompt = Self::prompt_for(report, directive, self.max_tokens)?;
        let narrative = self.transport.complete(prompt, self.max_tokens).await?;
        ResponseParser::parse(&narrative, &directive)
    }
}

/// Anthropic implementation of [`completion::CompletionTransport`].
///
/// The HTTP backend is a type parameter so the wire format can be observed
/// against a recording backend in tests.
struct AnthropicTransport<H> {
    client: anthropic::Client<H>,
    model: String,
}

#[async_trait::async_trait]
impl<H> completion::CompletionTransport for AnthropicTransport<H>
where
    H: HttpClientExt + Clone + Default + std::fmt::Debug + Send + Sync + 'static,
{
    async fn complete(&self, prompt: String, max_tokens: u32) -> Result<String, LensError> {
        let model = self.client.completion_model(self.model.clone());
        let request = completion::build_request(&model, prompt, max_tokens);

        let response = model
            .completion(request)
            .await
            .map_err(|error| LensError::ProviderError(error.to_string()))?;

        Ok(completion::extract_text(&response.choice))
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert invariants via .expect() for failure messages"
)]
mod tests {
    use malcolm::scenario::{ScenarioEvent, ScenarioRegime, ScenarioReport};
    use rig_core::providers::anthropic;
    use rig_core::test_utils::RecordingHttpClient;
    use serde_json::Value;

    use super::{AnthropicLens, AnthropicTransport};
    use crate::{Directive, LensConfig, LensProvider, Provider};

    /// Anthropic substitutes 64 000 tokens for `claude-sonnet-4*` models
    /// when a request carries no budget, so that is the number the
    /// regression test asserts is *not* sent.
    const PROVIDER_DEFAULT_MAX_TOKENS: u64 = 64_000;

    const SONNET_MODEL: &str = "claude-sonnet-4-20250514";

    const ANTHROPIC_RESPONSE: &str = r#"{
        "type": "message",
        "content": [{"type": "text", "text": "The scenario stayed below the tipping point."}],
        "id": "msg_test",
        "model": "claude-sonnet-4-20250514",
        "role": "assistant",
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": 12,
            "cache_read_input_tokens": null,
            "cache_creation_input_tokens": null,
            "output_tokens": 7
        }
    }"#;

    fn sample_report() -> ScenarioReport {
        ScenarioReport {
            name: "anthropic-wire".to_owned(),
            seed: 5,
            regime: ScenarioRegime::Sensitive,
            events: vec![ScenarioEvent {
                fault_type: "packet_loss".to_owned(),
                node_id: "node-a".to_owned(),
                seed: 5,
                intensity: 0.7,
                dry_run: false,
                timestamp_ms: 1,
            }],
            total_duration_ms: 10,
        }
    }

    #[test]
    fn missing_api_key_returns_error() {
        let config = LensConfig {
            provider: Provider::Anthropic,
            model: SONNET_MODEL.to_owned(),
            base_url: None,
            max_tokens: 1_024,
        };

        let result = AnthropicLens::from_config_with_env(config, |_key| None);
        assert!(matches!(result, Err(crate::LensError::MissingApiKey)));
    }

    #[test]
    fn configured_max_tokens_reach_the_anthropic_wire() {
        let recorder = RecordingHttpClient::new(ANTHROPIC_RESPONSE);
        let client = anthropic::Client::builder()
            .api_key("test-key")
            .http_client(recorder.clone())
            .build()
            .expect("recording-backed client should build");

        let config = LensConfig {
            provider: Provider::Anthropic,
            model: SONNET_MODEL.to_owned(),
            base_url: None,
            max_tokens: 1_024,
        };
        let LensConfig {
            model, max_tokens, ..
        } = config;
        let transport = AnthropicTransport { client, model };
        let lens = AnthropicLens::from_transport(Box::new(transport), max_tokens);

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("runtime should build");
        runtime
            .block_on(lens.analyze_with_directive(&sample_report(), Directive::Narrative))
            .expect("scripted response should parse");

        let requests = recorder.requests();
        assert_eq!(requests.len(), 1, "expected exactly one provider call");
        let request = requests
            .first()
            .expect("expected exactly one provider call");

        let body: Value = serde_json::from_slice(&request.body).expect("body should be JSON");
        let sent_max_tokens = body.get("max_tokens").and_then(Value::as_u64);
        assert_eq!(
            sent_max_tokens,
            Some(u64::from(max_tokens)),
            "the configured budget must reach the Anthropic request body"
        );
        assert_ne!(
            sent_max_tokens,
            Some(PROVIDER_DEFAULT_MAX_TOKENS),
            "Anthropic must not substitute its own default budget"
        );
    }
}
