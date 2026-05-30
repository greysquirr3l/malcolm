use malcolm::scenario::ScenarioReport;

/// Analysis mode requested from an LLM provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directive {
    /// Ask for a post-mortem narrative.
    Narrative,
    /// Ask for anomaly flags.
    AnomalyFlag,
    /// Ask for follow-up scenario ideas.
    SuggestScenarios,
    /// Ask why replay diverged.
    ExplainDivergence,
}

impl Directive {
    /// Stable label used in prompts and telemetry.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Narrative => "narrative",
            Self::AnomalyFlag => "anomaly_flag",
            Self::SuggestScenarios => "suggest_scenarios",
            Self::ExplainDivergence => "explain_divergence",
        }
    }

    const fn response_shape(self) -> &'static str {
        match self {
            Self::Narrative => {
                "Return JSON only as {\"kind\":\"narrative\",\"payload\":{\"summary\":string,\"key_events\":string[],\"regime_commentary\":string,\"recommended_actions\":string[]}}."
            }
            Self::AnomalyFlag => {
                "Return JSON only as {\"kind\":\"anomalies\",\"payload\":[{\"fault_type\":string,\"node_id\":string,\"severity\":\"low|medium|high|critical\",\"explanation\":string}]}."
            }
            Self::SuggestScenarios => {
                "Return JSON only as {\"kind\":\"suggestions\",\"payload\":[{\"name\":string,\"rationale\":string,\"fault_hints\":string[]}]}."
            }
            Self::ExplainDivergence => {
                "Return JSON only as {\"kind\":\"divergence\",\"payload\":{\"divergence_point\":string,\"likely_cause\":string,\"suggested_fix\":string}}."
            }
        }
    }

    const fn task_suffix(self) -> &'static str {
        match self {
            Self::Narrative => {
                "Focus on the failure story, the turning point, and concrete operator follow-up."
            }
            Self::AnomalyFlag => {
                "Flag the faults or nodes that look abnormal and rank them by severity."
            }
            Self::SuggestScenarios => {
                "Suggest follow-up chaos scenarios that would sharpen the diagnosis without auto-executing anything."
            }
            Self::ExplainDivergence => {
                "Explain where replay diverged, why it likely happened, and what to inspect next."
            }
        }
    }
}

/// Builds structured prompts for Malcolm Lens analysis.
pub struct PromptBuilder {
    directive: Directive,
}

impl PromptBuilder {
    /// Dr. Malcolm-flavored system prompt baked into the binary.
    pub const SYSTEM_PROMPT: &str = concat!(
        "You are Malcolm Lens, a chaos analyst who reads deterministic fault reports. ",
        "Stay skeptical, stay concise, and speak in operational terms. ",
        "Use only the supplied report. Do not invent missing facts."
    );

    /// Create a builder for one directive.
    #[must_use]
    pub const fn new(directive: Directive) -> Self {
        Self { directive }
    }

    /// Access the baked-in system prompt.
    #[must_use]
    pub const fn system_prompt() -> &'static str {
        Self::SYSTEM_PROMPT
    }

    /// Directive used by this builder.
    #[must_use]
    pub const fn directive(&self) -> Directive {
        self.directive
    }

    /// Serialize a scenario report into the provider prompt.
    ///
    /// # Errors
    ///
    /// Returns [`serde_json::Error`] when the scenario report cannot be serialized.
    pub fn build(&self, report: &ScenarioReport) -> Result<String, serde_json::Error> {
        let report_json = serde_json::to_string_pretty(report)?;
        Ok(format!(
            "SYSTEM:\n{}\n\nSCENARIO_REPORT_JSON:\n{}\n\nTASK [{}]:\n{}\n{}",
            Self::system_prompt(),
            report_json,
            self.directive.as_str(),
            self.directive.task_suffix(),
            self.directive.response_shape(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use malcolm::scenario::{ScenarioEvent, ScenarioRegime, ScenarioReport};

    use super::{Directive, PromptBuilder};

    fn sample_report() -> ScenarioReport {
        ScenarioReport {
            name: "leader-failure".to_owned(),
            seed: 99,
            regime: ScenarioRegime::Sensitive,
            events: vec![ScenarioEvent {
                fault_type: "packet_loss".to_owned(),
                node_id: "node-a".to_owned(),
                seed: 99,
                intensity: 0.72,
                dry_run: false,
                timestamp_ms: 1_234,
            }],
            total_duration_ms: 42,
        }
    }

    #[test]
    fn prompt_contains_json_block_and_narrative_suffix() -> Result<(), serde_json::Error> {
        let prompt = PromptBuilder::new(Directive::Narrative).build(&sample_report())?;

        assert!(prompt.contains(PromptBuilder::system_prompt()));
        assert!(prompt.contains("SCENARIO_REPORT_JSON:"));
        assert!(prompt.contains("\"name\": \"leader-failure\""));
        assert!(prompt.contains("TASK [narrative]:"));
        assert!(prompt.contains("failure story"));
        assert!(prompt.contains("\"kind\":\"narrative\""));
        Ok(())
    }

    #[test]
    fn prompts_differ_between_narrative_and_suggestions() -> Result<(), serde_json::Error> {
        let report = sample_report();
        let narrative_prompt = PromptBuilder::new(Directive::Narrative).build(&report)?;
        let suggestions_prompt = PromptBuilder::new(Directive::SuggestScenarios).build(&report)?;

        assert_ne!(narrative_prompt, suggestions_prompt);
        assert!(suggestions_prompt.contains("TASK [suggest_scenarios]:"));
        assert!(suggestions_prompt.contains("follow-up chaos scenarios"));
        assert!(suggestions_prompt.contains("\"kind\":\"suggestions\""));
        Ok(())
    }
}
