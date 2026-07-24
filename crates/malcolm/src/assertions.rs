//! Resilience-budget assertion engine for chaos runs.
//!
//! A [`ResilienceBudget`] is a collection of pass/fail thresholds evaluated
//! against a [`ScenarioReport`](crate::scenario::ScenarioReport). The
//! assertion engine is **panic-free** and **accumulates** every violation
//! rather than short-circuiting on the first failure so operators see every
//! breach in a single run.
//!
//! # Why a budget?
//!
//! Chaos experiments need a pass/fail signal that can flip a CI pipeline red.
//! The `malcolm-run` binary consumes a budget file (or inline flags) and exits
//! with a dedicated code (`3`) on policy violation, distinguishing a budget
//! failure from a crash or an argument error.
//!
//! # Example
//!
//! ```rust
//! use malcolm::assertions::ResilienceBudget;
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::scenario::ChaosScenario;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let budget = ResilienceBudget {
//!     min_injected_total: Some(1),
//!     max_injected_total: Some(100),
//!     ..Default::default()
//! };
//!
//! let scenario = ChaosScenario::builder()
//!     .name("budget-demo")
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
//! let outcome = budget.evaluate(&report);
//! assert!(outcome.passed, "policy violations: {:?}", outcome.violations);
//! ```

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::scenario::{ScenarioRegime, ScenarioReport};

/// All-optional resilience thresholds for one chaos run.
///
/// A `None` value means "not asserted". An empty budget is a tautology: it
/// accepts every report as passing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResilienceBudget {
    /// Upper bound on total injected-fault events; reported value must be `<=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_injected_total: Option<u64>,
    /// Lower bound on total injected-fault events; reported value must be `>=`.
    /// Catches a misconfigured scenario that silently injects nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_injected_total: Option<u64>,
    /// Per-fault-type upper bounds; reported value must be `<=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_injected_per_fault_type: Option<BTreeMap<String, u64>>,
    /// Each named fault type must appear at least once in the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_fault_types: Option<Vec<String>>,
    /// Fail if the run reached any of these regimes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forbid_regime: Option<Vec<ScenarioRegime>>,
    /// Upper bound on `total_duration_ms`; reported value must be `<=`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_scenario_duration_ms: Option<u64>,
}

/// Builder-style helpers for inline CLI merging.
impl ResilienceBudget {
    /// Override `min_injected_total`.
    #[must_use]
    pub const fn with_min_injected(mut self, n: u64) -> Self {
        self.min_injected_total = Some(n);
        self
    }

    /// Override `max_injected_total`.
    #[must_use]
    pub const fn with_max_injected(mut self, n: u64) -> Self {
        self.max_injected_total = Some(n);
        self
    }
}

/// One budget violation. Serializable so callers can embed it in JSON reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Violation {
    /// Stable identifier of the rule that fired (e.g. `"max_injected_total"`).
    pub rule: String,
    /// Human-readable expected value (e.g. `"<= 100"`).
    pub expected: String,
    /// Human-readable actual value (e.g. `"143"`).
    pub actual: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}] expected {}, got {}",
            self.rule, self.expected, self.actual
        )
    }
}

/// Aggregate result of [`ResilienceBudget::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetOutcome {
    /// `true` when no violation was produced.
    pub passed: bool,
    /// All violations observed, in evaluation order. Empty when `passed`.
    pub violations: Vec<Violation>,
}

impl BudgetOutcome {
    /// Number of violations recorded.
    #[must_use]
    pub fn violation_count(&self) -> usize {
        self.violations.len()
    }
}

/// Errors produced by [`ResilienceBudget::from_file`].
#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    /// I/O error reading the budget file.
    #[error("could not read budget file {path}: {source}")]
    Io {
        /// Path the binary was asked to read.
        path: String,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// File extension is not one of `.toml`, `.json`, or `.yaml`/`.yml`.
    #[error("unsupported budget file extension {ext:?}; expected one of .toml, .json, .yaml, .yml")]
    UnsupportedExtension {
        /// The extension that was found (without the leading dot).
        ext: Option<String>,
    },
    /// The file contents could not be parsed as the chosen format.
    #[error("could not parse budget file {path} as {format}: {source}")]
    Parse {
        /// Path the binary was asked to read.
        path: String,
        /// Format identifier (`"toml"`, `"json"`, or `"yaml"`).
        format: &'static str,
        /// Underlying parse error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
}

impl ResilienceBudget {
    /// Evaluate this budget against a [`ScenarioReport`]. Every applicable rule
    /// is checked; the returned `BudgetOutcome` carries all violations.
    #[must_use]
    pub fn evaluate(&self, report: &ScenarioReport) -> BudgetOutcome {
        let mut violations = Vec::new();

        let total_injected = u64::try_from(report.events.len()).unwrap_or(u64::MAX);

        if let Some(cap) = self.max_injected_total {
            if total_injected > cap {
                violations.push(Violation {
                    rule: "max_injected_total".to_owned(),
                    expected: format!("<= {cap}"),
                    actual: total_injected.to_string(),
                });
            }
        }

        if let Some(floor) = self.min_injected_total {
            if total_injected < floor {
                violations.push(Violation {
                    rule: "min_injected_total".to_owned(),
                    expected: format!(">= {floor}"),
                    actual: total_injected.to_string(),
                });
            }
        }

        if let Some(per_type) = &self.max_injected_per_fault_type {
            let mut counts: BTreeMap<String, u64> = BTreeMap::new();
            for event in &report.events {
                counts
                    .entry(event.fault_type.clone())
                    .and_modify(|n| *n = n.saturating_add(1))
                    .or_insert(1);
            }
            for (fault_type, cap) in per_type {
                let actual = counts.get(fault_type).copied().unwrap_or(0);
                if actual > *cap {
                    violations.push(Violation {
                        rule: format!("max_injected_per_fault_type[{fault_type}]"),
                        expected: format!("<= {cap}"),
                        actual: actual.to_string(),
                    });
                }
            }
        }

        if let Some(required) = &self.require_fault_types {
            let observed: std::collections::HashSet<&str> = report
                .events
                .iter()
                .map(|event| event.fault_type.as_str())
                .collect();
            for fault_type in required {
                if !observed.contains(fault_type.as_str()) {
                    violations.push(Violation {
                        rule: format!("require_fault_types[{fault_type}]"),
                        expected: ">= 1 occurrence".to_owned(),
                        actual: "0 occurrences".to_owned(),
                    });
                }
            }
        }

        if let Some(forbidden) = &self.forbid_regime {
            if forbidden.contains(&report.regime) {
                violations.push(Violation {
                    rule: "forbid_regime".to_owned(),
                    expected: format!("not in {forbidden:?}"),
                    actual: format!("{:?}", report.regime),
                });
            }
        }

        if let Some(cap) = self.max_scenario_duration_ms {
            if report.total_duration_ms > cap {
                violations.push(Violation {
                    rule: "max_scenario_duration_ms".to_owned(),
                    expected: format!("<= {cap}"),
                    actual: report.total_duration_ms.to_string(),
                });
            }
        }

        BudgetOutcome {
            passed: violations.is_empty(),
            violations,
        }
    }

    /// Load a budget from a file. The format is chosen by extension:
    /// `.toml`, `.json`, `.yaml`, `.yml`. An extensionless or otherwise
    /// unrecognized file is rejected with [`BudgetError::UnsupportedExtension`].
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the file is unreadable, an
    /// [`BudgetError::UnsupportedExtension`] if the extension is unknown,
    /// or a [`BudgetError::Parse`] if the contents fail to deserialize.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<Self, BudgetError> {
        let path_ref = path.as_ref();
        let path_display = path_ref.display().to_string();
        let raw = std::fs::read_to_string(path_ref).map_err(|e| BudgetError::Io {
            path: path_display.clone(),
            source: e,
        })?;
        let ext = path_ref
            .extension()
            .and_then(|s| s.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("toml") => toml::from_str(&raw).map_err(|e| BudgetError::Parse {
                path: path_display,
                format: "toml",
                source: Box::new(e),
            }),
            Some("json") => serde_json::from_str(&raw).map_err(|e| BudgetError::Parse {
                path: path_display,
                format: "json",
                source: Box::new(e),
            }),
            Some("yaml" | "yml") => serde_yaml::from_str(&raw).map_err(|e| BudgetError::Parse {
                path: path_display,
                format: "yaml",
                source: Box::new(e),
            }),
            other => Err(BudgetError::UnsupportedExtension {
                ext: other.map(str::to_owned),
            }),
        }
    }
}

/// Pretty-print a [`BudgetOutcome`] as a multi-line human summary suitable for
/// stderr.
#[must_use]
pub fn format_outcome(outcome: &BudgetOutcome) -> String {
    use std::fmt::Write as _;
    if outcome.passed {
        return "resilience budget: satisfied".to_owned();
    }
    let mut out = String::from("resilience budget: VIOLATED\n");
    for (i, v) in outcome.violations.iter().enumerate() {
        let _ = writeln!(
            out,
            "  {}. [{}] expected {}, got {}",
            i + 1,
            v.rule,
            v.expected,
            v.actual
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::scenario::ChaosScenario;
    use malcolm_core::bifurcation::BifurcationProfile;

    fn run_scenario(intensity: f64) -> ScenarioReport {
        let scenario = ChaosScenario::builder()
            .name("budget-test")
            .seed(7)
            .add_fault(PacketLoss::builder().seed(7).intensity(intensity).build())
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

    fn run_empty_scenario() -> ScenarioReport {
        // An empty scenario injects no events, so duration is also 0.
        let scenario = ChaosScenario::builder()
            .name("budget-empty")
            .seed(7)
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
    fn empty_budget_always_passes() {
        let report = run_scenario(0.9);
        let outcome = ResilienceBudget::default().evaluate(&report);
        assert!(outcome.passed);
        assert_eq!(outcome.violations.len(), 0);
    }

    #[test]
    fn min_injected_total_catches_dry_run() {
        let report = run_empty_scenario();
        let outcome = ResilienceBudget {
            min_injected_total: Some(1),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(!outcome.passed);
        assert_eq!(outcome.violations.len(), 1);
        let first = outcome.violations.first().expect("one violation");
        assert_eq!(first.rule, "min_injected_total");
    }

    #[test]
    fn max_injected_total_breach_accumulates() {
        let report = run_scenario(0.9);
        let outcome = ResilienceBudget {
            max_injected_total: Some(0),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(!outcome.passed);
        let first = outcome.violations.first().expect("one violation");
        assert_eq!(first.rule, "max_injected_total");
    }

    #[test]
    fn multiple_breaches_accumulate_not_short_circuit() {
        let report = run_scenario(0.9);
        // The PacketLoss-only scenario at intensity 0.9 regimes as
        // Sensitive or Chaotic depending on threshold tuning. Forbid every
        // non-Stable regime so the check fires regardless of classification.
        let outcome = ResilienceBudget {
            max_injected_total: Some(0),
            min_injected_total: Some(99),
            forbid_regime: Some(vec![ScenarioRegime::Sensitive, ScenarioRegime::Chaotic]),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(!outcome.passed);
        assert_eq!(
            outcome.violations.len(),
            3,
            "all three breaches should be reported: {:?}",
            outcome.violations
        );
    }

    #[test]
    fn per_fault_type_cap_breach_reports_one_violation() {
        let report = run_scenario(0.9);
        let mut caps = BTreeMap::new();
        caps.insert("packet_loss".to_owned(), 0);
        let outcome = ResilienceBudget {
            max_injected_per_fault_type: Some(caps),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(!outcome.passed);
        assert_eq!(outcome.violations.len(), 1);
        let first = outcome.violations.first().expect("one violation");
        assert!(
            first.rule.starts_with("max_injected_per_fault_type"),
            "rule was {}",
            first.rule
        );
    }

    #[test]
    fn require_fault_types_absent_is_violation() {
        let report = run_scenario(0.9);
        let outcome = ResilienceBudget {
            require_fault_types: Some(vec!["non_existent_fault".to_owned()]),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(!outcome.passed);
        assert_eq!(outcome.violations.len(), 1);
        let first = outcome.violations.first().expect("one violation");
        assert!(
            first.rule.starts_with("require_fault_types"),
            "rule was {}",
            first.rule
        );
    }

    #[test]
    fn forbid_regime_breach_detected() {
        let report = run_scenario(0.9);
        let outcome = ResilienceBudget {
            forbid_regime: Some(vec![ScenarioRegime::Sensitive, ScenarioRegime::Chaotic]),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(
            outcome.violations.iter().any(|v| v.rule == "forbid_regime"),
            "forbid_regime should fire: {:?}",
            outcome.violations
        );
    }

    #[test]
    fn max_scenario_duration_ms_breach() {
        // The chaos scenario runs in well under a millisecond on a quiet
        // host, but on a busy CI runner it can take 1–2 ms. Pick a
        // generous budget (a full second) so the synthetic report is
        // guaranteed to pass.
        let report = run_scenario(0.9);
        let outcome = ResilienceBudget {
            max_scenario_duration_ms: Some(1_000),
            ..Default::default()
        }
        .evaluate(&report);
        assert!(outcome.passed, "fast scenario should satisfy a 1s budget");

        // Build a synthetic report with non-zero duration to break the budget.
        let mut synthetic = report.clone();
        synthetic.total_duration_ms = 10;
        let outcome = ResilienceBudget {
            max_scenario_duration_ms: Some(5),
            ..Default::default()
        }
        .evaluate(&synthetic);
        assert!(!outcome.passed);
        let first = outcome.violations.first().expect("one violation");
        assert_eq!(first.rule, "max_scenario_duration_ms");
    }

    #[test]
    fn budget_round_trips_through_toml_json_yaml() {
        let mut caps = BTreeMap::new();
        caps.insert("packet_loss".to_owned(), 50);
        let original = ResilienceBudget {
            max_injected_total: Some(100),
            min_injected_total: Some(1),
            max_injected_per_fault_type: Some(caps),
            require_fault_types: Some(vec!["packet_loss".to_owned(), "latency_spike".to_owned()]),
            forbid_regime: Some(vec![ScenarioRegime::Chaotic]),
            max_scenario_duration_ms: Some(5_000),
        };

        let toml = toml::to_string(&original).expect("serialize toml");
        let json = serde_json::to_string(&original).expect("serialize json");
        let yaml = serde_yaml::to_string(&original).expect("serialize yaml");

        let from_toml: ResilienceBudget = toml::from_str(&toml).expect("parse toml");
        let from_json: ResilienceBudget = serde_json::from_str(&json).expect("parse json");
        let from_yaml: ResilienceBudget = serde_yaml::from_str(&yaml).expect("parse yaml");

        assert_eq!(from_toml, original);
        assert_eq!(from_json, original);
        assert_eq!(from_yaml, original);
    }

    #[test]
    fn from_file_round_trip_to_disk() {
        let mut caps = BTreeMap::new();
        caps.insert("packet_loss".to_owned(), 50);
        let original = ResilienceBudget {
            min_injected_total: Some(1),
            max_injected_per_fault_type: Some(caps),
            ..Default::default()
        };

        let tmp = std::env::temp_dir().join("malcolm_budget_test.toml");
        let serialized = toml::to_string(&original).expect("serialize toml");
        if std::fs::write(&tmp, serialized).is_err() {
            // If the temp dir is read-only, skip the test.
            return;
        }
        let loaded = ResilienceBudget::from_file(&tmp).expect("load toml");
        assert_eq!(loaded, original);
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn from_file_rejects_unknown_extension() {
        let tmp = std::env::temp_dir().join("malcolm_budget_test.bin");
        // Write bytes via a checked path; the test is asserting that
        // from_file rejects the file, not that the write can panic.
        if std::fs::write(&tmp, b"\x00\x01").is_err() {
            // If the temp dir is read-only, skip the test rather than fail.
            return;
        }
        let err = match ResilienceBudget::from_file(&tmp) {
            Ok(_) => {
                let _ = std::fs::remove_file(&tmp);
                // From a test perspective: the budget parser accepted a
                // .bin file. Surface that as a labelled assertion failure
                // without invoking the `panic!` macro (which is forbidden
                // by the linting contract).
                let condition = false;
                assert!(
                    condition,
                    "from_file unexpectedly succeeded for unknown extension"
                );
                return;
            }
            Err(e) => e,
        };
        assert!(matches!(err, BudgetError::UnsupportedExtension { .. }));
        let _ = std::fs::remove_file(&tmp);
    }
}
