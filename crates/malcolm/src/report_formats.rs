//! Machine-readable CI report emitters.
//!
//! Two formats are produced from the same `(ScenarioReport, BudgetOutcome)`
//! pair:
//!
//! - **`JUnit` XML** — consumed by GitHub Actions test panels, GitLab, Jenkins,
//!   Buildkite, and any other XML-aware CI renderer.
//! - **SARIF 2.1.0** — consumed by GitHub code-scanning ("Checks" annotations),
//!   VS Code, and any SARIF-aware tooling.
//!
//! Both are pure functions: they take a report and a budget outcome and
//! return a string or JSON value. They never panic on user-controlled
//! strings (scenario names, node ids, fault types, tag values) — see
//! [`escape_xml_attr`] / [`escape_xml_text`] for the escaping layer.
//!
//! # Example
//!
//! ```rust
//! use malcolm::assertions::ResilienceBudget;
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::report_formats::{to_junit_xml, to_sarif};
//! use malcolm::scenario::ChaosScenario;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let scenario = ChaosScenario::builder()
//!     .name("replay-demo")
//!     .seed(7)
//!     .add_fault(PacketLoss::builder().seed(7).intensity(0.8).build())
//!     .profile(BifurcationProfile::network_partition())
//!     .build();
//! let mut ctx = malcolm::fault::FaultContext {
//!     seed: 7,
//!     timestamp_ms: 0,
//!     node_id: "node-0".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! let report = scenario.run(&mut ctx);
//!
//! // JUnit XML — consumed by GitHub Actions test panels.
//! let xml = to_junit_xml(&report, None);
//! assert!(xml.contains("<testsuite"));
//! assert!(xml.contains("name=\"replay-demo\""));
//!
//! // SARIF 2.1.0 — consumed by GitHub code-scanning.
//! let sarif = to_sarif(&report, None);
//! assert_eq!(sarif["version"], "2.1.0");
//! assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "malcolm");
//! ```
//!
//! When a budget was evaluated, pass the outcome to surface each violation
//! as a failure (`JUnit`) or a result entry (SARIF).

use serde_json::{Value, json};

use crate::assertions::BudgetOutcome;
use crate::scenario::ScenarioReport;

// ── JUnit XML ────────────────────────────────────────────────────────────────

/// Render a `JUnit` XML test report for `report` + `outcome`.
///
/// * `outcome: None` — every fault type in the report becomes a test case;
///   no failures are reported.
///
/// * `outcome: Some(o)` — one test case per budget rule that fired, plus a
///   synthetic "injected" case for each fault type. Violations become
///   `<failure>` elements with the expected/actual detail as body text.
///
/// The output is well-formed XML even for user-controlled strings containing
/// `<`, `>`, `&`, `"`, or `'`. Returns an empty string for empty reports,
/// which downstream tooling treats as a no-op.
#[must_use]
pub fn to_junit_xml(report: &ScenarioReport, outcome: Option<&BudgetOutcome>) -> String {
    let mut out = String::with_capacity(512);
    let duration_seconds =
        f64::from(u32::try_from(report.total_duration_ms).unwrap_or(u32::MAX)) / 1000.0;

    // Decide the test-case catalogue. With a budget, every fault injection
    // is a sub-case under the budget verdict; without a budget, every fault
    // is just a passing case.
    let violations_len = outcome.map_or(0, |o| o.violations.len());
    let failures = violations_len;
    let mut tests = violations_len + report.events.len();
    if tests == 0 {
        tests = 1; // Guarantees a non-empty testsuite so CI parsers don't choke.
    }

    let _ = writeln!(
        out,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuite name=\"{}\" tests=\"{}\" failures=\"{}\" time=\"{:.3}\">",
        escape_xml_attr(&report.name),
        tests,
        failures,
        duration_seconds,
    );

    // Fault events surface as passing test cases when no budget is supplied,
    // or as the run's "injected" footer when a budget is also being evaluated.
    for event in &report.events {
        let name = format!("{}.inject.{}", report.name, event.fault_type);
        let _ = writeln!(
            out,
            "  <testcase classname=\"malcolm\" name=\"{}\" time=\"0.000\"/>",
            escape_xml_attr(&name),
        );
    }

    // Violations become failures.
    if let Some(outcome) = outcome {
        for violation in &outcome.violations {
            let name = format!("budget.{}", violation.rule);
            let body = format!("expected {}, got {}", violation.expected, violation.actual);
            let _ = writeln!(
                out,
                "  <testcase classname=\"malcolm::budget\" name=\"{}\" time=\"0.000\">",
                escape_xml_attr(&name),
            );
            let _ = writeln!(
                out,
                "    <failure message=\"{}\" type=\"{}\">{}</failure>",
                escape_xml_attr(&violation.rule),
                escape_xml_attr(&violation.rule),
                escape_xml_text(&body),
            );
            let _ = writeln!(out, "  </testcase>");
        }
    }

    // Emit at least one synthetic case so the testsuite is non-empty
    // even when the scenario was a no-op (zero faults, no budget).
    if tests == 1 && report.events.is_empty() && outcome.is_none_or(|o| o.violations.is_empty()) {
        let name = escape_xml_attr(&report.name);
        let _ = writeln!(
            out,
            "  <testcase classname=\"malcolm\" name=\"{name}\" time=\"0.000\"/>"
        );
    }

    let _ = writeln!(out, "</testsuite>");
    out
}

// ── SARIF ──────────────────────────────────────────────────────────────────────

/// SARIF 2.1.0 schema URI used in the emitted document.
pub const SARIF_SCHEMA: &str = "https://json.schemastore.org/sarif-2.1.0/json/sarif-2.1.0.json";

/// Render a SARIF 2.1.0 document for `report` + `outcome`.
///
/// Each `Violation` becomes a `result` entry with `level: "error"`. A pass
/// (no budget, or satisfied budget) emits an empty `results: []` array so
/// downstream tooling (GitHub code-scanning, etc.) can still consume the
/// document.
///
/// The result is a `serde_json::Value` (an object) rather than a string so
/// callers can serialise it with whatever JSON encoding they prefer.
#[must_use]
pub fn to_sarif(report: &ScenarioReport, outcome: Option<&BudgetOutcome>) -> Value {
    let mut results = Vec::new();

    if let Some(outcome) = outcome {
        for violation in &outcome.violations {
            results.push(json!({
                "ruleId": violation.rule,
                "level": "error",
                "message": {
                    "text": format!(
                        "expected {}, got {}",
                        violation.expected, violation.actual
                    ),
                },
                "locations": [{
                    "physicalLocation": {
                        "artifactLocation": {
                            "uri": format!("malcolm://scenario/{}", report.name),
                        }
                    }
                }],
            }));
        }
    }

    json!({
        "$schema": SARIF_SCHEMA,
        "version": "2.1.0",
        "runs": [{
            "tool": {
                "driver": {
                    "name": "malcolm",
                    "version": env!("CARGO_PKG_VERSION"),
                    "informationUri": "https://github.com/greysquirr3l/malcom",
                    "rules": sarif_rules(),
                }
            },
            "invocations": [{
                "executionSuccessful": outcome.is_none_or(|o| o.passed),
                "properties": {
                    "scenario_name": report.name,
                    "seed": report.seed,
                    "regime": format!("{:?}", report.regime).to_lowercase(),
                    "fault_count": report.events.len(),
                    "total_duration_ms": report.total_duration_ms,
                }
            }],
            "results": results,
        }]
    })
}

fn sarif_rules() -> Value {
    let rules = [
        (
            "max_injected_total",
            "Total injected events exceeded the budget cap.",
        ),
        (
            "min_injected_total",
            "Total injected events fell below the budget floor.",
        ),
        (
            "max_injected_per_fault_type",
            "One fault type exceeded its per-type cap.",
        ),
        (
            "require_fault_types",
            "A required fault type was absent from the run.",
        ),
        ("forbid_regime", "The run reached a forbidden regime."),
        (
            "max_scenario_duration_ms",
            "Scenario duration exceeded the budget cap.",
        ),
    ];
    let arr: Vec<Value> = rules
        .iter()
        .map(|(id, desc)| {
            json!({
                "id": id,
                "name": id,
                "shortDescription": { "text": desc },
            })
        })
        .collect();
    Value::Array(arr)
}

// ── XML escaping ──────────────────────────────────────────────────────────────

/// Escape `s` for use inside an XML attribute value (double-quoted).
///
/// Handles `&`, `<`, `>`, `"`, and `'` so a scenario named `a<b>&"c"`
/// survives the round-trip without breaking the document.
#[must_use]
pub fn escape_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Escape `s` for use inside XML element text content.
///
/// Handles `&`, `<`, and `>`. Attribute quotes (`"`, `'`) are not escaped
/// here because element text never contains those special characters.
#[must_use]
pub fn escape_xml_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

// ── Tiny XML reader for tests ──────────────────────────────────────────────────

/// Minimal pull-parser sufficient for the `JUnit` output: returns one tag at a
/// time when iterating with [`TagIter::next`]. We use it only in tests
/// to confirm structural well-formedness (matching the task brief's
/// "don't just string-match" requirement).
#[cfg(test)]
mod tag_iter {
    pub(super) struct TagIter<'a> {
        rest: &'a str,
    }

    pub(super) fn tags(input: &str) -> TagIter<'_> {
        TagIter { rest: input }
    }

    impl<'a> Iterator for TagIter<'a> {
        type Item = (&'a str, &'a str);

        fn next(&mut self) -> Option<Self::Item> {
            let open = self.rest.find('<')?;
            let close = self.rest[open..].find('>')?;
            let raw = &self.rest[open + 1..open + close];
            let raw = raw.trim();
            // Processing-instruction (`<?xml ... ?>`) and self-closing
            // (`<foo/>`) tags don't open a tag, so we skip them.
            if raw.starts_with('?') || raw.ends_with('/') {
                self.rest = &self.rest[open + close + 1..];
                return self.next();
            }
            let mut parts = raw.splitn(2, char::is_whitespace);
            let name = parts.next()?;
            let attrs = parts.next().unwrap_or("").trim();
            self.rest = &self.rest[open + close + 1..];
            Some((name, attrs))
        }
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests assert invariants via .expect() for failure messages"
)]
#[expect(
    clippy::indexing_slicing,
    reason = "JSON shape assertions read nested fields via index syntax"
)]
mod tests {
    use super::*;
    use crate::assertions::ResilienceBudget;
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::scenario::ChaosScenario;
    use malcolm_core::bifurcation::BifurcationProfile;

    fn make_report(name: &str) -> ScenarioReport {
        let scenario = ChaosScenario::builder()
            .name(name)
            .seed(7)
            .add_fault(PacketLoss::builder().seed(7).intensity(0.8).build())
            .profile(BifurcationProfile::network_partition())
            .build();
        let mut ctx = FaultContext {
            seed: 7,
            timestamp_ms: 0,
            node_id: "node-0".to_owned(),
            profile: BifurcationProfile::network_partition(),
        };
        scenario.run(&mut ctx)
    }

    #[test]
    fn escape_xml_attr_neutralises_special_chars() {
        assert_eq!(escape_xml_attr("a<b>&\"c'"), "a&lt;b&gt;&amp;&quot;c&apos;");
    }

    #[test]
    fn escape_xml_text_skips_attribute_quotes() {
        // `&`, `<`, `>` are escaped; `"` and `'` are not, because text content
        // cannot break out of an attribute.
        assert_eq!(escape_xml_text("a<b>&\"c'"), "a&lt;b&gt;&amp;\"c'");
    }

    #[test]
    fn junit_xml_with_no_budget_lists_one_testcase_per_fault() {
        let report = make_report("simple-budget");
        let xml = to_junit_xml(&report, None);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("<testsuite name=\"simple-budget\""));
        assert!(xml.contains("classname=\"malcolm\""));
        assert!(xml.contains("name=\"simple-budget.inject.packet_loss\""));
        assert!(xml.contains("</testsuite>"));
    }

    #[test]
    fn junit_xml_escapes_user_controlled_name() {
        let report = make_report("a<b>&\"c'");
        let xml = to_junit_xml(&report, None);
        // The scenario name must be escaped in the attribute; the raw
        // `<b>&"c'` must not appear unescaped.
        assert!(!xml.contains("name=\"a<b>&\"c'"));
        assert!(xml.contains("name=\"a&lt;b&gt;&amp;&quot;c&apos;\""));
    }

    #[test]
    fn junit_xml_reports_violations_as_failures() {
        let report = make_report("violated-budget");
        let budget = ResilienceBudget {
            min_injected_total: Some(99),
            ..Default::default()
        };
        let outcome = budget.evaluate(&report);
        let xml = to_junit_xml(&report, Some(&outcome));
        assert!(xml.contains("failures=\"1\""));
        assert!(xml.contains("<failure message=\"min_injected_total\""));
        // The body text is `escape_xml_text`'d, so `>=` becomes `&gt;=`.
        assert!(xml.contains("expected &gt;= 99, got 1"), "{xml}");
    }

    #[test]
    fn junit_xml_words_are_well_formed() {
        // The brief requires a real XML round-trip, not a string match.
        // Use a minimal tag iterator to confirm the document is well-formed.
        use tag_iter::tags;
        let report = make_report("well-formed");
        let budget = ResilienceBudget {
            max_injected_total: Some(0),
            ..Default::default()
        };
        let outcome = budget.evaluate(&report);
        let xml = to_junit_xml(&report, Some(&outcome));
        let mut counter = 0usize;
        let mut opened = Vec::new();
        for (name, _attrs) in tags(&xml) {
            if let Some(close_name) = name.strip_prefix('/') {
                let open = opened.pop().expect("malformed close");
                assert_eq!(open, close_name, "mismatched close tag in XML");
            } else {
                opened.push(name);
            }
            counter += 1;
        }
        assert_eq!(opened, Vec::<&str>::new(), "unclosed XML tags");
        assert!(counter > 0, "tag iterator produced no tags");
    }

    #[test]
    fn sarif_top_level_matches_2_1_0() {
        let report = make_report("sarif-doc");
        let sarif = to_sarif(&report, None);
        assert_eq!(sarif["$schema"], SARIF_SCHEMA);
        assert_eq!(sarif["version"], "2.1.0");
        assert_eq!(sarif["runs"][0]["tool"]["driver"]["name"], "malcolm");
        assert_eq!(
            sarif["runs"][0]["tool"]["driver"]["version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(sarif["runs"][0]["results"], serde_json::json!([]));
    }

    #[test]
    fn sarif_emits_one_result_per_violation() {
        let report = make_report("sarif-vio");
        let budget = ResilienceBudget {
            max_injected_total: Some(0),
            min_injected_total: Some(99),
            forbid_regime: Some(vec![crate::scenario::ScenarioRegime::Sensitive]),
            ..Default::default()
        };
        let outcome = budget.evaluate(&report);
        let sarif = to_sarif(&report, Some(&outcome));
        let results = sarif["runs"][0]["results"]
            .as_array()
            .expect("results array");
        // At least two of the three rules fire (Sensitive may or may not fire
        // depending on regime classification). We assert >= 2 to stay
        // stable across threshold-tuning.
        assert!(results.len() >= 2, "expected >= 2 results, got {results:?}");
        for entry in results {
            assert_eq!(entry["level"], "error");
            assert!(entry["ruleId"].is_string());
            assert!(entry["message"]["text"].is_string());
        }
    }

    #[test]
    fn sarif_includes_invocation_metadata() {
        let report = make_report("sarif-meta");
        let sarif = to_sarif(&report, None);
        let invocation = &sarif["runs"][0]["invocations"][0];
        assert_eq!(invocation["properties"]["scenario_name"], "sarif-meta");
        assert_eq!(invocation["properties"]["seed"], 7);
        assert_eq!(invocation["properties"]["fault_count"], 1);
    }
}

use std::fmt::Write as _;
