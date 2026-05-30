use std::time::{Duration, Instant};

use malcolm::scenario::ScenarioReport;

use crate::{
    Directive, LensConfig, LensError, LensProvider, LensReport, PromptBuilder, Provider,
    provider_from_config,
};

const OLLAMA_TIMEOUT_SECS: u64 = 30;
const ANTHROPIC_TIMEOUT_SECS: u64 = 10;

/// End-to-end advisory analyzer for lens workflows.
///
/// This type is advisory only. It cannot modify fault execution, scenario
/// replay, or any state in the chaos runtime.
///
/// # Example
///
/// ```no_run
/// use malcolm_lens::{LensAnalyzer, LensConfig, Provider};
///
/// let analyzer = LensAnalyzer::builder()
///     .config(LensConfig {
///         provider: Provider::Ollama,
///         model: "llama3.2".to_owned(),
///         base_url: None,
///         max_tokens: 1024,
///     })
///     .build();
///
/// if let Err(error) = analyzer {
///     eprintln!("lens analyzer setup failed: {error}");
/// }
/// ```
pub struct LensAnalyzer {
    provider: Box<dyn LensProvider>,
    config: LensConfig,
    prompt_builder: PromptBuilder,
    timeout: Duration,
}

impl LensAnalyzer {
    /// Create a builder for ergonomic analyzer construction.
    #[must_use]
    pub const fn builder() -> LensAnalyzerBuilder {
        LensAnalyzerBuilder::new()
    }

    /// Analyze a report for one directive.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::Timeout`] when the provider call exceeds the active
    /// timeout or any provider/parser error returned by the lens pipeline.
    pub async fn analyze(
        &self,
        report: &ScenarioReport,
        directive: Directive,
    ) -> Result<LensReport, LensError> {
        let default_directive = self.prompt_builder.directive();
        let span = tracing::info_span!(
            target: "malcolm_lens::analyzer",
            "analyze",
            provider = self.provider_name(),
            model = self.config.model.as_str(),
            directive = directive.as_str(),
            default_directive = default_directive.as_str(),
            duration_ms = tracing::field::Empty,
            parse_ok = tracing::field::Empty,
        );

        let _entered = span.enter();

        let started = Instant::now();
        let result = tokio::time::timeout(
            self.timeout,
            self.provider.analyze_with_directive(report, directive),
        )
        .await;

        let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        span.record("duration_ms", elapsed_ms);

        match result {
            Ok(Ok(lens_report)) => {
                span.record("parse_ok", Self::report_parse_ok(&lens_report));
                Ok(lens_report)
            }
            Ok(Err(error)) => {
                span.record("parse_ok", false);
                Err(error)
            }
            Err(_elapsed) => {
                span.record("parse_ok", false);
                Err(LensError::Timeout)
            }
        }
    }

    /// Run the primary directives in sequence.
    ///
    /// Failure in one directive does not abort the sequence.
    pub async fn analyze_all(&self, report: &ScenarioReport) -> Vec<Result<LensReport, LensError>> {
        let directives = [
            Directive::Narrative,
            Directive::AnomalyFlag,
            Directive::SuggestScenarios,
        ];

        let mut results = Vec::with_capacity(directives.len());
        for directive in directives {
            results.push(self.analyze(report, directive).await);
        }
        results
    }

    const fn provider_name(&self) -> &'static str {
        match self.config.provider {
            Provider::Ollama => "ollama",
            Provider::Anthropic => "anthropic",
        }
    }

    const fn report_parse_ok(report: &LensReport) -> bool {
        match report {
            LensReport::Narrative(narrative) => narrative.parse_warning.is_none(),
            LensReport::Anomalies(_) | LensReport::Suggestions(_) | LensReport::Divergence(_) => {
                true
            }
        }
    }

    const fn default_timeout_for(provider: Provider) -> Duration {
        match provider {
            Provider::Ollama => Duration::from_secs(OLLAMA_TIMEOUT_SECS),
            Provider::Anthropic => Duration::from_secs(ANTHROPIC_TIMEOUT_SECS),
        }
    }
}

/// Builder for [`LensAnalyzer`].
pub struct LensAnalyzerBuilder {
    config: Option<LensConfig>,
    provider: Option<Box<dyn LensProvider>>,
    timeout: Option<Duration>,
}

impl LensAnalyzerBuilder {
    const fn new() -> Self {
        Self {
            config: None,
            provider: None,
            timeout: None,
        }
    }

    /// Set analyzer runtime config.
    #[must_use]
    pub fn config(mut self, config: LensConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Inject a provider implementation.
    #[must_use]
    pub fn provider(mut self, provider: Box<dyn LensProvider>) -> Self {
        self.provider = Some(provider);
        self
    }

    /// Override the provider timeout.
    #[must_use]
    pub const fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Build the analyzer.
    ///
    /// # Errors
    ///
    /// Returns [`LensError`] when config parsing fails or provider construction
    /// fails for the selected backend.
    pub fn build(self) -> Result<LensAnalyzer, LensError> {
        let config = match self.config {
            Some(cfg) => cfg,
            None => LensConfig::from_env()?,
        };

        let provider = match self.provider {
            Some(provider) => provider,
            None => provider_from_config(config.clone())?,
        };

        let timeout = match self.timeout {
            Some(value) => value,
            None => LensAnalyzer::default_timeout_for(config.provider),
        };

        Ok(LensAnalyzer {
            provider,
            config,
            prompt_builder: PromptBuilder::new(Directive::Narrative),
            timeout,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use async_trait::async_trait;
    use malcolm::scenario::{ScenarioEvent, ScenarioRegime, ScenarioReport};

    use super::LensAnalyzer;
    use crate::{Directive, LensError, LensProvider, LensReport, ResponseParser};

    fn sample_report() -> ScenarioReport {
        ScenarioReport {
            name: "analyzer-smoke".to_owned(),
            seed: 17,
            regime: ScenarioRegime::Sensitive,
            events: vec![ScenarioEvent {
                fault_type: "packet_loss".to_owned(),
                node_id: "node-a".to_owned(),
                seed: 17,
                intensity: 0.8,
                dry_run: false,
                timestamp_ms: 77,
            }],
            total_duration_ms: 12,
        }
    }

    struct CannedJsonProvider;

    #[async_trait]
    impl LensProvider for CannedJsonProvider {
        async fn analyze_with_directive(
            &self,
            _report: &ScenarioReport,
            directive: Directive,
        ) -> Result<LensReport, LensError> {
            let raw = match directive {
                Directive::Narrative => {
                    r#"{"kind":"narrative","payload":{"summary":"ok","key_events":[],"regime_commentary":"stable","recommended_actions":[]}}"#
                }
                Directive::AnomalyFlag => {
                    r#"{"kind":"anomalies","payload":[{"fault_type":"packet_loss","node_id":"node-a","severity":"high","explanation":"spike"}]}"#
                }
                Directive::SuggestScenarios => {
                    r#"{"kind":"suggestions","payload":[{"name":"next","rationale":"probe","fault_hints":["clock_jump"]}]}"#
                }
                Directive::ExplainDivergence => {
                    r#"{"kind":"divergence","payload":{"divergence_point":"event 2","likely_cause":"ordering","suggested_fix":"sort"}}"#
                }
            };

            ResponseParser::parse(raw, &directive)
        }
    }

    struct SlowProvider;

    #[async_trait]
    impl LensProvider for SlowProvider {
        async fn analyze_with_directive(
            &self,
            _report: &ScenarioReport,
            _directive: Directive,
        ) -> Result<LensReport, LensError> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            ResponseParser::parse(
                r#"{"kind":"narrative","payload":{"summary":"late","key_events":[],"regime_commentary":"late","recommended_actions":[]}}"#,
                &Directive::Narrative,
            )
        }
    }

    struct FlakyProvider;

    #[async_trait]
    impl LensProvider for FlakyProvider {
        async fn analyze_with_directive(
            &self,
            _report: &ScenarioReport,
            directive: Directive,
        ) -> Result<LensReport, LensError> {
            match directive {
                Directive::AnomalyFlag => {
                    Err(LensError::ProviderError("synthetic failure".to_owned()))
                }
                _ => ResponseParser::parse(
                    r#"{"kind":"narrative","payload":{"summary":"ok","key_events":[],"regime_commentary":"stable","recommended_actions":[]}}"#,
                    &Directive::Narrative,
                ),
            }
        }
    }

    #[test]
    fn pipeline_with_mock_provider_returns_structured_report()
    -> Result<(), Box<dyn std::error::Error>> {
        let analyzer = LensAnalyzer::builder()
            .config(crate::LensConfig {
                provider: crate::Provider::Ollama,
                model: "mock".to_owned(),
                base_url: None,
                max_tokens: 256,
            })
            .provider(Box::new(CannedJsonProvider))
            .build()?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;

        let result = runtime.block_on(analyzer.analyze(&sample_report(), Directive::AnomalyFlag));
        match result? {
            LensReport::Anomalies(flags) => {
                let first = flags
                    .first()
                    .ok_or("expected one anomaly from canned provider")?;
                assert_eq!(first.fault_type, "packet_loss");
            }
            _ => return Err("expected anomalies report".into()),
        }

        Ok(())
    }

    #[test]
    fn timeout_maps_to_timeout_error() -> Result<(), Box<dyn std::error::Error>> {
        let analyzer = LensAnalyzer::builder()
            .config(crate::LensConfig {
                provider: crate::Provider::Ollama,
                model: "mock".to_owned(),
                base_url: None,
                max_tokens: 128,
            })
            .provider(Box::new(SlowProvider))
            .timeout(Duration::from_millis(5))
            .build()?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;

        let result = runtime.block_on(analyzer.analyze(&sample_report(), Directive::Narrative));
        assert!(matches!(result, Err(LensError::Timeout)));
        Ok(())
    }

    #[test]
    fn analyze_all_collects_independent_failures() -> Result<(), Box<dyn std::error::Error>> {
        let analyzer = LensAnalyzer::builder()
            .config(crate::LensConfig {
                provider: crate::Provider::Anthropic,
                model: "mock".to_owned(),
                base_url: None,
                max_tokens: 128,
            })
            .provider(Box::new(FlakyProvider))
            .build()?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;

        let results = runtime.block_on(analyzer.analyze_all(&sample_report()));
        assert_eq!(results.len(), 3);
        assert!(results.first().is_some_and(Result::is_ok));
        assert!(results.get(1).is_some_and(Result::is_err));
        assert!(results.get(2).is_some_and(Result::is_ok));
        Ok(())
    }

    #[cfg(feature = "ollama")]
    #[test]
    #[ignore = "requires running Ollama service with local model"]
    fn ollama_integration_returns_narrative() -> Result<(), Box<dyn std::error::Error>> {
        let analyzer = LensAnalyzer::builder()
            .config(crate::LensConfig {
                provider: crate::Provider::Ollama,
                model: "llama3.2".to_owned(),
                base_url: std::env::var("OLLAMA_BASE_URL").ok(),
                max_tokens: 256,
            })
            .build()?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;

        let result = runtime.block_on(analyzer.analyze(&sample_report(), Directive::Narrative))?;
        assert!(matches!(result, LensReport::Narrative(_)));
        Ok(())
    }
}
