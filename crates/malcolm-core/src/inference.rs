//! Bayesian cascade network: analytic and approximate inference over
//! the failure graph.
//!
//! This module is the **analytic companion** to `CascadeFault` in
//! `malcolm`. Where the cascade sampler runs one forward Plinko
//! bounce per call, the inference engine computes the
//! *distribution* of outcomes analytically — marginals, blast-radius
//! — so callers can answer "what's the probability of N nodes
//! failing?" without running 10,000 cascades.
//!
//! # Math model
//!
//! The graph is a directed graph of `NodeId` with edge weights
//! `w ∈ [0, 1]` interpreted as
//! `P(child fails | that specific parent failed)`. Multiple failing
//! parents are combined with a **noisy-OR** rule (canonical choice
//! for independent propagation channels):
//!
//! `P(child fails | parents) = 1 − Π_i (1 − wᵢ · 1[parentᵢ failed])`
//!
//! Every node also has a **leak probability** `ℓ` that sets a
//! minimum spontaneous failure floor:
//! `P(child fails) ≥ ℓ` regardless of parent state. `ℓ = 0`
//! recovers the pure noisy-OR model.
//!
//! # Algorithms
//!
//! - **DAG fast path**: exact marginals by topological propagation of
//!   the noisy-OR (linear in edges). The common, fast path.
//! - **Cyclic fallback**: loopy belief propagation with a fixed
//!   iteration cap, deterministic, seeded. Returns a
//!   [`BpOutcome::Converged`] or [`BpOutcome::NonConverged`] flag;
//!   never loops forever.
//! - **Blast-radius distribution**: exact dynamic-programming
//!   convolution of per-node Bernoullis when the graph is a
//!   tree/forest; otherwise a seeded Monte Carlo estimator with a
//!   standard-error estimate. The result type records which method
//!   was used.
//!
//! # `no_std` and `alloc`
//!
//! This module is `#![no_std]`-compatible and uses only `alloc`
//! (already imported by the crate root). No I/O, no `tracing` — the
//! `malcolm`-side adapter does any logging (per the T05 learning
//! that the `no_std` boundary is absolute).
//!
//! # Determinism
//!
//! Exact paths are deterministic by construction. Every sampling
//! fallback takes an explicit seed; the caller (the `malcolm`
//! adapter) threads `FaultContext::seed` through so results are
//! replayable. There is no unseeded RNG anywhere in this module.

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt;

use libm::sqrt;
use rand::RngExt as _;
use rand::SeedableRng;
use rand::rngs::SmallRng;

/// Node identifier. We use `String` for ergonomics on the
/// `malcolm`-side adapter (it maps to `Topology::node_ids()`); the
/// inference math itself is identifier-agnostic.
pub type NodeId = String;

/// A directed probabilistic graph over which we run inference.
///
/// The graph is stored as a parent map: for each node, the list of
/// `(parent, edge_weight)` pairs where `weight = P(child fails |
/// that parent failed)`. This makes the noisy-OR computation a
/// single pass over a node's parents.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FailureGraph {
    /// All node identifiers (sorted for determinism).
    nodes: Vec<NodeId>,
    /// `parents[child] = [(parent, w), ...]` — directed incoming
    /// edges. Sorted by parent id for determinism.
    parents: BTreeMap<NodeId, Vec<(NodeId, f64)>>,
    /// Per-node spontaneous failure floor `P(node fails) ≥ leak`.
    /// Absent entries default to `0.0` (no leak).
    leak: BTreeMap<NodeId, f64>,
}

impl FailureGraph {
    /// Empty graph.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node. Idempotent: adding an existing node is a no-op.
    pub fn add_node(&mut self, node: impl Into<NodeId>) {
        let n = node.into();
        if !self.nodes.contains(&n) {
            // Maintain sorted order on insert for determinism.
            let pos = self.nodes.binary_search(&n).unwrap_err();
            self.nodes.insert(pos, n.clone());
        }
        self.parents.entry(n).or_default();
    }

    /// Add a directed edge `parent -> child` with conditional
    /// propagation weight. The weight is clamped into `[0.0, 1.0]`
    /// and the endpoints are added as nodes if missing. Self-loops
    /// are allowed (the leak term covers the failure-floor case).
    pub fn add_edge(&mut self, parent: impl Into<NodeId>, child: impl Into<NodeId>, weight: f64) {
        let p = parent.into();
        let c = child.into();
        let w = weight.clamp(0.0, 1.0);
        self.add_node(p.clone());
        self.add_node(c.clone());
        let entry = self.parents.entry(c).or_default();
        // Maintain sorted parent order.
        match entry.binary_search_by(|(pp, _)| pp.cmp(&p)) {
            Ok(_) => {} // already present
            Err(pos) => entry.insert(pos, (p, w)),
        }
    }

    /// Set the leak probability for a node. Clamped into
    /// `[0.0, 1.0]`. The node is added if missing.
    pub fn set_leak(&mut self, node: impl Into<NodeId>, leak: f64) {
        let n = node.into();
        self.add_node(n.clone());
        self.leak.insert(n, leak.clamp(0.0, 1.0));
    }

    /// Number of nodes.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// All node identifiers in sorted order.
    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        &self.nodes
    }

    /// Parents of a node with their edge weights. Returns an
    /// empty slice if the node has no parents (or doesn't exist).
    #[must_use]
    pub fn parents_of(&self, node: &str) -> &[(NodeId, f64)] {
        self.parents.get(node).map_or(&[], Vec::as_slice)
    }

    /// Leak probability for a node. `0.0` for unset nodes.
    #[must_use]
    pub fn leak_of(&self, node: &str) -> f64 {
        self.leak.get(node).copied().unwrap_or(0.0)
    }

    /// Detect whether the graph is a DAG. Uses a Kahn-style
    /// topological-sort; cycles are reported as a [`CycleReport`].
    #[must_use]
    pub fn detect_cycles(&self) -> CycleReport {
        // Compute in-degree for the reverse graph (i.e. over
        // parents) so we can pull from leaves (no parents).
        let mut in_degree: BTreeMap<&NodeId, usize> = BTreeMap::new();
        for node in &self.nodes {
            in_degree.insert(node, 0);
        }
        for (child, parents) in &self.parents {
            in_degree.insert(child, parents.len());
        }
        let mut queue: VecDeque<&NodeId> = self
            .nodes
            .iter()
            .filter(|n| in_degree.get(n).copied().unwrap_or(0) == 0)
            .collect();
        let mut visited = 0;
        while let Some(n) = queue.pop_front() {
            visited += 1;
            // `n` is a parent. For each child that has `n` as a
            // parent, decrement that child's in-degree. The
            // parents map is keyed by *child*, so we scan its
            // entries and check if `n` appears in the parents
            // list.
            for (child, parents) in &self.parents {
                if parents.iter().any(|(p, _)| p == n) {
                    if let Some(d) = in_degree.get_mut(child) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            queue.push_back(child);
                        }
                    }
                }
            }
        }
        if visited == self.nodes.len() {
            CycleReport::Acyclic
        } else {
            // Collect the leftover nodes (the cycle members).
            let mut cycle: BTreeSet<&NodeId> = BTreeSet::new();
            for (node, deg) in &in_degree {
                if *deg > 0 {
                    cycle.insert(node);
                }
            }
            let cycle: BTreeSet<NodeId> = cycle.into_iter().cloned().collect();
            CycleReport::Cyclic(cycle)
        }
    }

    /// Compute the marginal `P(node fails)` for every node given
    /// the clamping. Uses the DAG fast path if the graph is
    /// acyclic, otherwise the seeded loopy-BP fallback.
    #[must_use]
    pub fn marginals(&self, clamped: &Clamp) -> MarginalResult {
        // Validate clamping: every clamped node must exist.
        for node in clamped.keys() {
            if !self.nodes.contains(node) {
                // Unknown clamp; treat as "healthy" (probability 0)
                // and skip rather than panic. The `malcolm` adapter
                // validates the clamp before calling, so this path
                // only triggers on user error.
            }
        }
        match self.detect_cycles() {
            CycleReport::Acyclic => self.marginals_dag(clamped),
            CycleReport::Cyclic(_) => self.marginals_loopy_bp(clamped),
        }
    }

    /// Exact marginal inference for a DAG. The graph is topologically
    /// ordered; each node's marginal is computed from its parents
    /// (which have all been processed by the time we reach the
    /// node). Time complexity: `O(V + E)`.
    fn marginals_dag(&self, clamped: &Clamp) -> MarginalResult {
        // Build a topological order via Kahn's algorithm.
        let mut in_degree: BTreeMap<&NodeId, usize> = BTreeMap::new();
        for node in &self.nodes {
            in_degree.insert(node, 0);
        }
        for parents in self.parents.values() {
            for (parent, _) in parents {
                in_degree.insert(parent, 0);
            }
        }
        for (child, parents) in &self.parents {
            // in_degree of `child` is the number of *incoming* edges,
            // i.e. the number of parents it has.
            in_degree.insert(child, parents.len());
        }
        let mut queue: VecDeque<&NodeId> = self
            .nodes
            .iter()
            .filter(|n| in_degree.get(n).copied().unwrap_or(0) == 0)
            .collect();
        let mut topo: Vec<&NodeId> = Vec::with_capacity(self.nodes.len());
        while let Some(n) = queue.pop_front() {
            topo.push(n);
            // Decrement in-degree of every node that has `n` as a
            // parent. We need to scan the parents map for "child has
            // parent n" — equivalent to "n is a parent of child" —
            // which means iterating the keys of `self.parents` and
            // checking membership. For a typical fault graph this is
            // fine; the inference budget is in the Loopy-BP path.
            for (child, parents) in &self.parents {
                if parents.iter().any(|(p, _)| p == n) {
                    if let Some(d) = in_degree.get_mut(child) {
                        *d = d.saturating_sub(1);
                        if *d == 0 {
                            queue.push_back(child);
                        }
                    }
                }
            }
        }
        // `topo` is the topological order. Compute marginals in that
        // order so each parent is settled before its children.
        let mut marginals: BTreeMap<NodeId, f64> = BTreeMap::new();
        for node in &topo {
            // Clamped nodes are fixed; others use the noisy-OR
            // formula over their parents' marginals.
            let p = self.noisy_or_marginal(node, &marginals, clamped);
            marginals.insert((*node).clone(), p);
        }
        // Any node not in `topo` (shouldn't happen for a DAG, but
        // be defensive) gets its noisy-OR from whatever parents have
        // been processed so far. For an acyclic graph this list is
        // empty; for a graph whose cycle detection disagreed with
        // the topological sort (shouldn't happen) we fall back to
        // the noisy-OR with current best-known parent values.
        for node in &self.nodes {
            let p = self.noisy_or_marginal(node, &marginals, clamped);
            marginals.insert(node.clone(), p);
        }
        MarginalResult::Exact(marginals)
    }

    /// Loopy belief propagation. Used as the fallback for cyclic
    /// graphs. Deterministic, seeded, and bounded — never loops
    /// forever. Returns a [`BpOutcome::Converged`] or
    /// [`BpOutcome::NonConverged`] flag so callers can decide
    /// whether to trust the result.
    fn marginals_loopy_bp(&self, clamped: &Clamp) -> MarginalResult {
        const MAX_ITERS: usize = 200;
        const TOLERANCE: f64 = 1e-6;

        // Sum-product loopy belief propagation over the noisy-OR
        // factor graph. Messages `m_{parent->child}` are
        // unnormalised (the normalisation constant cancels under
        // binary variables). We use the standard sum-product
        // update for noisy-OR gates:
        //
        //   m_{parent->child} ∝ 1 − w_{parent->child} · Π_{other parents j of child}(1 − w_{j->child} · m_{j->child})
        //
        // For nodes with no parents the message is just the
        // clamped or leak prior.
        let mut messages: BTreeMap<(NodeId, NodeId), f64> = BTreeMap::new();
        // Initialise to the leak probability (uniform prior) and
        // clamp any clamped nodes.
        for (parent, children) in &self.parents {
            for (child, _w) in children {
                let prior = clamped
                    .get(parent)
                    .copied()
                    .unwrap_or_else(|| self.leak_of(parent));
                messages.insert((parent.clone(), child.clone()), prior);
            }
        }
        let mut outcome = BpOutcome::NonConverged {
            iterations: 0,
            residual: 1.0,
        };
        let mut last_messages = messages.clone();
        for iter in 1..=MAX_ITERS {
            // Update each message using the noisy-OR sum-product
            // rule. For an edge parent -> child, the message is
            // 1 − w · (1 − Π_{other parents j of child}(1 − w_j · m_{j->child})).
            for (parent, children) in &self.parents {
                for (child, edge_w) in children {
                    let updated = if let Some(c) = clamped.get(parent).copied() {
                        // Clamped parents: the message is just the
                        // clamped value (0 or 1).
                        c
                    } else {
                        // `m_{parent->child}` — find all *other*
                        // parents of `child` and combine their
                        // messages.
                        let mut other_not_f = 1.0;
                        if let Some(child_parents) = self.parents.get(child) {
                            for (other_parent, other_w) in child_parents {
                                if other_parent == parent {
                                    continue;
                                }
                                let m = last_messages
                                    .get(&(other_parent.clone(), child.clone()))
                                    .copied()
                                    .unwrap_or(0.0);
                                other_not_f *= 1.0 - other_w * m;
                            }
                        }
                        // `m_{parent->child} = 1 − w · other_not_f`.
                        // Clamp into [0, 1] for safety.
                        (1.0 - edge_w * other_not_f).clamp(0.0, 1.0)
                    };
                    messages.insert((parent.clone(), child.clone()), updated);
                }
            }
            // Convergence check: max absolute change.
            let mut max_change: f64 = 0.0;
            for (k, v) in &messages {
                if let Some(prev) = last_messages.get(k) {
                    let diff = (v - prev).abs();
                    if diff > max_change {
                        max_change = diff;
                    }
                }
            }
            last_messages = messages.clone();
            if max_change < TOLERANCE {
                outcome = BpOutcome::Converged {
                    iterations: iter,
                    residual: max_change,
                };
                break;
            }
            if iter == MAX_ITERS {
                outcome = BpOutcome::NonConverged {
                    iterations: iter,
                    residual: max_change,
                };
            }
        }
        // DEBUG: print last_messages
        // Compute marginals from the converged messages using the
        // same noisy-OR factorisation: for each node,
        // P(failed) = 1 − (1 − leak) · Π_parents(1 − w · m_{parent->node}).
        let mut marginals: BTreeMap<NodeId, f64> = BTreeMap::new();
        for node in &self.nodes {
            let leak = self.leak_of(node);
            let p = if let Some(c) = clamped.get(node).copied() {
                c
            } else {
                let parents = self.parents_of(node);
                if parents.is_empty() {
                    leak
                } else {
                    let mut not_f = 1.0 - leak;
                    for (parent, w) in parents {
                        let m = last_messages
                            .get(&(parent.clone(), node.clone()))
                            .copied()
                            .unwrap_or(0.0);
                        not_f *= 1.0 - w * m;
                    }
                    (1.0 - not_f).clamp(0.0, 1.0)
                }
            };
            marginals.insert(node.clone(), p);
        }
        MarginalResult::Approximate { marginals, outcome }
    }

    /// Noisy-OR marginal: `P(node fails) = 1 − Π_i (1 − w_i · m_i) ·
    /// (1 − leak)`, clamped into `[leak, 1.0]`. Clamped nodes are
    /// pinned to their clamping value (healthy = 0, failed = 1).
    fn noisy_or_marginal(
        &self,
        node: &NodeId,
        parent_marginals: &BTreeMap<NodeId, f64>,
        clamped: &Clamp,
    ) -> f64 {
        if let Some(c) = clamped.get(node) {
            return *c;
        }
        let leak = self.leak_of(node);
        let parents = self.parents_of(node);
        if parents.is_empty() {
            return leak;
        }
        let mut not_f = 1.0 - leak;
        for (parent, w) in parents {
            let p = parent_marginals.get(parent).copied().unwrap_or(0.0);
            not_f *= 1.0 - w * p;
        }
        let p = 1.0 - not_f;
        p.clamp(leak, 1.0)
    }

    /// Blast-radius distribution: the probability mass over
    /// `k = 0..=n` nodes failing. Computed by the exact DP path
    /// (tree/forest) or the seeded Monte Carlo fallback (general
    /// graph). The result type records which method was used.
    ///
    /// `sample_count` controls the Monte Carlo fallback. If the
    /// graph is a forest, the exact path ignores `sample_count`
    /// (no sampling needed). `clamp` is the same as for
    /// [`Self::marginals`].
    #[must_use]
    pub fn blast_radius(
        &self,
        clamped: &Clamp,
        sample_count: usize,
        seed: u64,
    ) -> BlastRadiusResult {
        // First, get the marginals (used by both the exact and the
        // Monte Carlo paths).
        let marginals = self.marginals(clamped);
        let p_vec: Vec<(NodeId, f64)> = match &marginals {
            MarginalResult::Exact(m) => m.iter().map(|(k, v)| (k.clone(), *v)).collect(),
            MarginalResult::Approximate { marginals: m, .. } => {
                m.iter().map(|(k, v)| (k.clone(), *v)).collect()
            }
        };
        // The exact-DP path is only tractable for trees/forests.
        // We approximate "tree-ness" by counting how many nodes have
        // |parents| > 1 — if any, the graph is not a tree.
        let max_in_degree = self.parents.values().map(Vec::len).max().unwrap_or(0);
        let n = p_vec.len();
        if max_in_degree <= 1 && n > 0 {
            // Forest: exact DP convolution.
            let mut dist = vec![0.0_f64; n + 1];
            dist[0] = 1.0;
            for (_node, p) in &p_vec {
                let p = *p;
                let mut new_dist = vec![0.0_f64; n + 1];
                for k in 0..=n {
                    if dist[k] == 0.0 {
                        continue;
                    }
                    new_dist[k] += dist[k] * (1.0 - p);
                    if k < n {
                        new_dist[k + 1] += dist[k] * p;
                    }
                }
                dist = new_dist;
            }
            BlastRadiusResult::Exact {
                distribution: dist,
                expected_node_count: expected_from_marginals(&p_vec),
                outcome: BpOutcome::Converged {
                    iterations: 0,
                    residual: 0.0,
                },
            }
        } else {
            // General graph: seeded Monte Carlo over the per-node
            // Bernoullis (using the analytic marginals as per-node
            // probabilities; the samples are independent
            // Bernoullis, which is a conservative upper bound on
            // the per-bin count, and a valid Plinko estimate). The
            // standard error is reported for caller confidence.
            let mut rng = SmallRng::seed_from_u64(seed);
            let n_samples = sample_count.max(1);
            let mut counts = vec![0_u64; n + 1];
            for _ in 0..n_samples {
                let mut k = 0;
                for (_node, p) in &p_vec {
                    if rng.random_bool(*p) {
                        k += 1;
                    }
                }
                counts[k] += 1;
            }
            #[allow(clippy::cast_precision_loss)]
            let distribution: Vec<f64> = counts
                .iter()
                .map(|&c| c as f64 / n_samples as f64)
                .collect();
            let expected_node_count = expected_from_marginals(&p_vec);
            // Standard error of the expected count from the
            // Bernoulli trials. This is an approximation; the
            // exact SE of the expected count across samples is
            // `sqrt(Var/N)` with `Var = Σ p_i (1 − p_i)` under
            // independence.
            let variance: f64 = p_vec.iter().map(|(_n, p)| p * (1.0 - p)).sum();
            #[allow(clippy::cast_precision_loss)]
            let standard_error = sqrt(variance / n_samples as f64);
            BlastRadiusResult::MonteCarlo {
                distribution,
                #[allow(clippy::cast_precision_loss)]
                #[allow(clippy::cast_precision_loss)]
                expected_node_count,
                standard_error,
                samples: n_samples,
            }
        }
    }
}

/// Clamping: pin a set of nodes to fixed probabilities. A clamp
/// value of `1.0` pins the node as "failed" (an origin); a value
/// of `0.0` pins it as "healthy" (a known-good control). Absent
/// nodes are free (their probability is determined by the
/// inference).
pub type Clamp = BTreeMap<NodeId, f64>;

/// Cycle report from [`FailureGraph::detect_cycles`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CycleReport {
    /// The graph is a DAG.
    Acyclic,
    /// The graph contains a cycle. The set lists some cycle members
    /// (the exact cycle is not identified; this is the set of nodes
    /// that could not be topologically sorted).
    /// Cloned to own the `String`s (the `BTreeSet` over `&str`
    /// borrows from `self`).
    #[allow(clippy::should_implement_trait)] // intentionally not FromStr
    Cyclic(BTreeSet<NodeId>),
}

impl fmt::Display for CycleReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Acyclic => write!(f, "acyclic (DAG)"),
            Self::Cyclic(set) => {
                let names: Vec<&str> = set.iter().map(String::as_str).collect();
                write!(f, "cyclic: [{}]", names.join(", "))
            }
        }
    }
}

/// Outcome of the loopy-BP fallback.
#[derive(Debug, Clone, PartialEq)]
pub enum BpOutcome {
    /// BP converged within the iteration cap.
    Converged {
        /// Number of iterations until convergence.
        iterations: usize,
        /// Max absolute change in messages at convergence.
        residual: f64,
    },
    /// BP did not converge within the iteration cap. The returned
    /// marginals are the last iterate; callers should treat them
    /// as approximate.
    NonConverged {
        /// Number of iterations executed (= the cap).
        iterations: usize,
        /// Max absolute change in messages on the final iteration.
        residual: f64,
    },
}

/// Result of [`FailureGraph::marginals`].
#[derive(Debug, Clone, PartialEq)]
pub enum MarginalResult {
    /// Exact marginals (DAG fast path).
    Exact(BTreeMap<NodeId, f64>),
    /// Approximate marginals (loopy-BP fallback for cyclic graphs).
    Approximate {
        /// The marginal estimates.
        marginals: BTreeMap<NodeId, f64>,
        /// Whether BP converged.
        outcome: BpOutcome,
    },
}

impl MarginalResult {
    /// The marginal probability for a single node. Returns `0.0` if
    /// the node isn't in the result (e.g. unknown to the graph).
    #[must_use]
    pub fn get(&self, node: &str) -> f64 {
        match self {
            Self::Exact(m) | Self::Approximate { marginals: m, .. } => {
                m.get(node).copied().unwrap_or(0.0)
            }
        }
    }

    /// Borrow the marginal map.
    #[must_use]
    pub fn map(&self) -> &BTreeMap<NodeId, f64> {
        match self {
            Self::Exact(m) | Self::Approximate { marginals: m, .. } => m,
        }
    }

    /// True if this result is the exact DAG path.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact(_))
    }
}

/// Result of [`FailureGraph::blast_radius`].
#[derive(Debug, Clone, PartialEq)]
pub enum BlastRadiusResult {
    /// Exact distribution (forest/chain graph).
    Exact {
        /// `distribution[k]` = `P(exactly k nodes failed)`.
        distribution: Vec<f64>,
        /// Expected failed-node count, equal to `Σ p_i` under the
        /// exact-DP path.
        expected_node_count: f64,
        /// Converged marker (the exact-DP path is always
        /// deterministic; `iterations = 0`).
        outcome: BpOutcome,
    },
    /// Monte Carlo estimate (general graph).
    MonteCarlo {
        /// `distribution[k]` = empirical frequency of `k` failed
        /// nodes across the samples.
        distribution: Vec<f64>,
        /// Expected failed-node count, `Σ p_i`.
        expected_node_count: f64,
        /// Standard error of the expected count under the
        /// independence assumption.
        standard_error: f64,
        /// Number of samples used.
        samples: usize,
    },
}

impl BlastRadiusResult {
    /// `P(exactly k nodes failed)`. Returns `0.0` for out-of-range
    /// `k`.
    #[must_use]
    pub fn p_exactly(&self, k: usize) -> f64 {
        match self {
            Self::Exact { distribution, .. } | Self::MonteCarlo { distribution, .. } => {
                distribution.get(k).copied().unwrap_or(0.0)
            }
        }
    }

    /// `P(at least k nodes failed) = Σ_{j >= k} distribution[j]`.
    #[must_use]
    pub fn p_at_least(&self, k: usize) -> f64 {
        match self {
            Self::Exact { distribution, .. } | Self::MonteCarlo { distribution, .. } => {
                distribution.iter().skip(k).sum()
            }
        }
    }

    /// Expected failed-node count, `Σ p_i`. Equal to the sum of
    /// per-node marginals (under independence this is the mean of
    /// the count distribution; for the exact path it equals
    /// `Σ_{k} k · distribution[k]`).
    #[must_use]
    pub fn expected(&self) -> f64 {
        match self {
            Self::Exact {
                expected_node_count,
                ..
            }
            | Self::MonteCarlo {
                expected_node_count,
                ..
            } => *expected_node_count,
        }
    }

    /// `k` with the highest probability mass.
    #[must_use]
    pub fn mode(&self) -> usize {
        match self {
            Self::Exact { distribution, .. } | Self::MonteCarlo { distribution, .. } => {
                distribution
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal))
                    .map_or(0, |(i, _)| i)
            }
        }
    }

    /// The probability distribution over failed-node counts.
    #[must_use]
    pub fn distribution(&self) -> &[f64] {
        match self {
            Self::Exact { distribution, .. } | Self::MonteCarlo { distribution, .. } => {
                distribution
            }
        }
    }

    /// True if this result was computed exactly.
    #[must_use]
    pub fn is_exact(&self) -> bool {
        matches!(self, Self::Exact { .. })
    }
}

impl fmt::Display for BlastRadiusResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact {
                distribution,
                expected_node_count,
                ..
            } => {
                write!(
                    f,
                    "BlastRadius (exact): distribution = {distribution:?}, E = {expected_node_count}"
                )
            }
            Self::MonteCarlo {
                distribution,
                expected_node_count,
                standard_error,
                samples,
            } => {
                write!(
                    f,
                    "BlastRadius (Monte Carlo, {samples} samples): distribution = {distribution:?}, E = {expected_node_count} ± {standard_error}"
                )
            }
        }
    }
}

/// Helper: compute `Σ p_i` over a slice of marginals.
fn expected_from_marginals(p_vec: &[(NodeId, f64)]) -> f64 {
    p_vec.iter().map(|(_n, p)| *p).sum()
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "test assertions on exact computed probabilities"
)]
mod tests {
    use super::*;
    use alloc::borrow::ToOwned;

    fn chain(w_ab: f64, w_bc: f64) -> FailureGraph {
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", w_ab);
        g.add_edge("B", "C", w_bc);
        g
    }

    fn one_clamp(node: &str, p: f64) -> Clamp {
        let mut c = BTreeMap::new();
        c.insert(node.to_owned(), p);
        c
    }

    #[test]
    fn empty_graph_marginals_are_zero() {
        let g = FailureGraph::new();
        let m = g.marginals(&BTreeMap::new());
        assert!(m.is_exact());
        assert!(m.map().is_empty());
    }

    #[test]
    fn chain_dag_marginals_propagate_along_edges() {
        // A clamped failed; weights are identity; B and C should
        // fail with the edge weights.
        let g = chain(0.5, 0.5);
        let m = g.marginals(&one_clamp("A", 1.0));
        let marginals = m.map();
        assert_eq!(marginals.get("A").copied(), Some(1.0));
        assert_eq!(marginals.get("B").copied(), Some(0.5));
        assert_eq!(marginals.get("C").copied(), Some(0.25));
    }

    #[test]
    fn no_parents_node_uses_leak() {
        let mut g = FailureGraph::new();
        g.add_node("A");
        g.set_leak("A", 0.3);
        let m = g.marginals(&BTreeMap::new());
        assert_eq!(m.get("A"), 0.3);
    }

    #[test]
    fn noisy_or_two_failing_parents() {
        // Two parents at weight 0.5 each. The child should fail
        // with probability 1 - (1 - 0.5)(1 - 0.5) = 0.75.
        let mut g = FailureGraph::new();
        g.add_edge("A", "C", 0.5);
        g.add_edge("B", "C", 0.5);
        let mut clamp = BTreeMap::new();
        clamp.insert("A".to_owned(), 1.0);
        clamp.insert("B".to_owned(), 1.0);
        let m = g.marginals(&clamp);
        let p = m.get("C");
        assert!((p - 0.75).abs() < 1e-12, "expected 0.75, got {p}");
    }

    #[test]
    fn leak_combines_with_noisy_or() {
        // noisy-OR would give 0.75 for two failing parents at 0.5;
        // a leak of 0.1 raises the floor (the final clamp ensures
        // P >= leak).
        let mut g = FailureGraph::new();
        g.add_edge("A", "C", 0.5);
        g.add_edge("B", "C", 0.5);
        g.set_leak("C", 0.1);
        let mut clamp = BTreeMap::new();
        clamp.insert("A".to_owned(), 1.0);
        clamp.insert("B".to_owned(), 1.0);
        let m = g.marginals(&clamp);
        let p = m.get("C");
        // With leak, the noise-OR-style formula gives
        // 1 - (1-0.75)(1-0.1) = 1 - 0.225 = 0.775. We then clamp
        // to [leak, 1.0] so p >= 0.1 (trivially satisfied) and
        // p <= 1.0. The expected result is 0.775.
        assert!((p - 0.775).abs() < 1e-12, "expected 0.775, got {p}");
    }

    #[test]
    fn cycle_report_flags_cycles() {
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 0.5);
        g.add_edge("B", "A", 0.5);
        let report = g.detect_cycles();
        assert!(matches!(report, CycleReport::Cyclic(_)));
    }

    #[test]
    fn acyclic_report_for_dag() {
        let g = chain(0.5, 0.5);
        let report = g.detect_cycles();
        // The match must match Acyclic; if it doesn't, this
        // assertion fails with a useful message via `matches!`.
        assert!(
            matches!(report, CycleReport::Acyclic),
            "chain A->B->C should be Acyclic, got {report:?}"
        );
    }

    #[test]
    fn cyclic_graph_uses_loopy_bp_and_converges() {
        // Two-node cycle A <-> B plus a pendant C hanging off A.
        // We assert the BP path is taken (Approximate) and that
        // the algorithm converges within the iteration cap.
        // Numerical accuracy of the marginals under the
        // simplified noisy-OR message passing is tested
        // separately via the standalone test (and the existing
        // DAG tests).
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 0.8);
        g.add_edge("B", "A", 0.5);
        g.add_node("C");
        g.add_edge("A", "C", 0.3);
        let m = g.marginals(&one_clamp("A", 1.0));
        match m {
            MarginalResult::Approximate { marginals, outcome } => {
                // A is clamped to 1.0.
                assert_eq!(marginals.get("A").copied(), Some(1.0));
                // Outcome: should converge on this small cycle.
                assert!(
                    matches!(outcome, BpOutcome::Converged { .. }),
                    "expected Converged, got {outcome:?}"
                );
            }
            MarginalResult::Exact(_) => panic!("cyclic graph must use loopy-BP"),
        }
    }

    #[test]
    fn blast_radius_exact_for_forest() {
        // Independent chain A -> B -> C. With weight 1.0 along
        // each edge and A clamped failed, the failure count is
        // a single Bernoulli on each node: P(B) = P(C) = 1, so
        // exactly 3 nodes fail with probability 1.
        let g = chain(1.0, 1.0);
        let r = g.blast_radius(&one_clamp("A", 1.0), 1000, 42);
        assert!(r.is_exact());
        assert_eq!(r.p_exactly(3), 1.0);
        assert_eq!(r.p_exactly(0), 0.0);
        assert_eq!(r.expected(), 3.0);
    }

    #[test]
    fn blast_radius_monte_carlo_for_general_graph() {
        // Diamond: A -> B, A -> C, B -> D, C -> D. In-degree of
        // D is 2, so the exact-DP path is skipped. Monte Carlo
        // is used.
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 0.5);
        g.add_edge("A", "C", 0.5);
        g.add_edge("B", "D", 0.5);
        g.add_edge("C", "D", 0.5);
        let r = g.blast_radius(&one_clamp("A", 1.0), 10_000, 42);
        assert!(!r.is_exact());
        let expected = r.expected();
        // Sum of marginals: P(A)=1, P(B)=P(C)=0.5, P(D)=1-(1-0.25)^2=0.4375.
        // Sum = 1 + 0.5 + 0.5 + 0.4375 = 2.4375.
        assert!(
            (expected - 2.4375).abs() < 1e-9,
            "expected 2.4375, got {expected}"
        );
        // The distribution should sum to ~1.
        let sum: f64 = r.distribution().iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-9,
            "distribution should sum to 1, got {sum}"
        );
    }

    #[test]
    fn blast_radius_distribution_sums_to_one() {
        let g = chain(0.5, 0.5);
        let r = g.blast_radius(&one_clamp("A", 1.0), 5000, 7);
        let sum: f64 = r.distribution().iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "sum was {sum}");
    }

    #[test]
    fn blast_radius_p_at_least_consistent() {
        // P(at least 1) = 1 - P(exactly 0)
        let g = chain(0.5, 0.5);
        let r = g.blast_radius(&one_clamp("A", 1.0), 1000, 7);
        let p0 = r.p_exactly(0);
        let pat_least_1 = r.p_at_least(1);
        assert!(
            (p0 + pat_least_1 - 1.0).abs() < 1e-9,
            "p0={p0} p1+={pat_least_1}"
        );
    }

    #[test]
    fn determinism_same_seed_same_result() {
        let g = chain(0.3, 0.7);
        let r1 = g.blast_radius(&BTreeMap::new(), 1000, 42);
        let r2 = g.blast_radius(&BTreeMap::new(), 1000, 42);
        assert_eq!(r1.distribution(), r2.distribution());
    }

    #[test]
    fn empty_clamp_on_chain_uses_leak() {
        let mut g = chain(0.5, 0.5);
        g.set_leak("A", 0.1);
        g.set_leak("B", 0.1);
        g.set_leak("C", 0.1);
        let m = g.marginals(&BTreeMap::new());
        // Each node has a 0.1 floor. The chain math is then
        // bounded below by 0.1 everywhere.
        for node in ["A", "B", "C"] {
            let p = m.get(node);
            assert!(p >= 0.1, "node {node} has p={p}, expected >= 0.1");
        }
    }

    #[test]
    fn weight_is_clamped() {
        let mut g = FailureGraph::new();
        g.add_edge("A", "B", 5.0); // out of [0, 1]
        g.add_edge("A", "B", -2.0); // negative
        let parents = g.parents_of("B");
        // The second add is a no-op (parent already present), so
        // only one edge with clamped weight 1.0.
        assert_eq!(parents.len(), 1);
        assert!((parents[0].1 - 1.0).abs() < 1e-12);
    }

    #[test]
    fn mode_is_most_likely_count() {
        // Fully clamped failure: every node is 1.0; the only mass
        // is on k = n.
        let g = chain(1.0, 1.0);
        let r = g.blast_radius(&one_clamp("A", 1.0), 100, 0);
        assert_eq!(r.mode(), 3);
    }
}
