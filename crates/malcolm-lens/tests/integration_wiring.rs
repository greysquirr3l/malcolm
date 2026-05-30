use async_trait::async_trait;
use malcolm::scenario::{ScenarioEvent, ScenarioRegime, ScenarioReport};
use malcolm_lens::{
    Directive, LensAnalyzer, LensConfig, LensError, LensProvider, LensReport, Provider,
    ResponseParser,
};

struct WiringProvider;

#[async_trait]
impl LensProvider for WiringProvider {
    async fn analyze_with_directive(
        &self,
        _report: &ScenarioReport,
        directive: Directive,
    ) -> Result<LensReport, LensError> {
        let raw = match directive {
            Directive::Narrative => {
                r#"{"kind":"narrative","payload":{"summary":"narrative path","key_events":[],"regime_commentary":"stable","recommended_actions":[]}}"#
            }
            Directive::AnomalyFlag => {
                r#"{"kind":"anomalies","payload":[{"fault_type":"packet_loss","node_id":"node-a","severity":"low","explanation":"minor"}]}"#
            }
            Directive::SuggestScenarios => {
                r#"{"kind":"suggestions","payload":[{"name":"retry-jitter","rationale":"reduce herd","fault_hints":["latency_spike"]}]}"#
            }
            Directive::ExplainDivergence => {
                r#"{"kind":"divergence","payload":{"divergence_point":"event 3","likely_cause":"ordering drift","suggested_fix":"stabilize ordering"}}"#
            }
        };

        ResponseParser::parse(raw, &directive)
    }
}

struct FailingAnomalyProvider;

#[async_trait]
impl LensProvider for FailingAnomalyProvider {
    async fn analyze_with_directive(
        &self,
        _report: &ScenarioReport,
        directive: Directive,
    ) -> Result<LensReport, LensError> {
        if directive == Directive::AnomalyFlag {
            return Err(LensError::ProviderError("injected failure".to_owned()));
        }

        ResponseParser::parse(
            r#"{"kind":"narrative","payload":{"summary":"ok","key_events":[],"regime_commentary":"stable","recommended_actions":[]}}"#,
            &Directive::Narrative,
        )
    }
}

fn sample_report() -> ScenarioReport {
    ScenarioReport {
        name: "integration-wiring".to_owned(),
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
fn end_to_end_public_api_path_returns_structured_report() -> Result<(), Box<dyn std::error::Error>>
{
    let analyzer = LensAnalyzer::builder()
        .config(LensConfig {
            provider: Provider::Ollama,
            model: "wiring-mock".to_owned(),
            base_url: None,
            max_tokens: 128,
        })
        .provider(Box::new(WiringProvider))
        .build()?;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    let result =
        runtime.block_on(analyzer.analyze(&sample_report(), Directive::SuggestScenarios))?;

    match result {
        LensReport::Suggestions(suggestions) => {
            assert_eq!(suggestions.len(), 1);
            let first = suggestions
                .first()
                .ok_or("expected one suggestion from wiring provider")?;
            assert_eq!(first.name, "retry-jitter");
        }
        _ => return Err("expected suggestions report".into()),
    }

    Ok(())
}

#[test]
fn analyze_all_keeps_sequence_when_one_directive_fails() -> Result<(), Box<dyn std::error::Error>> {
    let analyzer = LensAnalyzer::builder()
        .config(LensConfig {
            provider: Provider::Anthropic,
            model: "wiring-failure".to_owned(),
            base_url: None,
            max_tokens: 128,
        })
        .provider(Box::new(FailingAnomalyProvider))
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
