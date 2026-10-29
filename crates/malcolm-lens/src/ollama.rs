use std::fmt::Write as _;

use rig_core::client::CompletionClient;
use rig_core::completion::CompletionModel;
use rig_core::http_client::HttpClientExt;
use rig_core::providers::ollama;

use crate::{
    Directive, LensConfig, LensError, LensProvider, LensReport, PromptBuilder, ResponseParser,
    completion,
};

/// Ollama-backed lens provider.
pub struct OllamaLens {
    transport: Box<dyn completion::CompletionTransport>,
    max_tokens: u32,
}

impl OllamaLens {
    /// Build an Ollama-backed provider from lens configuration.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::ProviderError`] if the underlying Rig client cannot be created.
    pub fn from_config(config: LensConfig) -> Result<Self, LensError> {
        let mut builder = ollama::Client::builder().api_key("");
        if let Some(base_url) = config.base_url.as_deref() {
            builder = builder.base_url(base_url);
        }

        let client = builder
            .build()
            .map_err(|error| LensError::ProviderError(error.to_string()))?;

        let LensConfig {
            model, max_tokens, ..
        } = config;
        let transport = OllamaTransport { client, model };

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
impl LensProvider for OllamaLens {
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

/// Ollama implementation of [`completion::CompletionTransport`].
///
/// The HTTP backend is a type parameter so the wire format can be observed
/// against a recording backend in tests.
struct OllamaTransport<H> {
    client: ollama::Client<H>,
    model: String,
}

#[async_trait::async_trait]
impl<H> completion::CompletionTransport for OllamaTransport<H>
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
    use rig_core::providers::ollama;
    use rig_core::test_utils::RecordingHttpClient;
    use serde_json::Value;

    use super::{OllamaLens, OllamaTransport};
    use crate::{Directive, LensConfig, LensProvider, Provider};

    const OLLAMA_MODEL: &str = "llama3.2";

    const OLLAMA_RESPONSE: &str = r#"{
        "model": "llama3.2",
        "created_at": "2024-01-01T00:00:00Z",
        "message": {"role": "assistant", "content": "The scenario stayed below the tipping point."},
        "done": true
    }"#;

    fn sample_report() -> ScenarioReport {
        ScenarioReport {
            name: "ollama-wire".to_owned(),
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
    fn configured_max_tokens_reach_the_ollama_wire() {
        let recorder = RecordingHttpClient::new(OLLAMA_RESPONSE);
        let client = ollama::Client::builder()
            .api_key("")
            .http_client(recorder.clone())
            .build()
            .expect("recording-backed client should build");

        let config = LensConfig {
            provider: Provider::Ollama,
            model: OLLAMA_MODEL.to_owned(),
            base_url: None,
            max_tokens: 1_024,
        };
        let LensConfig {
            model, max_tokens, ..
        } = config;
        let transport = OllamaTransport { client, model };
        let lens = OllamaLens::from_transport(Box::new(transport), max_tokens);

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
        let sent_num_predict = body
            .get("options")
            .and_then(|options| options.get("num_predict"))
            .and_then(Value::as_u64);
        assert_eq!(
            sent_num_predict,
            Some(u64::from(max_tokens)),
            "the configured budget must reach the Ollama request body"
        );
    }
}
