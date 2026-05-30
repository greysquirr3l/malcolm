use serde_json::Value;

use crate::{
    AnomalyFlag, Directive, DivergenceExplanation, LensError, LensReport, NarrativeReport,
    ParseWarning, ScenarioSuggestion,
};

/// Parses raw model responses into structured lens reports.
pub struct ResponseParser;

impl ResponseParser {
    /// Parse one raw model response using the expected directive shape.
    ///
    /// # Errors
    ///
    /// Returns [`LensError::ParseFailure`] when the input is empty or no parse path
    /// can extract structured data from the response.
    pub fn parse(raw: &str, directive: &Directive) -> Result<LensReport, LensError> {
        let directive = *directive;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(LensError::ParseFailure {
                raw: raw.to_owned(),
                reason: "empty response".to_owned(),
            });
        }

        match Self::parse_direct(trimmed, directive) {
            Ok(report) => return Ok(report),
            Err(reason) => Self::warn_parse_failure("primary", trimmed, &reason),
        }

        if let Some(block) = Self::extract_json_code_block(trimmed) {
            match Self::parse_direct(&block, directive) {
                Ok(report) => return Ok(report),
                Err(reason) => Self::warn_parse_failure("fenced", &block, &reason),
            }
        }

        Ok(Self::fallback_narrative(trimmed, directive))
    }

    fn parse_direct(raw: &str, directive: Directive) -> Result<LensReport, String> {
        if let Ok(report) = serde_json::from_str::<LensReport>(raw) {
            return if Self::matches_directive(&report, directive) {
                Ok(report)
            } else {
                Err("report kind did not match requested directive".to_owned())
            };
        }

        let value = serde_json::from_str::<Value>(raw).map_err(|error| error.to_string())?;
        Self::parse_value(value, directive)
    }

    fn parse_value(value: Value, directive: Directive) -> Result<LensReport, String> {
        if let Some(payload) = value.get("payload") {
            return Self::parse_payload(payload.clone(), directive);
        }
        Self::parse_payload(value, directive)
    }

    fn parse_payload(payload: Value, directive: Directive) -> Result<LensReport, String> {
        match directive {
            Directive::Narrative => serde_json::from_value::<NarrativeReport>(payload)
                .map(LensReport::Narrative)
                .map_err(|error| error.to_string()),
            Directive::AnomalyFlag => serde_json::from_value::<Vec<AnomalyFlag>>(payload)
                .map(LensReport::Anomalies)
                .map_err(|error| error.to_string()),
            Directive::SuggestScenarios => {
                serde_json::from_value::<Vec<ScenarioSuggestion>>(payload)
                    .map(LensReport::Suggestions)
                    .map_err(|error| error.to_string())
            }
            Directive::ExplainDivergence => {
                serde_json::from_value::<DivergenceExplanation>(payload)
                    .map(LensReport::Divergence)
                    .map_err(|error| error.to_string())
            }
        }
    }

    const fn matches_directive(report: &LensReport, directive: Directive) -> bool {
        matches!(
            (report, directive),
            (LensReport::Narrative(_), Directive::Narrative)
                | (LensReport::Anomalies(_), Directive::AnomalyFlag)
                | (LensReport::Suggestions(_), Directive::SuggestScenarios)
                | (LensReport::Divergence(_), Directive::ExplainDivergence)
        )
    }

    fn extract_json_code_block(raw: &str) -> Option<String> {
        let mut rest = raw;
        while let Some(start) = rest.find("```") {
            let after_fence = &rest[start + 3..];
            let newline_idx = after_fence.find('\n')?;
            let language = after_fence[..newline_idx].trim();
            let body_start = newline_idx + 1;
            let body = &after_fence[body_start..];
            let end_idx = body.find("```")?;
            let content = body[..end_idx].trim();

            if !content.is_empty() && (language.eq_ignore_ascii_case("json") || language.is_empty())
            {
                return Some(content.to_owned());
            }

            rest = &body[end_idx + 3..];
        }
        None
    }

    fn fallback_narrative(raw: &str, directive: Directive) -> LensReport {
        LensReport::Narrative(NarrativeReport {
            summary: raw.to_owned(),
            key_events: Vec::new(),
            regime_commentary: format!(
                "Parser fallback used for {} directive.",
                directive.as_str()
            ),
            recommended_actions: Vec::new(),
            parse_warning: Some(ParseWarning {
                reason: "failed to parse model output as structured JSON".to_owned(),
            }),
        })
    }

    fn warn_parse_failure(stage: &str, raw: &str, reason: &str) {
        tracing::warn!(
            target: "malcolm_lens::parser",
            stage,
            reason,
            raw_excerpt = %Self::truncate(raw, 200),
            "failed to parse llm response"
        );
    }

    fn truncate(raw: &str, max_chars: usize) -> String {
        let mut truncated = String::new();
        for (idx, ch) in raw.chars().enumerate() {
            if idx >= max_chars {
                truncated.push_str("...");
                break;
            }
            truncated.push(ch);
        }
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::ResponseParser;
    use crate::{Directive, LensError, LensReport, Severity};

    #[test]
    fn well_formed_json_parses_for_each_directive() -> Result<(), Box<dyn std::error::Error>> {
        let narrative = r#"{"kind":"narrative","payload":{"summary":"brief","key_events":["e1"],"regime_commentary":"stable","recommended_actions":["a1"]}}"#;
        let anomalies = r#"{"kind":"anomalies","payload":[{"fault_type":"packet_loss","node_id":"node-a","severity":"high","explanation":"hotspot"}]}"#;
        let suggestions = r#"{"kind":"suggestions","payload":[{"name":"next-run","rationale":"stress retry path","fault_hints":["clock_jump"]}]}"#;
        let divergence = r#"{"kind":"divergence","payload":{"divergence_point":"event 4","likely_cause":"non-deterministic ordering","suggested_fix":"sort node ids"}}"#;

        let narrative_report = ResponseParser::parse(narrative, &Directive::Narrative)?;
        let anomalies_report = ResponseParser::parse(anomalies, &Directive::AnomalyFlag)?;
        let suggestions_report = ResponseParser::parse(suggestions, &Directive::SuggestScenarios)?;
        let divergence_report = ResponseParser::parse(divergence, &Directive::ExplainDivergence)?;

        assert!(matches!(narrative_report, LensReport::Narrative(_)));
        match anomalies_report {
            LensReport::Anomalies(items) => {
                assert_eq!(items.len(), 1);
                let item = items
                    .first()
                    .ok_or("expected one anomaly item in payload")?;
                assert_eq!(item.severity, Severity::High);
            }
            _ => return Err("expected anomalies report".into()),
        }
        assert!(matches!(suggestions_report, LensReport::Suggestions(_)));
        assert!(matches!(divergence_report, LensReport::Divergence(_)));

        Ok(())
    }

    #[test]
    fn fenced_json_block_is_extracted() -> Result<(), Box<dyn std::error::Error>> {
        let raw = r#"Model reasoning\n```json
{"kind":"narrative","payload":{"summary":"from block","key_events":[],"regime_commentary":"ok","recommended_actions":[]}}
```"#;

        let report = ResponseParser::parse(raw, &Directive::Narrative)?;
        match report {
            LensReport::Narrative(payload) => {
                assert_eq!(payload.summary, "from block");
                assert!(payload.parse_warning.is_none());
            }
            _ => return Err("expected narrative report".into()),
        }
        Ok(())
    }

    #[test]
    fn malformed_response_falls_back_to_raw_narrative() -> Result<(), Box<dyn std::error::Error>> {
        let raw = "non-json model output";
        let report = ResponseParser::parse(raw, &Directive::SuggestScenarios)?;

        match report {
            LensReport::Narrative(payload) => {
                assert_eq!(payload.summary, raw);
                assert!(payload.parse_warning.is_some());
            }
            _ => return Err("expected fallback narrative report".into()),
        }

        Ok(())
    }

    #[test]
    fn empty_response_returns_parse_failure() {
        let err = ResponseParser::parse("  ", &Directive::Narrative).err();
        assert!(matches!(err, Some(LensError::ParseFailure { .. })));
    }
}
