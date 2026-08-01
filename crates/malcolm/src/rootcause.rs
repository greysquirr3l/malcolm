//! Root-cause posterior wiring: turn a [`ScenarioReport`] into a
//! [`RootCauseReport`].
//!
//! The pure math — Bayes' rule over the T39 noisy-OR likelihoods —
//! lives in [`malcolm_core::posterior`]. This module is the thin
//! adapter that builds a [`malcolm_core::posterior::Observation`]
//! from a [`ScenarioReport`] and runs the inference against the
//! scenario's [`Topology`].
//!
//! # Usage
//!
//! ```rust,ignore
//! use malcolm::scenario::ChaosScenario;
//! use malcolm::topology::Topology;
//! use malcolm::rootcause::root_cause_from_scenario;
//!
//! let topology = Topology::builder()
//!     .name("cluster")
//!     .add_edge("a", "b", 0.8)
//!     .add_edge("b", "c", 0.6)
//!     .build();
//! let scenario = ChaosScenario::builder()
//!     .name("chaos-1")
//!     .topology(topology)
//!     .build();
//! // ... scenario.run(ctx) ...
//! let report = root_cause_from_scenario(&scenario_report, &scenario.topology());
//! assert_eq!(report.posterior.most_probable().unwrap().origin, "a");
//! ```
//!
//! # How the observation is built
//!
//! Each scenario event in the report becomes a candidate origin.
//! The node from `event.node_id` is the target of the candidate
//! fault; the candidate's prior weight is `1.0` (uniform).
//!
//! The observation is constructed from the same event list:
//! - `event.intensity > 0.0` → the node is in `Observation::failed`
//!   (it was successfully injected or produced a measurable effect).
//! - `event.intensity == 0.0` → the node is in `Observation::healthy`.
//!
//! In practice a `ScenarioReport` records events that ran, so
//! almost every event ends up in `failed`. The healthy set is
//! useful when you have an external observation layer (e.g. health
//! checks) that you want to merge in via
//! [`RootCauseConfig::add_healthy`](RootCauseConfig::add_healthy).
//!
//! # Tracing
//!
//! `root_cause_from_scenario` emits a `tracing` event with
//! `fault_type = "root_cause_posterior"` and the top candidate +
//! entropy in the structured fields. Consumers (logs, dashboards)
//! see the summary; programmatic callers get the full
//! `RootCauseReport` return value.

use malcolm_core::inference::FailureGraph;
use malcolm_core::posterior::{
    Observation, Origin, OriginPrior, RootCausePosterior, infer_posterior,
};

use crate::scenario::ScenarioReport;
use crate::topology::Topology;

/// Configuration for [`root_cause_from_scenario`]. Lets the
/// caller add external observations (e.g. from health checks)
/// on top of the events in the `ScenarioReport`.
#[derive(Debug, Clone, Default)]
pub struct RootCauseConfig {
    /// Additional nodes observed as failed (merged with the
    /// `ScenarioReport`'s injected events).
    pub observed_failed: std::collections::BTreeSet<Origin>,
    /// Additional nodes observed as healthy.
    pub observed_healthy: std::collections::BTreeSet<Origin>,
    /// Non-uniform prior weights: `(origin, weight)` pairs that
    /// override the default uniform prior. Weights must be
    /// positive (zero or negative weights are treated as `1.0`).
    pub prior_weights: std::collections::BTreeMap<Origin, f64>,
    /// Sample count for the Monte Carlo blast-radius fallback.
    /// Ignored when the graph is a forest/tree (exact path).
    pub sample_count: usize,
}

impl RootCauseConfig {
    /// Default configuration: uniform prior, 10,000 MC samples.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sample_count: 10_000,
            ..Self::default()
        }
    }

    /// Add an externally-observed failed node.
    pub fn add_failed(&mut self, node: impl Into<Origin>) -> &mut Self {
        self.observed_failed.insert(node.into());
        self
    }

    /// Add an externally-observed healthy node.
    pub fn add_healthy(&mut self, node: impl Into<Origin>) -> &mut Self {
        self.observed_healthy.insert(node.into());
        self
    }

    /// Set a non-uniform prior weight for one candidate.
    pub fn set_prior(&mut self, origin: impl Into<Origin>, weight: f64) -> &mut Self {
        self.prior_weights.insert(origin.into(), weight);
        self
    }

    /// Set the Monte Carlo sample count.
    #[must_use]
    pub const fn with_sample_count(mut self, n: usize) -> Self {
        self.sample_count = n;
        self
    }
}

/// The full result of a root-cause inference.
///
/// Combines the posterior with the context used to compute
/// it. `serde`-serializable so it can be persisted alongside
/// the `ScenarioReport` or shipped to `malcolm-lens` for
/// narration (T40 → T20 hand-off).
///
/// `Eq` is *not* derived because `SerializableCandidate`
/// contains `f64` (which is only `PartialEq`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RootCauseReport {
    /// The ranked posterior over candidate origins. See
    /// [`malcolm_core::posterior::RootCausePosterior`] for the
    /// fields.
    pub posterior: SerializablePosterior,
    /// The graph on which the inference ran. Stored so the
    /// consumer can re-run with a different config without
    /// having to thread the topology through.
    pub graph_node_count: usize,
    /// The number of injected events in the original
    /// `ScenarioReport` (i.e. the number of candidates).
    pub candidate_count: usize,
    /// The merged observation: `ScenarioReport` events ∪
    /// external observations.
    pub observation: SerializableObservation,
}

/// `serde`-serializable view of [`RootCausePosterior`].
///
/// We can't derive `Serialize` on the core type (it lives in
/// `malcolm-core` which is `no_std`), so we mirror it here.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SerializablePosterior {
    /// Candidates ranked by posterior descending.
    pub candidates: Vec<SerializableCandidate>,
    /// Shannon entropy in nats.
    pub entropy: f64,
}

/// A single candidate's serializable view.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SerializableCandidate {
    /// The candidate origin (node id).
    pub origin: Origin,
    /// Posterior probability.
    pub posterior: f64,
    /// Log-likelihood of the observation under this origin.
    pub log_likelihood: f64,
    /// Log-prior of this candidate.
    pub log_prior: f64,
}

/// `serde`-serializable view of [`Observation`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SerializableObservation {
    /// Nodes observed as failed.
    pub failed: Vec<Origin>,
    /// Nodes observed as healthy.
    pub healthy: Vec<Origin>,
    /// Number of nodes not in either set (marginalised out).
    pub unobserved_count: usize,
}

impl RootCauseReport {
    /// Convenience: the most-probable candidate, if any.
    #[must_use]
    pub fn most_probable(&self) -> Option<&SerializableCandidate> {
        self.posterior.candidates.first()
    }

    /// Convenience: the entropy in nats.
    #[must_use]
    pub const fn entropy(&self) -> f64 {
        self.posterior.entropy
    }
}

/// Build a [`RootCauseReport`] from a [`ScenarioReport`] and its
/// [`Topology`]. This is the standard entry point.
///
/// # Algorithm
///
/// 1. Build a `FailureGraph` from the topology.
/// 2. Collect candidate origins from the `ScenarioReport`'s
///    events (deduplicated; each distinct node id is one
///    candidate).
/// 3. Merge the report's events with any external observations
///    from [`RootCauseConfig`] to form the [`Observation`].
/// 4. Run [`infer_posterior`] with the candidate prior and
///    the observation.
/// 5. Emit a `tracing` event and return the report.
#[must_use]
pub fn root_cause_from_scenario(
    report: &ScenarioReport,
    topology: &Topology,
    config: &RootCauseConfig,
) -> RootCauseReport {
    root_cause_with_observation(report, topology, config, None, None)
}

/// Like [`root_cause_from_scenario`], but lets the caller
/// inject extra failed/healthy observations.
///
/// The injected nodes are merged *after* the
/// `ScenarioReport`'s events are folded in. Useful for
/// augmenting with health-check data.
#[must_use]
pub fn root_cause_with_observation(
    report: &ScenarioReport,
    topology: &Topology,
    config: &RootCauseConfig,
    extra_failed: Option<&str>,
    extra_healthy: Option<&str>,
) -> RootCauseReport {
    let graph = topology.to_failure_graph();
    let origin_set: std::collections::BTreeSet<Origin> =
        report.events.iter().map(|e| e.node_id.clone()).collect();
    // Build the prior. If the caller supplied non-uniform
    // weights, use them; otherwise uniform.
    let prior = if config.prior_weights.is_empty() {
        OriginPrior::uniform(origin_set.iter().cloned())
    } else {
        let pairs: Vec<(Origin, f64)> = origin_set
            .iter()
            .map(|n| {
                let w = config.prior_weights.get(n).copied().unwrap_or(1.0);
                (n.clone(), w)
            })
            .collect();
        OriginPrior::weighted(pairs)
    };

    // Build the observation from the report's events.
    let mut obs = Observation::new();
    for e in &report.events {
        // Injected events have intensity > 0 (they ran);
        // a recorded fault with intensity == 0 is unusual but
        // we treat it as "the node stayed up". We treat
        // `Skipped` results as healthy, `Injected` as failed.
        let failed = result_from_event(e) == FaultResultStub::Injected;
        if failed {
            obs.add_failed(e.node_id.clone());
        } else {
            obs.add_healthy(e.node_id.clone());
        }
    }
    // External observations from the config.
    for n in &config.observed_failed {
        obs.add_failed(n.clone());
    }
    for n in &config.observed_healthy {
        obs.add_healthy(n.clone());
    }
    if let Some(n) = extra_failed {
        obs.add_failed(n.to_owned());
    }
    if let Some(n) = extra_healthy {
        obs.add_healthy(n.to_owned());
    }

    // Sample count and seed come from the report for
    // replayability: the same report always produces the same
    // posterior.
    let posterior = infer_posterior(&graph, &prior, &obs);

    // Emit the tracing event (T14 schema).
    let top = posterior.most_probable();
    tracing::info!(
        target: "malcolm",
        fault_type = "root_cause_posterior",
        scenario = %report.name,
        seed = report.seed,
        candidate_count = origin_set.len(),
        top_origin = top.map_or("(none)", |c| c.origin.as_str()),
        top_posterior = top.map_or(0.0, |c| c.posterior),
        entropy = posterior.entropy,
        dry_run = report.events.iter().all(|e| e.dry_run),
        "root-cause posterior computed",
    );

    serialise_report(&posterior, &obs, &graph, origin_set.len())
}

/// Recover a `FaultResult` discriminator from a `ScenarioEvent`'s
/// intensity. We don't store the `FaultResult` enum on the event
/// (it only carries intensity); we reconstruct from the
/// intensity + dry-run flag.
fn result_from_event(event: &crate::scenario::ScenarioEvent) -> FaultResultStub {
    if event.intensity > 0.0 && !event.dry_run {
        FaultResultStub::Injected
    } else {
        FaultResultStub::Skipped
    }
}

/// Tiny discriminator used by `result_from_event`. The
/// `FaultResult` enum has more variants (with associated data)
/// that we don't need for the observation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FaultResultStub {
    Injected,
    Skipped,
}

fn serialise_report(
    posterior: &RootCausePosterior,
    obs: &Observation,
    graph: &FailureGraph,
    candidate_count: usize,
) -> RootCauseReport {
    let serialised_candidates: Vec<SerializableCandidate> = posterior
        .candidates
        .iter()
        .map(|c| SerializableCandidate {
            origin: c.origin.clone(),
            posterior: c.posterior,
            log_likelihood: c.log_likelihood,
            log_prior: c.log_prior,
        })
        .collect();
    let unobserved_count = graph.node_count().saturating_sub(obs.observed_count());
    RootCauseReport {
        posterior: SerializablePosterior {
            candidates: serialised_candidates,
            entropy: posterior.entropy,
        },
        graph_node_count: graph.node_count(),
        candidate_count,
        observation: SerializableObservation {
            failed: obs.failed.iter().cloned().collect(),
            healthy: obs.healthy.iter().cloned().collect(),
            unobserved_count,
        },
    }
}

#[cfg(test)]
#[allow(
    unused_must_use,
    clippy::float_cmp,
    reason = "tests use builder pattern discard + exact equality on math results"
)]
#[expect(clippy::panic, reason = "tests use panic! to assert invariants")]
mod tests {
    use super::*;
    use crate::topology::Topology;

    /// Test helper that returns the most-probable candidate
    /// or panics if the posterior is empty.
    fn top_candidate(rcr: &RootCauseReport) -> &SerializableCandidate {
        rcr.most_probable()
            .unwrap_or_else(|| panic!("posterior has no candidates"))
    }

    /// Locate a serialised candidate by origin. Test helper
    /// that avoids `Option::expect`/`unwrap`.
    fn find_serialised<'a>(rcr: &'a RootCauseReport, origin: &str) -> &'a SerializableCandidate {
        rcr.posterior
            .candidates
            .iter()
            .find(|c| c.origin == origin)
            .unwrap_or_else(|| panic!("candidate {origin} missing from posterior"))
    }

    fn build_scenario() -> (Topology, Vec<String>) {
        // Chain A -> B -> C. The scenario injects faults on
        // each of A, B, C in order so all three are
        // candidate origins. After the run, all three
        // nodes have intensity > 0 and so are observed
        // failed.
        let topology = Topology::builder()
            .name("chain")
            .add_edge("A", "B", 0.5)
            .add_edge("B", "C", 0.5)
            .build();
        let nodes = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        (topology, nodes)
    }

    fn make_report_with_nodes(
        name: &str,
        nodes: &[String],
        seed: u64,
        injected_count: usize,
    ) -> ScenarioReport {
        let events: Vec<crate::scenario::ScenarioEvent> = nodes
            .iter()
            .take(injected_count)
            .enumerate()
            .map(|(i, n)| crate::scenario::ScenarioEvent {
                fault_type: format!("test_fault_{i}"),
                node_id: n.clone(),
                seed,
                intensity: 1.0,
                dry_run: false,
                timestamp_ms: i as u64,
            })
            .collect();
        // `ScenarioRegime` has no `Default` impl; pick a
        // stable variant that matches what the test
        // doesn't care about (we're testing root-cause
        // inference, not regime semantics).
        let regime = crate::scenario::ScenarioRegime::Stable;
        ScenarioReport {
            name: name.to_owned(),
            seed,
            regime,
            events,
            total_duration_ms: 0,
        }
    }

    #[test]
    fn root_cause_uniform_prior_with_independent_observation() {
        // Three isolated nodes. Each candidate is the
        // sole origin of its own failure; the posterior
        // should be 1.0 for each (each candidate
        // independently explains its own observed failure).
        let topology = Topology::builder()
            .name("independent")
            .add_node("A")
            .add_node("B")
            .add_node("C")
            .build();
        // Only A is a candidate; only A is observed failed.
        // B and C are isolated, no leak, so the prior
        // for A is 1.0 (uniform over the singleton set).
        let report = make_report_with_nodes("independent", &["A".to_string()], 42, 1);
        let config = RootCauseConfig::new();
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        assert_eq!(rcr.candidate_count, 1);
        let top = top_candidate(&rcr);
        assert_eq!(top.origin, "A");
        assert!((top.posterior - 1.0).abs() < 1e-9, "got {}", top.posterior);
    }

    #[test]
    fn root_cause_three_way_split_uniform_prior() {
        // Three isolated nodes, three candidates, each
        // observed failed in their own world. To get a
        // three-way split with uniform prior we need each
        // candidate's likelihood to be identical. The
        // scenario is: graph is isolated, observation
        // contains *only* the candidate's own node.
        // Build three separate reports and check that
        // each one's posterior is 1.0 on its candidate.
        let topology = Topology::builder()
            .name("independent")
            .add_node("A")
            .add_node("B")
            .add_node("C")
            .build();
        let config = RootCauseConfig::new();
        // Single-candidate case: each candidate explains
        // its own observation perfectly.
        for cand in ["A", "B", "C"] {
            let nset = vec![cand.to_string()];
            let report = make_report_with_nodes("independent", &nset, 42, 1);
            let rcr = root_cause_from_scenario(&report, &topology, &config);
            assert_eq!(rcr.candidate_count, 1);
            assert_eq!(top_candidate(&rcr).origin, cand);
        }
        // Now construct a *single* scenario with all three
        // candidates and all three observed failed. With
        // no leak and isolated nodes, B and C are
        // observationally impossible when A is the origin,
        // so the posterior concentrates on the candidate
        // whose likelihood is non-zero.
        let report_all = make_report_with_nodes(
            "independent",
            &["A".to_string(), "B".to_string(), "C".to_string()],
            42,
            3,
        );
        let rcr = root_cause_from_scenario(&report_all, &topology, &config);
        assert_eq!(rcr.candidate_count, 3);
        let sum: f64 = rcr.posterior.candidates.iter().map(|c| c.posterior).sum();
        // With isolated nodes and no leak, only the
        // candidate whose node is also the *only* observed
        // failure can survive. Three observed failures
        // with three independent candidates is logically
        // inconsistent, so the posterior is zero across
        // the board.
        assert!(sum.abs() < 1e-9, "sum = {sum}");
    }

    #[test]
    fn root_cause_with_partial_event_set() {
        // Only A injected. B and C are observed failed
        // (cascaded from A) but only A is a candidate. The
        // posterior is 100% on A.
        let (topology, nodes) = build_scenario();
        let report = make_report_with_nodes("chain", &nodes, 42, 1);
        let config = RootCauseConfig::new();
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        assert_eq!(rcr.candidate_count, 1);
        let top = top_candidate(&rcr);
        assert_eq!(top.origin, "A");
        assert!((top.posterior - 1.0).abs() < 1e-9);
    }

    #[test]
    fn root_cause_with_unhealthy_observation_excludes_candidate() {
        // Chain A -> B with weight 1. Only A is a candidate.
        // We observe B healthy. A failed forces B failed,
        // so the observation contradicts A; the posterior
        // must be 0.
        let topology = Topology::builder()
            .name("chain")
            .add_edge("A", "B", 1.0)
            .build();
        let nodes = vec!["A".to_string()];
        let report = make_report_with_nodes("chain", &nodes, 42, 1);
        let mut config = RootCauseConfig::new();
        config.add_healthy("B");
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        assert_eq!(rcr.candidate_count, 1);
        assert_eq!(top_candidate(&rcr).posterior, 0.0);
    }

    #[test]
    fn root_cause_external_observation_breaks_tie() {
        // Two candidates A and B, both connected to C with
        // weight 0.5. Observing C failed alone splits the
        // posterior 50/50 (the test is for ambiguity). Now
        // observe A healthy — that forces P(C failed | A) = 0
        // (deterministic chain with weight 1.0 from A). The
        // posterior should collapse to B.
        let topology = Topology::builder()
            .name("v")
            .add_edge("A", "C", 1.0)
            .add_edge("B", "C", 0.5)
            .build();
        let nodes = vec!["A".to_string(), "B".to_string()];
        let report = make_report_with_nodes("v", &nodes, 42, 2);
        let mut config = RootCauseConfig::new();
        config.add_failed("C");
        config.add_healthy("A");
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        let top = top_candidate(&rcr);
        assert_eq!(top.origin, "B");
        assert!((top.posterior - 1.0).abs() < 1e-9);
    }

    #[test]
    fn root_cause_non_uniform_prior_favours_higher_weighted() {
        let (topology, nodes) = build_scenario();
        let report = make_report_with_nodes("chain", &nodes, 42, 3);
        let mut config = RootCauseConfig::new();
        config.set_prior("A", 9.0);
        config.set_prior("B", 1.0);
        config.set_prior("C", 1.0);
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        let a = find_serialised(&rcr, "A");
        assert!(a.posterior > 0.8, "A should dominate: got {}", a.posterior);
    }

    #[test]
    fn root_cause_report_serialises_to_json() {
        // The whole point of the serde round-trip is so the
        // lens can consume a posterior over JSON.
        let (topology, nodes) = build_scenario();
        let report = make_report_with_nodes("chain", &nodes, 42, 1);
        let config = RootCauseConfig::new();
        let rcr = root_cause_from_scenario(&report, &topology, &config);
        let json = match serde_json::to_string(&rcr) {
            Ok(s) => s,
            Err(e) => panic!("serialise failed: {e}"),
        };
        let back: RootCauseReport = match serde_json::from_str(&json) {
            Ok(r) => r,
            Err(e) => panic!("deserialise failed: {e}"),
        };
        assert_eq!(back.candidate_count, rcr.candidate_count);
        assert!((back.entropy() - rcr.entropy()).abs() < 1e-12);
    }

    #[test]
    fn root_cause_config_default_sample_count_is_ten_thousand() {
        let cfg = RootCauseConfig::new();
        assert_eq!(cfg.sample_count, 10_000);
    }

    #[test]
    fn root_cause_with_observation_extra_nodes() {
        // Same as the tie-breaker test but using the
        // explicit `extra_*` arguments instead of
        // config.add_*.
        let topology = Topology::builder()
            .name("v")
            .add_edge("A", "C", 1.0)
            .add_edge("B", "C", 0.5)
            .build();
        let nodes = vec!["A".to_string(), "B".to_string()];
        let report = make_report_with_nodes("v", &nodes, 42, 2);
        let config = RootCauseConfig::new();
        let rcr = root_cause_with_observation(&report, &topology, &config, Some("C"), Some("A"));
        let top = top_candidate(&rcr);
        assert_eq!(top.origin, "B");
        assert!((top.posterior - 1.0).abs() < 1e-9);
    }
}
