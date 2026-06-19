use std::fmt::Write as _;

use rig_core::client::CompletionClient;
use rig_core::completion::Prompt;
use rig_core::providers::ollama;

use crate::{
    Directive, LensConfig, LensError, LensProvider, LensReport, PromptBuilder, ResponseParser,
};

/// Ollama-backed lens provider.
pub struct OllamaLens {
    client: ollama::Client,
    model: String,
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

        Ok(Self {
            client,
            model: config.model,
            max_tokens: config.max_tokens,
        })
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
        let agent = self.client.agent(self.model.clone()).build();

        let narrative = agent
            .prompt(prompt)
            .await
            .map_err(|error| LensError::ProviderError(error.to_string()))?;

        ResponseParser::parse(&narrative, &directive)
    }
}
