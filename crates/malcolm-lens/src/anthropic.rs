use std::fmt::Write as _;

use rig_core::client::CompletionClient;
use rig_core::completion::Prompt;
use rig_core::providers::anthropic;

use crate::{
    Directive, LensConfig, LensError, LensProvider, LensReport, PromptBuilder, ResponseParser,
};

/// Anthropic-backed lens provider.
pub struct AnthropicLens {
    client: anthropic::Client,
    model: String,
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
impl LensProvider for AnthropicLens {
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

#[cfg(test)]
mod tests {
    use super::AnthropicLens;

    #[test]
    fn missing_api_key_returns_error() {
        let config = crate::LensConfig {
            provider: crate::Provider::Anthropic,
            model: "claude-sonnet-4-20250514".to_owned(),
            base_url: None,
            max_tokens: 1_024,
        };

        let result = AnthropicLens::from_config_with_env(config, |_key| None);
        assert!(matches!(result, Err(crate::LensError::MissingApiKey)));
    }
}
