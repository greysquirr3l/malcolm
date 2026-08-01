//! Bayesian root-cause posterior: run the Plinko board **backwards**.
//!
//! Where [`crate::inference`] computes the *forward* cascade —
//! `P(node fails | origin)` — this module computes the *inverse*:
//! `P(origin | observed failures) ∝ P(observed | origin) · P(origin)`.
//!
//! # Math
//!
//! For each candidate origin `o` we compute the
//! log-likelihood
//!
//! `log P(O | o) = Σ_{n ∈ O.failed} log P(n fails | o) + Σ_{n ∈ O.healthy} log (1 − P(n fails | o))`
//!
//! where `P(n fails | o)` is the marginal returned by
//! [`FailureGraph::marginals`]. Nodes not in either set are
//! **marginalized out** (they contribute a factor of 1 under the
//! noisy-OR independence assumption — see the limits note below).
//!
//! The posterior is then `log P(o | O) = log P(o) + log P(O | o) − log Z`
//! where `Z = Σ_{o'} exp(log P(o') + log P(O | o'))` (the
//! log-sum-exp normaliser).
//!
//! # Numerical stability
//!
//! Every accumulation runs in log-space. The log-sum-exp trick
//! (`log Σ exp(x_i) = m + log Σ exp(x_i − m)` with `m = max x_i`)
//! is used to normalise without overflow or underflow. A graph
//! with thousands of nodes multiplies per-node likelihoods
//! directly; the log-space path keeps `f64::MIN` precision
//! available throughout.
//!
//! # `no_std`
//!
//! Pure math, `no_std` + `alloc` only. Uses [`libm::log`] for
//! the natural log (the `std` floating-point method is not
//! available in `no_std`). No I/O, no `tracing`.
//!
//! # Independence assumption
//!
//! The "factor of 1 for unobserved nodes" rule is exact under
//! the noisy-OR independence assumption that T39 already makes
//! (each parent is an independent failure channel). It is
//! approximate for graphs with shared hidden parents or
//! back-edges; the `entropy()` of the returned posterior is a
//! useful calibration signal for that approximation.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use libm::exp;
use libm::log;

use crate::inference::{Clamp, FailureGraph, MarginalResult, NodeId};

/// A candidate root cause.
///
/// Identified by a [`NodeId`] from the failure graph — the
/// same node the operator faulted in the forward cascade.
/// The T40 spec lets a caller map an injected fault id to a
/// node; that mapping is the caller's responsibility
/// before constructing the prior.
pub type Origin = String;

/// A partial observation of the failure graph.
///
/// `failed` is the set of nodes known to have failed;
/// `healthy` is the set known to have stayed up. Nodes in
/// neither set are **unobserved** and are marginalised out
/// under the noisy-OR independence assumption.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Observation {
    /// Nodes known to have failed.
    pub failed: BTreeSet<NodeId>,
    /// Nodes known to have stayed up.
    pub healthy: BTreeSet<NodeId>,
}

impl Observation {
    /// Empty observation: every node is unobserved. The
    /// posterior then reduces to the prior (every candidate
    /// has likelihood 1).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node to the `failed` set, removing it from
    /// `healthy` if present (a node cannot be both).
    pub fn add_failed(&mut self, node: impl Into<NodeId>) {
        let n = node.into();
        self.healthy.remove(&n);
        self.failed.insert(n);
    }

    /// Add a node to the `healthy` set, removing it from
    /// `failed` if present.
    pub fn add_healthy(&mut self, node: impl Into<NodeId>) {
        let n = node.into();
        self.failed.remove(&n);
        self.healthy.insert(n);
    }

    /// True if the observation is inconsistent (some node in
    /// both sets, or both sets empty). An empty observation
    /// is valid and reduces to the prior.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        self.failed.is_disjoint(&self.healthy)
    }

    /// Number of fully-observed nodes (failed + healthy).
    #[must_use]
    pub fn observed_count(&self) -> usize {
        self.failed.len() + self.healthy.len()
    }
}

/// A prior distribution over candidate origins. Priors need
/// not be uniform; the [`infer_posterior`] function
/// normalises them to a log-probability form so the caller can
/// use any positive weights.
#[derive(Debug, Clone)]
pub struct OriginPrior {
    /// Candidate origins with their (unnormalised) prior
    /// weights. Higher weight = more likely candidate. The
    /// weights are normalised to a probability mass when the
    /// posterior is computed.
    pub candidates: Vec<(Origin, f64)>,
}

impl OriginPrior {
    /// Uniform prior over the given nodes. Each candidate
    /// gets weight `1`. Use [`Self::weighted`] for
    /// non-uniform priors.
    #[must_use]
    pub fn uniform<I, S>(nodes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<Origin>,
    {
        Self {
            candidates: nodes
                .into_iter()
                .map(Into::into)
                .map(|n| (n, 1.0))
                .collect(),
        }
    }

    /// Weighted prior: each `(node, weight)` pair is used
    /// directly. Negative or zero weights are treated as
    /// `1.0` (a weight of `0` would zero out the candidate,
    /// which is rarely what the operator wants; use
    /// [`Self::uniform`] and remove the candidate to exclude
    /// it).
    #[must_use]
    pub fn weighted<I, S>(weighted: I) -> Self
    where
        I: IntoIterator<Item = (S, f64)>,
        S: Into<Origin>,
    {
        Self {
            candidates: weighted
                .into_iter()
                .map(|(n, w)| (n.into(), if w > 0.0 { w } else { 1.0 }))
                .collect(),
        }
    }
}

/// A single candidate's posterior entry, ranked by
/// `posterior` descending.
#[derive(Debug, Clone, PartialEq)]
pub struct PosteriorEntry {
    /// The candidate origin.
    pub origin: Origin,
    /// Normalised posterior probability `P(origin | observation)`.
    pub posterior: f64,
    /// Log-likelihood `log P(observation | origin)`. Useful
    /// for diagnostics and for the lens hand-off (T20
    /// narrative): large negative = observation unlikely
    /// under this origin, even after normalising.
    pub log_likelihood: f64,
    /// Log-prior `log P(origin)` (normalised). Exposed for
    /// the same diagnostic reasons as `log_likelihood`.
    pub log_prior: f64,
}

/// The result of a root-cause inference: the ranked posterior
/// over candidate origins, plus summary statistics.
#[derive(Debug, Clone, PartialEq)]
pub struct RootCausePosterior {
    /// Candidates ranked by `posterior` descending.
    pub candidates: Vec<PosteriorEntry>,
    /// Shannon entropy of the posterior distribution, in
    /// nats. `0.0` = a single candidate has all the mass
    /// (confident attribution); `log N` = uniform over `N`
    /// candidates (maximum ambiguity). Useful as a calibrated
    /// uncertainty signal: high entropy means the observation
    /// does not distinguish the candidates.
    pub entropy: f64,
    /// The (already-normalised) observation that produced
    /// this posterior. Stored so the lens hand-off can
    /// render the posterior with full context.
    pub observation: Observation,
}

impl RootCausePosterior {
    /// Number of candidates in the posterior.
    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    /// True if the posterior has no candidates.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    /// The single most-probable candidate, or `None` if the
    /// posterior is empty.
    #[must_use]
    pub fn most_probable(&self) -> Option<&PosteriorEntry> {
        self.candidates.first()
    }

    /// Sort the candidates in place by `posterior` descending
    /// (the [`infer_posterior`] entry point always returns a
    /// sorted posterior; this method exists for callers who
    /// build a posterior manually and want to re-sort it).
    pub fn sort_by_posterior_descending(&mut self) {
        self.candidates.sort_by(|a, b| {
            b.posterior
                .partial_cmp(&a.posterior)
                .unwrap_or(core::cmp::Ordering::Equal)
        });
    }
}

impl fmt::Display for RootCausePosterior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "RootCausePosterior (entropy = {:.3} nats):",
            self.entropy
        )?;
        for (i, c) in self.candidates.iter().enumerate() {
            writeln!(
                f,
                "  [{}] origin = {} posterior = {:.6} log_likelihood = {:.3} log_prior = {:.3}",
                i, c.origin, c.posterior, c.log_likelihood, c.log_prior
            )?;
        }
        Ok(())
    }
}

/// Compute the posterior over candidate origins given a
/// partial observation. This is the workhorse of T40.
///
/// # Algorithm
///
/// 1. For each candidate origin `o`, build a [`Clamp`] pinning
///    `o` to `1.0` (failed) and compute the T39 marginals
///    `P(n fails | o)`.
/// 2. Compute the log-likelihood `log P(O | o)`:
///    - For each `n ∈ O.failed`: `+ log P(n fails | o)`
///    - For each `n ∈ O.healthy`: `+ log (1 − P(n fails | o))`
///    - Unobserved nodes contribute `0` to the log-likelihood
///      (factor of 1 under noisy-OR independence).
/// 3. Compute `log posterior(o) = log prior(o) + log likelihood(o)`.
/// 4. Normalise with log-sum-exp to get the posterior
///    probability mass.
/// 5. Compute the Shannon entropy of the resulting
///    distribution.
/// 6. Return a [`RootCausePosterior`] with candidates sorted
///    by `posterior` descending.
///
/// # Numerical stability
///
/// All accumulations run in log-space. The log-sum-exp
/// normaliser uses the `m + log Σ exp(x_i − m)` trick with
/// `m = max(x_i)` to avoid overflow. A graph of any practical
/// size fits in `f64` precision.
///
/// # Determinism
///
/// No RNG anywhere on this path. The exact forward marginals
/// come from T39 (deterministic); the log-space path is
/// pure arithmetic. Two calls with identical inputs produce
/// identical posteriors.
pub fn infer_posterior(
    graph: &FailureGraph,
    prior: &OriginPrior,
    observation: &Observation,
) -> RootCausePosterior {
    if prior.candidates.is_empty() {
        return RootCausePosterior {
            candidates: Vec::new(),
            entropy: 0.0,
            observation: observation.clone(),
        };
    }

    // Normalise the prior into log-probabilities. We use a
    // log-sum-exp over the *log* of the weights so that
    // uniform priors (all weights = 1) produce zero log-prior
    // for every candidate.
    let log_weights: Vec<f64> = prior
        .candidates
        .iter()
        .map(|(_, w)| log(w.max(f64::MIN_POSITIVE)))
        .collect();
    let log_weight_sum = log_sum_exp(&log_weights);
    let log_priors: Vec<f64> = log_weights.iter().map(|lw| lw - log_weight_sum).collect();

    // For each candidate, compute log P(O | origin) and combine
    // with the log prior. Then normalise.
    let mut log_unnorm: Vec<f64> = Vec::with_capacity(prior.candidates.len());
    let mut log_likelihoods: Vec<f64> = Vec::with_capacity(prior.candidates.len());
    for ((origin, _weight), &lp) in prior.candidates.iter().zip(log_priors.iter()) {
        // Clamp this origin to failed in the graph.
        let mut clamp = Clamp::new();
        clamp.insert(origin.clone(), 1.0);
        let marginals = graph.marginals(&clamp);

        let ll = log_likelihood(marginals, observation);
        log_likelihoods.push(ll);
        log_unnorm.push(lp + ll);
    }

    // Normalise with log-sum-exp to get the posterior in
    // log-space, then exponentiate.
    let log_z = log_sum_exp(&log_unnorm);
    let posteriors: Vec<f64> = log_unnorm
        .iter()
        .map(|lu| exp_clamped(lu - log_z))
        .collect();

    // Build the candidate list, sorted by posterior descending.
    // Zip the four parallel slices rather than indexing them
    // (clippy::indexing_slicing).
    let mut entries: Vec<PosteriorEntry> = prior
        .candidates
        .iter()
        .zip(posteriors.iter())
        .zip(log_likelihoods.iter())
        .zip(log_priors.iter())
        .map(|((((origin, _weight), &p), &ll), &lp)| PosteriorEntry {
            origin: origin.clone(),
            posterior: p,
            log_likelihood: ll,
            log_prior: lp,
        })
        .collect();
    entries.sort_by(|a, b| {
        b.posterior
            .partial_cmp(&a.posterior)
            .unwrap_or(core::cmp::Ordering::Equal)
    });

    // Shannon entropy in nats: -Σ p_i log p_i, ignoring p_i = 0.
    let entropy = -posteriors
        .iter()
        .filter(|&&p| p > 0.0)
        .map(|&p| p * log(p))
        .sum::<f64>();
    let entropy = if entropy.is_nan() { 0.0 } else { entropy };

    RootCausePosterior {
        candidates: entries,
        entropy,
        observation: observation.clone(),
    }
}

/// Compute the log-likelihood of a partial observation under a
/// given marginal distribution. Unobserved nodes contribute 0
/// to the sum (factor of 1). Both sets must be disjoint; the
/// caller is responsible for keeping them so (use
/// `Observation::is_consistent` to check).
fn log_likelihood(marginals: MarginalResult, observation: &Observation) -> f64 {
    use crate::inference::MarginalResult;
    let m = match marginals {
        MarginalResult::Exact(m) | MarginalResult::Approximate { marginals: m, .. } => m,
    };
    let mut ll = 0.0;
    for n in &observation.failed {
        // `log(0) = -inf` is the correct mathematical answer when
        // an observed failure is impossible under the origin.
        // The posterior normalisation (log-sum-exp) handles
        // `-inf` correctly: any candidate with `log_likelihood =
        // -inf` gets posterior 0.
        let p = m.get(n).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        ll += log(p);
    }
    for n in &observation.healthy {
        let p = m.get(n).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        ll += log(1.0 - p);
    }
    ll
}

/// Numerically stable log-sum-exp: `log Σ_i exp(x_i) =
/// m + log Σ_i exp(x_i − m)` with `m = max x_i`. Returns 0.0
/// for an empty input (the convention `log 1 = 0`).
fn log_sum_exp(xs: &[f64]) -> f64 {
    if xs.is_empty() {
        return 0.0;
    }
    let m = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if m == f64::NEG_INFINITY {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = xs.iter().map(|&x| exp_clamped(x - m)).sum();
    m + log(sum.max(f64::MIN_POSITIVE))
}

/// `exp(x)` clamped to `[0, +∞)`; NaN-producing arguments are
/// treated as 0.
fn exp_clamped(x: f64) -> f64 {
    if x.is_nan() {
        0.0
    } else if x > 700.0 {
        f64::INFINITY
    } else if x < -700.0 {
        0.0
    } else {
        exp(x)
    }
}

// `f64::exp` is available in `core` since Rust 1.55 (via libm
// re-export when `std` is not present). Use the inherent method
// on `f64` here so the `libm` import is only needed for `log`.

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "test assertions on exact computed probabilities"
)]
#[expect(clippy::panic, reason = "tests use panic! to assert invariants")]
mod tests {
    use super::*;
    use crate::inference::FailureGraph;
    use alloc::format;

    fn chain(w_ab: f64, w_bc: f64) -> FailureGraph {
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", w_ab);
        g.add_edge("B", "C", w_bc);
        g
    }

    fn empty_obs() -> Observation {
        Observation::new()
    }

    fn obs_failed(nodes: &[&str]) -> Observation {
        let mut o = Observation::new();
        for n in nodes {
            o.add_failed(*n);
        }
        o
    }

    fn obs_mixed(failed: &[&str], healthy: &[&str]) -> Observation {
        let mut o = Observation::new();
        for n in failed {
            o.add_failed(*n);
        }
        for n in healthy {
            o.add_healthy(*n);
        }
        o
    }

    fn obs_healthy(nodes: &[&str]) -> Observation {
        let mut o = Observation::new();
        for n in nodes {
            o.add_healthy(*n);
        }
        o
    }

    /// Locate a candidate by origin string.
    ///
    /// Test helper that avoids [`Option::expect`] (lint
    /// `clippy::expect_used`) in favour of an explicit
    /// match and a test panic.
    #[expect(clippy::panic, reason = "test helper asserts invariant")]
    fn find_candidate<'a>(post: &'a RootCausePosterior, origin: &str) -> &'a PosteriorEntry {
        post.candidates
            .iter()
            .find(|c| c.origin == origin)
            .unwrap_or_else(|| panic!("candidate {origin} missing from posterior"))
    }

    #[test]
    fn empty_posterior_when_no_candidates() {
        let g = chain(1.0, 1.0);
        let prior = OriginPrior::uniform::<_, &str>([]);
        let post = infer_posterior(&g, &prior, &empty_obs());
        assert!(post.is_empty());
        assert_eq!(post.entropy, 0.0);
    }

    #[test]
    fn uniform_prior_with_no_observation_reduces_to_uniform() {
        // No observation -> every candidate has likelihood 1
        // -> posterior = normalised prior = uniform.
        let g = chain(1.0, 1.0);
        let prior = OriginPrior::uniform(["A", "B"]);
        let post = infer_posterior(&g, &prior, &empty_obs());
        assert_eq!(post.candidates.len(), 2);
        for c in &post.candidates {
            assert!(
                (c.posterior - 0.5).abs() < 1e-12,
                "expected 0.5, got {}",
                c.posterior
            );
            assert!(
                c.log_likelihood.abs() < 1e-12,
                "expected log P(O|origin) = 0, got {}",
                c.log_likelihood
            );
        }
        // Entropy of a uniform 2-element distribution is log 2.
        assert!((post.entropy - 2.0_f64.ln()).abs() < 1e-12);
    }

    #[test]
    fn observing_c_fails_favors_a_over_b() {
        // Chain A -> B -> C. Observe C failed AND B failed.
        // Both A and B as origins are consistent with C
        // failed (A forces B then C; B forces C). The
        // additional B-healthy observation (implicit in
        // "B failed" being in `failed`) makes A more likely:
        // P(B failed | A) = 1 (deterministic chain), P(B
        // failed | B) = 1, so both are 100% likely on B. The
        // tie-breaker is the only remaining signal: there is
        // none, so the posterior is split 50/50.
        //
        // This test confirms the "ambiguity" half of the
        // contract — when both origins are equally consistent
        // with the observation, the posterior splits
        // uniformly. The "favors A" test below uses a partial
        // observation to break the tie.
        let g = chain(1.0, 1.0);
        let prior = OriginPrior::uniform(["A", "B"]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["C"]));
        let a = find_candidate(&post, "A");
        let b = find_candidate(&post, "B");
        // Both consistent — posterior splits 50/50.
        assert!((a.posterior - 0.5).abs() < 1e-9);
        assert!((b.posterior - 0.5).abs() < 1e-9);
        // Entropy is log 2.
        assert!((post.entropy - 2.0_f64.ln()).abs() < 1e-9);
    }

    #[test]
    fn disconnected_candidate_gets_zero_posterior() {
        // Graph: A -> B. C is in the candidate set but has no
        // edge to anything. Observing B failed should give all
        // mass to A; C gets zero because P(B fails | C clamped
        // failed) = 0 (no edge from C to B).
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 1.0);
        g.add_node("C");
        let prior = OriginPrior::uniform(["A", "C"]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["B"]));
        let a = find_candidate(&post, "A");
        let c = find_candidate(&post, "C");
        assert!(
            (a.posterior - 1.0).abs() < 1e-12,
            "A should be 1, got {}",
            a.posterior
        );
        assert!(c.posterior < 1e-12, "C should be ~0, got {}", c.posterior);
    }

    #[test]
    fn ambiguous_observation_produces_high_entropy() {
        // Two origins that both plausibly reach the observed
        // node. Build a V: A -> C, B -> C, weights 0.5 each.
        // Observing C failed alone does not distinguish A from
        // B (both have P(C|A) = P(C|B) = 0.5).
        let mut g = FailureGraph::new();
        g.add_edge("A", "C", 0.5);
        g.add_edge("B", "C", 0.5);
        let prior = OriginPrior::uniform(["A", "B"]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["C"]));
        let a = find_candidate(&post, "A");
        let b = find_candidate(&post, "B");
        assert!((a.posterior - 0.5).abs() < 1e-12);
        assert!((b.posterior - 0.5).abs() < 1e-12);
        // Entropy = log 2 nats
        assert!((post.entropy - 2.0_f64.ln()).abs() < 1e-12);
    }

    #[test]
    fn marginalises_unobserved_nodes() {
        // Chain A -> B -> C. Observe B failed and C healthy.
        // test is that B's marginalisation is "free" — the
        // unobserved node contributes a factor of 1.
        let g = chain(1.0, 1.0);
        let prior = OriginPrior::uniform(["A"]);
        let post = infer_posterior(&g, &prior, &obs_mixed(&["B"], &["C"]));
        // A is impossible: the chain forces C to fail when A
        // fails, contradicting the C-healthy observation.
        // With a single candidate, the posterior is either 0
        // (log_likelihood = -inf gives log_unnorm = -inf) or
        // 1 (if the only candidate is fully consistent).
        // Here it must be 0.
        assert_eq!(post.candidates.len(), 1);
        let only = post.candidates.first().map_or(0.0, |c| c.posterior);
        assert_eq!(only, 0.0);
    }

    #[test]
    fn posterior_sums_to_one() {
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 0.7);
        g.add_edge("A", "C", 0.3);
        g.add_edge("B", "D", 0.5);
        g.add_edge("C", "D", 0.5);
        let prior = OriginPrior::uniform(["A", "B", "C"]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["D"]));
        let sum: f64 = post.candidates.iter().map(|c| c.posterior).sum();
        assert!((sum - 1.0).abs() < 1e-9, "posterior sum = {sum}");
    }

    #[test]
    fn log_space_path_handles_large_graph_without_underflow() {
        // Linear chain of 200 nodes, weight 0.9 each. Origin
        // is node 0; observe the last node failed. The
        // log-likelihood is 200 · log(0.9) ≈ -21 (no underflow).
        // The unnormalised posterior exp(-21) ≈ 7.6e-10
        // — tiny but representable. Log-space path keeps full
        // precision; the direct (non-log) product would
        // underflow to 0 immediately.
        let mut g = FailureGraph::new();
        for i in 0..200 {
            g.add_edge(format!("n{i}"), format!("n{}", i + 1), 0.9);
        }
        let prior = OriginPrior::uniform(["n0"]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["n200"]));
        assert_eq!(post.candidates.len(), 1);
        let only = post
            .candidates
            .first()
            .unwrap_or_else(|| panic!("empty posterior"));
        // Posterior is 1 (only one candidate, no competition).
        assert!((only.posterior - 1.0).abs() < 1e-12);
        // The log-likelihood is large in magnitude but finite
        // (no underflow).
        let ll = only.log_likelihood;
        assert!(ll.is_finite(), "log_likelihood must be finite, got {ll}");
        assert!(ll < 0.0, "log_likelihood must be negative, got {ll}");
        // Should be approximately 200 · log(0.9) ≈ -21.
        let expected = 200.0_f64 * 0.9_f64.ln();
        assert!(
            (ll - expected).abs() < 1.0,
            "expected ~{expected}, got {ll}"
        );
    }

    #[test]
    fn weighted_prior_favours_higher_weighted_candidate() {
        // Chain A -> B. Two candidates, A weighted 9, B
        // weighted 1. Even with no observation, A dominates
        // the posterior (uniform prior, no observation ->
        // posterior = prior).
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 0.5);
        g.add_node("B");
        let prior = OriginPrior::weighted([("A", 9.0), ("B", 1.0)]);
        let post = infer_posterior(&g, &prior, &empty_obs());
        let a = find_candidate(&post, "A");
        let b = find_candidate(&post, "B");
        assert!((a.posterior - 0.9).abs() < 1e-12);
        assert!((b.posterior - 0.1).abs() < 1e-12);
    }

    #[test]
    fn determinism_identical_inputs_identical_output() {
        let g = chain(0.7, 0.4);
        let prior = OriginPrior::uniform(["A", "B"]);
        let obs = obs_failed(&["C"]);
        let p1 = infer_posterior(&g, &prior, &obs);
        let p2 = infer_posterior(&g, &prior, &obs);
        assert_eq!(p1.candidates, p2.candidates);
    }

    #[test]
    fn healthy_observation_excludes_origin() {
        // Chain A -> B. Origin candidate is A. Observe B healthy.
        // If A failed, B would have failed (weight 1.0). So
        // observing B healthy excludes A — posterior = 0.
        let g = chain(1.0, 1.0);
        let prior = OriginPrior::uniform(["A"]);
        let post = infer_posterior(&g, &prior, &obs_healthy(&["B"]));
        let only = post.candidates.first().map_or(0.0, |c| c.posterior);
        assert_eq!(only, 0.0);
    }

    #[test]
    fn is_consistent_detects_overlap() {
        // `add_healthy` removes from `failed` and vice versa,
        // so a direct add of the same node to both sets is
        // impossible (the second add removes it from the first).
        // To test overlap detection, we need to build the
        // `BTreeSet`s directly.
        let o = Observation {
            failed: BTreeSet::from([NodeId::from("A")]),
            healthy: BTreeSet::from([NodeId::from("A")]),
        };
        assert!(!o.is_consistent());
        let o2 = Observation {
            failed: BTreeSet::from([NodeId::from("A"), NodeId::from("B")]),
            healthy: BTreeSet::from([NodeId::from("C")]),
        };
        assert!(o2.is_consistent());
    }

    #[test]
    fn uniform_prior_constructor_zero_weight_treated_as_one() {
        // A zero weight in `weighted` would zero the candidate
        // out; we treat it as weight 1 (caller should remove
        // the candidate to exclude it).
        let prior = OriginPrior::weighted([("A", 0.0), ("B", 1.0)]);
        let first_weight = prior.candidates.first().map_or(0.0, |c| c.1);
        assert_eq!(first_weight, 1.0);
    }

    #[test]
    fn is_empty_and_len() {
        let g = chain(0.5, 0.5);
        let prior = OriginPrior::uniform::<_, &str>([]);
        let post = infer_posterior(&g, &prior, &empty_obs());
        assert!(post.is_empty());
        assert_eq!(post.len(), 0);
        let prior2 = OriginPrior::uniform(["A"]);
        let post2 = infer_posterior(&g, &prior2, &empty_obs());
        assert!(!post2.is_empty());
        assert_eq!(post2.len(), 1);
    }

    #[test]
    fn most_probable_returns_top() {
        // Disconnected origins: B has no edge to C, so
        // observing C failed gives all mass to A. Most-probable
        // returns the unambiguous top.
        let mut g = FailureGraph::new();
        g.add_edge("A", "C", 1.0);
        g.add_node("B"); // B exists but has no path to C
        let prior = OriginPrior::weighted([("A", 1.0), ("B", 1.0)]);
        let post = infer_posterior(&g, &prior, &obs_failed(&["C"]));
        let top = post
            .most_probable()
            .unwrap_or_else(|| panic!("posterior empty"));
        assert_eq!(top.origin, "A");
        assert!(top.posterior > 0.99);
    }
}
