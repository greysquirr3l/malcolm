use async_trait::async_trait;
use malcolm::scenario::ScenarioReport;

use crate::{Directive, LensConfig, LensError, LensReport, Provider};

/// Object-safe contract for advisory LLM analysis backends.
///
/// # Example
///
/// ```rust
/// use async_trait::async_trait;
/// use malcolm::scenario::ScenarioReport;
/// use malcolm_lens::{Directive, LensError, LensProvider, LensReport, NarrativeReport};
///
/// struct Stub;
///
/// #[async_trait]
/// impl LensProvider for Stub {
///     async fn analyze_with_directive(
///         &self,
///         _report: &ScenarioReport,
///         _directive: Directive,
///     ) -> Result<LensReport, LensError> {
///         Ok(LensReport::Narrative(NarrativeReport::new(
///             "stub narrative",
///             Vec::new(),
///             "The scenario stayed below the tipping point.",
///             vec!["rerun with higher packet loss".to_owned()],
///         )))
///     }
/// }
/// ```
#[async_trait]
pub trait LensProvider: Send + Sync {
    /// Analyze one scenario report and return an advisory interpretation.
    ///
    /// # Errors
    ///
    /// Returns [`LensError`] when report serialization fails or the selected
    /// provider returns an upstream error.
    async fn analyze(&self, report: &ScenarioReport) -> Result<LensReport, LensError> {
        self.analyze_with_directive(report, Directive::Narrative)
            .await
    }

    /// Analyze one scenario report with a specific prompt directive.
    ///
    /// # Errors
    ///
    /// Returns [`LensError`] when report serialization fails or the selected
    /// provider returns an upstream error.
    async fn analyze_with_directive(
        &self,
        report: &ScenarioReport,
        directive: Directive,
    ) -> Result<LensReport, LensError>;
}

/// Build a boxed provider from runtime configuration.
///
/// # Errors
///
/// Returns [`LensError::FeatureDisabled`] when the requested provider feature is
/// not compiled into this crate.
pub fn provider_from_config(config: LensConfig) -> Result<Box<dyn LensProvider>, LensError> {
    match config.provider {
        Provider::Ollama => {
            #[cfg(feature = "ollama")]
            {
                Ok(Box::new(crate::OllamaLens::from_config(config)?))
            }
            #[cfg(not(feature = "ollama"))]
            {
                Err(LensError::FeatureDisabled("ollama"))
            }
        }
        Provider::Anthropic => {
            #[cfg(feature = "anthropic")]
            {
                Ok(Box::new(crate::AnthropicLens::from_config(config)?))
            }
            #[cfg(not(feature = "anthropic"))]
            {
                Err(LensError::FeatureDisabled("anthropic"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LensProvider;

    fn takes_dyn(_provider: Box<dyn LensProvider>) {}

    #[test]
    fn lens_provider_trait_is_object_safe() {
        struct NullProvider;

        #[async_trait::async_trait]
        impl LensProvider for NullProvider {
            async fn analyze_with_directive(
                &self,
                _report: &malcolm::scenario::ScenarioReport,
                _directive: crate::Directive,
            ) -> Result<crate::LensReport, crate::LensError> {
                Ok(crate::LensReport::Narrative(crate::NarrativeReport::new(
                    "stub",
                    Vec::new(),
                    "No regime change detected.",
                    vec!["none".to_owned()],
                )))
            }
        }

        takes_dyn(Box::new(NullProvider));
    }
}
