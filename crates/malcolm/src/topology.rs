//! Named node graphs for cascade fault modeling.
//!
//! Provides a lightweight directed graph model and a cascade fault wrapper
//! that propagates faults across neighboring nodes probabilistically.
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::topology::{CascadeFault, Topology};
//! use malcolm::fault::{Fault, FaultContext};
//! use malcolm_core::bifurcation::BifurcationProfile;
//! use malcolm_core::types::FaultResult;
//!
//! let topology = Topology::builder()
//!     .name("cluster-a")
//!     .add_edge("node-a", "node-b", 1.0)
//!     .build();
//! let fault = CascadeFault::new(
//!     Box::new(PacketLoss::builder().seed(1).intensity(0.8).build()),
//!     topology,
//!     42,
//! );
//! let ctx = FaultContext {
//!     seed: 42,
//!     timestamp_ms: 0,
//!     node_id: "node-a".to_owned(),
//!     profile: BifurcationProfile::network_partition(),
//! };
//! assert!(matches!(fault.inject(&ctx), FaultResult::Injected(_)));
//! ```

use std::collections::{HashMap, HashSet, VecDeque};

use rand::RngExt as _;
use rand::SeedableRng as _;
use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use malcolm_core::inference::{BlastRadiusResult, Clamp, FailureGraph, MarginalResult};
use malcolm_core::types::{DryRunReport, FaultResult};

use crate::fault::{Fault, FaultContext};

/// One directed weighted edge in the topology graph.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    /// Destination node identifier.
    pub to: String,
    /// Propagation weight in `[0.0, 1.0]`.
    pub weight: f64,
}

/// Directed adjacency-list topology.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Topology {
    name: String,
    adjacency: HashMap<String, Vec<Edge>>,
}

impl Topology {
    /// Create an empty named topology.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            adjacency: HashMap::new(),
        }
    }

    /// Begin building a topology.
    #[must_use]
    pub fn builder() -> TopologyBuilder {
        TopologyBuilder::default()
    }

    /// Returns the topology name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Add one node if it does not exist.
    pub fn add_node(&mut self, node_id: impl Into<String>) {
        self.adjacency.entry(node_id.into()).or_default();
    }

    /// Add one directed edge with weight clamped into `[0.0, 1.0]`.
    pub fn add_edge(&mut self, from: impl Into<String>, to: impl Into<String>, weight: f64) {
        let from_node = from.into();
        let to_node = to.into();
        let clamped = weight.clamp(0.0, 1.0);

        self.adjacency.entry(to_node.clone()).or_default();
        self.adjacency.entry(from_node).or_default().push(Edge {
            to: to_node,
            weight: clamped,
        });
    }

    /// Get outgoing edges from one node.
    #[must_use]
    pub fn neighbors(&self, node_id: &str) -> Option<&[Edge]> {
        self.adjacency.get(node_id).map(Vec::as_slice)
    }

    /// Number of nodes in this topology.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.adjacency.len()
    }

    /// Return all node identifiers in sorted order.
    #[must_use]
    pub fn node_ids(&self) -> Vec<String> {
        let mut nodes: Vec<String> = self.adjacency.keys().cloned().collect();
        nodes.sort_unstable();
        nodes
    }

    /// Return all directed edges in deterministic sorted order.
    #[must_use]
    pub fn edges(&self) -> Vec<(String, String, f64)> {
        let mut all = Vec::new();
        for (from, edges) in &self.adjacency {
            for edge in edges {
                all.push((from.clone(), edge.to.clone(), edge.weight));
            }
        }
        all.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then_with(|| a.1.cmp(&b.1))
                .then_with(|| a.2.total_cmp(&b.2))
        });
        all
    }

    /// Build a `FailureGraph` from this topology. Every edge
    /// becomes a probabilistic edge with the same propagation
    /// weight; every node becomes a `FailureGraph` node. The
    /// returned graph has no leak probabilities (zero by
    /// default) — add them via
    /// [`FailureGraph::set_leak`] if your scenario has
    /// spontaneous failures.
    #[must_use]
    pub fn to_failure_graph(&self) -> FailureGraph {
        let mut g = FailureGraph::new();
        for (from, to, w) in self.edges() {
            g.add_edge(from, to, w);
        }
        g
    }

    /// Render this topology as a [Graphviz DOT](https://graphviz.org/doc/info/lang.html)
    /// directed graph. Edge labels carry the propagation weight so cascade
    /// configuration can be inspected visually with `dot -Tsvg`.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::topology::Topology;
    ///
    /// let topology = Topology::builder()
    ///     .name("cluster")
    ///     .add_edge("a", "b", 1.0)
    ///     .add_edge("b", "c", 0.5)
    ///     .build();
    /// let dot = topology.to_dot();
    /// assert!(dot.contains("digraph"));
    /// assert!(dot.contains("\"a\" -> \"b\""));
    /// assert!(dot.contains("\"b\" -> \"c\""));
    /// ```
    #[must_use]
    pub fn to_dot(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "digraph \"{label}\" {{",
            label = escape_dot(&self.name)
        );
        let _ = writeln!(out, "  rankdir=LR;");
        let _ = writeln!(
            out,
            "  node [shape=circle, style=filled, fillcolor=\"#dde6f3\"];"
        );
        for node in self.node_ids() {
            let _ = writeln!(out, "  \"{id}\";", id = escape_dot(&node));
        }
        for (from, to, weight) in self.edges() {
            let pct = (weight * 100.0).clamp(0.0, 100.0);
            let _ = writeln!(
                out,
                "  \"{from}\" -> \"{to}\" [label=\"{pct:.0}%\", weight=\"{weight:.3}\"];",
                from = escape_dot(&from),
                to = escape_dot(&to),
                pct = pct,
                weight = weight,
            );
        }
        let _ = writeln!(out, "}}");
        out
    }

    /// Render this topology as a [Mermaid](https://mermaid.js.org/) flowchart
    /// `graph TD` block. Suitable for embedding in markdown documentation and
    /// GitHub-flavored markdown viewers.
    ///
    /// # Example
    ///
    /// ```rust
    /// use malcolm::topology::Topology;
    ///
    /// let topology = Topology::builder()
    ///     .name("cluster")
    ///     .add_edge("a", "b", 1.0)
    ///     .build();
    /// let mermaid = topology.to_mermaid();
    /// assert!(mermaid.starts_with("graph TD"));
    /// assert!(mermaid.contains("a -->|100%| b"));
    /// ```
    #[must_use]
    pub fn to_mermaid(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::from("graph TD\n");
        for node in self.node_ids() {
            let _ = writeln!(out, "  {id}[\"{id}\"]", id = escape_mermaid(&node));
        }
        for (from, to, weight) in self.edges() {
            let pct = (weight * 100.0).clamp(0.0, 100.0);
            let _ = writeln!(
                out,
                "  {from} -->|{pct:.0}%| {to}",
                from = escape_mermaid(&from),
                to = escape_mermaid(&to),
            );
        }
        out
    }
}

fn escape_dot(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn escape_mermaid(s: &str) -> String {
    // Mermaid node ids permit alphanumerics and underscores; replace anything
    // else with an underscore to keep the chart well-formed.
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

/// Builder for [`Topology`].
#[derive(Default)]
pub struct TopologyBuilder {
    name: Option<String>,
    nodes: HashSet<String>,
    edges: Vec<(String, String, f64)>,
}

impl TopologyBuilder {
    /// Set topology name.
    #[must_use]
    pub fn name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Add one node.
    #[must_use]
    pub fn add_node(mut self, node_id: impl Into<String>) -> Self {
        self.nodes.insert(node_id.into());
        self
    }

    /// Add one directed edge with a weighted propagation probability.
    #[must_use]
    pub fn add_edge(mut self, from: impl Into<String>, to: impl Into<String>, weight: f64) -> Self {
        let from_node = from.into();
        let to_node = to.into();
        self.nodes.insert(from_node.clone());
        self.nodes.insert(to_node.clone());
        self.edges.push((from_node, to_node, weight));
        self
    }

    /// Build the final topology.
    #[must_use]
    pub fn build(self) -> Topology {
        let mut topology =
            Topology::named(self.name.unwrap_or_else(|| "default-topology".to_owned()));
        for node in self.nodes {
            topology.add_node(node);
        }
        for (from, to, weight) in self.edges {
            topology.add_edge(from, to, weight);
        }
        topology
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PropagationHop {
    from: String,
    to: String,
    probability: f64,
    injected: bool,
}

/// Wraps another fault and propagates it across topology edges.
pub struct CascadeFault {
    inner: Box<dyn Fault>,
    topology: Topology,
    seed: u64,
    max_hops: usize,
}

impl CascadeFault {
    /// Create a new cascade fault wrapper.
    #[must_use]
    pub fn new(inner: Box<dyn Fault>, topology: Topology, seed: u64) -> Self {
        Self {
            inner,
            topology,
            seed,
            max_hops: usize::MAX,
        }
    }

    /// Limit cascade traversal depth.
    #[must_use]
    pub const fn with_max_hops(mut self, max_hops: usize) -> Self {
        self.max_hops = max_hops;
        self
    }

    /// Compute deterministic propagation path from a source node.
    #[must_use]
    pub fn propagation_path(&self, source: &str, intensity: f64) -> Vec<String> {
        self.propagation_hops(source, intensity)
            .into_iter()
            .filter(|hop| hop.injected)
            .fold(vec![source.to_owned()], |mut acc, hop| {
                if !acc.iter().any(|n| n == &hop.to) {
                    acc.push(hop.to);
                }
                acc
            })
    }

    fn propagation_hops(&self, source: &str, intensity: f64) -> Vec<PropagationHop> {
        let mut hops = Vec::new();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        let mut rng = SmallRng::seed_from_u64(self.seed);

        visited.insert(source.to_owned());
        queue.push_back((source.to_owned(), 0_usize));

        while let Some((current, depth)) = queue.pop_front() {
            if depth >= self.max_hops {
                continue;
            }

            if let Some(edges) = self.topology.neighbors(&current) {
                for edge in edges {
                    let probability = (edge.weight * intensity).clamp(0.0, 1.0);
                    let injected = rng.random::<f64>() < probability;

                    hops.push(PropagationHop {
                        from: current.clone(),
                        to: edge.to.clone(),
                        probability,
                        injected,
                    });

                    if injected && visited.insert(edge.to.clone()) {
                        queue.push_back((edge.to.clone(), depth + 1));
                    }
                }
            }
        }

        hops
    }
}

impl Fault for CascadeFault {
    fn inject(&self, ctx: &FaultContext) -> FaultResult {
        let result = self.inner.inject(ctx);

        let intensity = match &result {
            FaultResult::Injected(event) => event.intensity,
            FaultResult::Skipped(_) => 0.0,
        };

        for hop in self.propagation_hops(&ctx.node_id, intensity) {
            tracing::info!(
                target: "malcolm",
                fault_type = "cascade_fault",
                from_node = %hop.from,
                to_node = %hop.to,
                propagation_probability = hop.probability,
                injected = hop.injected,
                dry_run = false,
                "cascade propagation hop",
            );
        }

        result
    }

    fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
        let inner = self.inner.dry_run(ctx);
        let reason = format!(
            "cascade dry-run on topology '{}' for node {} (inner: {})",
            self.topology.name(),
            ctx.node_id,
            inner.reason,
        );

        tracing::debug!(
            target: "malcolm",
            fault_type = "cascade_fault",
            node_id = %ctx.node_id,
            topology = %self.topology.name(),
            dry_run = true,
            "cascade dry-run",
        );

        DryRunReport {
            fault_type: self.fault_type().to_owned(),
            node_id: ctx.node_id.clone(),
            would_inject: inner.would_inject,
            reason,
        }
    }

    fn fault_type(&self) -> &'static str {
        "cascade_fault"
    }
}

/// Bayesian cascade: the **analytic companion** to [`CascadeFault`].
///
/// [`CascadeFault`] samples one forward Plinko bounce per call. This
/// type instead computes the *distribution* of outcomes analytically
/// from the same graph — marginal failure probabilities per node and
/// the blast-radius distribution — without running many cascades.
///
/// # Usage
///
/// Construct a [`BayesianCascade`] from a [`Topology`], then call
/// [`BayesianCascade::analyse`] with a set of origin nodes clamped
/// to "failed" to get the per-node failure marginals and the
/// blast-radius distribution. The result is a `tracing` event
/// (`fault_type = "bayesian_cascade"`) plus the raw numbers, suitable
/// for a chaos experiment's pre-flight estimate or a post-mortem
/// blast-radius report.
///
/// # Determinism
///
/// Exact paths (DAGs) are deterministic. Monte Carlo fallbacks
/// (general graphs) take an explicit `sample_count` and `seed` so
/// the result is replayable. There is no unseeded RNG anywhere in
/// the call path.
///
/// # Where the math lives
///
/// All the inference math — noisy-OR marginals, cycle detection,
/// exact-DP blast-radius, loopy-BP fallback — is in
/// `malcolm_core::inference`. This type is the thin
/// `Topology` ↔ `FailureGraph` adapter plus the tracing event.
pub struct BayesianCascade {
    graph: FailureGraph,
}

impl BayesianCascade {
    /// Adapter kind for tracing events and any future
    /// `Fault`-like trait integration.
    pub const KIND: &'static str = "bayesian_cascade";

    /// Build a Bayesian cascade from an existing [`Topology`].
    /// The mapping is direct: every edge in the topology becomes
    /// an edge in the failure graph with the same propagation
    /// weight.
    #[must_use]
    pub fn from_topology(topology: &Topology) -> Self {
        Self {
            graph: topology.to_failure_graph(),
        }
    }

    /// Build a Bayesian cascade from an existing [`FailureGraph`]
    /// directly. Useful for tests and for callers who want to
    /// inject leak probabilities or non-topology sources.
    #[must_use]
    pub fn from_graph(graph: FailureGraph) -> Self {
        Self { graph }
    }

    /// Borrow the underlying failure graph.
    #[must_use]
    pub fn graph(&self) -> &FailureGraph {
        &self.graph
    }

    /// Run the inference engine: compute per-node failure
    /// marginals and the blast-radius distribution for the given
    /// origin clamp. Emits a `tracing` event
    /// (`fault_type = "bayesian_cascade"`) with the summary
    /// numbers, then returns the raw marginals and blast-radius
    /// result for further inspection.
    #[must_use]
    pub fn analyse(&self, origins: &Clamp, sample_count: usize, seed: u64) -> BayesianReport {
        let marginals = self.graph.marginals(origins);
        let blast_radius = self.graph.blast_radius(origins, sample_count, seed);

        // Per-node marginals as a `BTreeMap<String, f64>` (in
        // sorted node order for determinism). Keys are owned
        // `String`s so the map can be moved out of `&self`.
        let per_node: std::collections::BTreeMap<String, f64> = self
            .graph
            .nodes()
            .iter()
            .map(|n| (n.clone(), marginals.get(n)))
            .collect();

        // Emit the tracing event. This is the standard
        // `tracing::info!` shape used elsewhere in this crate
        // (T14 schema).
        tracing::info!(
            target: "malcolm",
            fault_type = Self::KIND,
            node_count = self.graph.node_count(),
            is_exact_marginals = marginals.is_exact(),
            is_exact_blast_radius = blast_radius.is_exact(),
            expected_failures = blast_radius.expected(),
            mode_failures = blast_radius.mode(),
            "bayesian cascade analysis",
        );

        BayesianReport {
            marginals,
            blast_radius,
            per_node,
        }
    }
}

/// The result of a [`BayesianCascade::analyse`] call: the raw
/// marginals, the blast-radius distribution, and a per-node
/// summary for ergonomic access.
#[derive(Debug, Clone)]
pub struct BayesianReport {
    /// The marginal probabilities (exact or approximate,
    /// depending on the graph structure). Use
    /// [`BayesianReport::is_exact`] to check.
    pub marginals: MarginalResult,
    /// The blast-radius distribution. Use
    /// [`BayesianReport::is_exact_blast_radius`] to check.
    pub blast_radius: BlastRadiusResult,
    /// Per-node failure probabilities, indexed by node id. The
    /// underlying map is sorted for determinism. This is a
    /// convenience view over `marginals.map()`; both stay in
    /// sync.
    pub per_node: std::collections::BTreeMap<String, f64>,
}

impl BayesianReport {
    /// True if the marginals were computed exactly (DAG fast path).
    #[must_use]
    pub fn is_exact(&self) -> bool {
        self.marginals.is_exact()
    }

    /// True if the blast-radius distribution was computed exactly.
    #[must_use]
    pub fn is_exact_blast_radius(&self) -> bool {
        self.blast_radius.is_exact()
    }

    /// Per-node failure probability, or `0.0` if the node is
    /// unknown to the graph.
    #[must_use]
    pub fn node_marginal(&self, node: &str) -> f64 {
        self.marginals.get(node)
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "test assertions on exact computed probabilities"
)]
mod tests {
    use tracing_test::traced_test;

    use super::*;
    use malcolm_core::bifurcation::BifurcationProfile;
    use malcolm_core::types::{FaultEvent, SkipReason};

    struct FixedIntensityFault {
        intensity: f64,
    }

    impl Fault for FixedIntensityFault {
        fn inject(&self, ctx: &FaultContext) -> FaultResult {
            FaultResult::Injected(FaultEvent {
                fault_type: self.fault_type().to_owned(),
                node_id: ctx.node_id.clone(),
                seed: ctx.seed,
                intensity: self.intensity,
                dry_run: false,
                timestamp_ms: ctx.timestamp_ms,
            })
        }

        fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
            DryRunReport {
                fault_type: self.fault_type().to_owned(),
                node_id: ctx.node_id.clone(),
                would_inject: true,
                reason: "fixed intensity dry-run".to_owned(),
            }
        }

        fn fault_type(&self) -> &'static str {
            "fixed_intensity"
        }
    }

    fn ctx(node_id: &str) -> FaultContext {
        FaultContext {
            seed: 1,
            timestamp_ms: 0,
            node_id: node_id.to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    fn linear_chain_weight_one_propagates_all_nodes() {
        let topology = Topology::builder()
            .name("chain")
            .add_edge("a", "b", 1.0)
            .add_edge("b", "c", 1.0)
            .build();
        let fault = CascadeFault::new(
            Box::new(FixedIntensityFault { intensity: 1.0 }),
            topology,
            42,
        );

        let nodes = fault.propagation_path("a", 1.0);

        assert_eq!(nodes, vec!["a", "b", "c"]);
    }

    #[test]
    fn weight_zero_never_propagates_beyond_source() {
        let topology = Topology::builder()
            .name("zero")
            .add_edge("a", "b", 0.0)
            .add_edge("b", "c", 0.0)
            .build();
        let fault = CascadeFault::new(
            Box::new(FixedIntensityFault { intensity: 1.0 }),
            topology,
            7,
        );

        let nodes = fault.propagation_path("a", 1.0);

        assert_eq!(nodes, vec!["a"]);
    }

    #[test]
    fn weight_half_converges_near_fifty_percent() {
        let topology = Topology::builder()
            .name("half")
            .add_edge("a", "b", 0.5)
            .build();

        let mut injected_count = 0usize;
        for seed in 0..1_000_u64 {
            let fault = CascadeFault::new(
                Box::new(FixedIntensityFault { intensity: 1.0 }),
                topology.clone(),
                seed,
            );
            let nodes = fault.propagation_path("a", 1.0);
            if nodes.iter().any(|node| node == "b") {
                injected_count = injected_count.saturating_add(1);
            }
        }

        assert!(
            (450..=550).contains(&injected_count),
            "expected ~50% propagation to b, got {injected_count}/1000"
        );
    }

    #[test]
    fn propagation_path_is_deterministic_for_seed() {
        let topology = Topology::builder()
            .name("det")
            .add_edge("a", "b", 0.5)
            .add_edge("b", "c", 0.5)
            .build();

        let fault_a = CascadeFault::new(
            Box::new(FixedIntensityFault { intensity: 1.0 }),
            topology.clone(),
            123,
        );
        let fault_b = CascadeFault::new(
            Box::new(FixedIntensityFault { intensity: 1.0 }),
            topology,
            123,
        );

        assert_eq!(
            fault_a.propagation_path("a", 1.0),
            fault_b.propagation_path("a", 1.0)
        );
    }

    #[test]
    #[traced_test]
    fn cascade_inject_emits_per_hop_events() {
        let topology = Topology::builder()
            .name("hop")
            .add_edge("a", "b", 1.0)
            .build();
        let fault = CascadeFault::new(
            Box::new(FixedIntensityFault { intensity: 1.0 }),
            topology,
            99,
        );

        let result = fault.inject(&ctx("a"));

        assert!(matches!(result, FaultResult::Injected(_)));
        assert!(logs_contain("cascade propagation hop"));
    }

    #[test]
    fn cascade_passes_through_skip_result() {
        struct SkipFault;

        impl Fault for SkipFault {
            fn inject(&self, _ctx: &FaultContext) -> FaultResult {
                FaultResult::Skipped(SkipReason::BelowThreshold)
            }

            fn dry_run(&self, ctx: &FaultContext) -> DryRunReport {
                DryRunReport {
                    fault_type: self.fault_type().to_owned(),
                    node_id: ctx.node_id.clone(),
                    would_inject: false,
                    reason: "below threshold".to_owned(),
                }
            }

            fn fault_type(&self) -> &'static str {
                "skip_fault"
            }
        }

        let topology = Topology::builder()
            .name("skip")
            .add_edge("a", "b", 1.0)
            .build();
        let cascade = CascadeFault::new(Box::new(SkipFault), topology, 1);
        let result = cascade.inject(&ctx("a"));

        assert!(matches!(
            result,
            FaultResult::Skipped(SkipReason::BelowThreshold)
        ));
    }

    #[test]
    fn to_dot_includes_header_nodes_and_weighted_edges() {
        let topology = Topology::builder()
            .name("cluster-a")
            .add_edge("a", "b", 1.0)
            .add_edge("b", "c", 0.25)
            .build();

        let dot = topology.to_dot();
        assert!(dot.starts_with("digraph \"cluster-a\" {"));
        assert!(dot.contains("\"a\";"));
        assert!(dot.contains("\"b\";"));
        assert!(dot.contains("\"c\";"));
        assert!(dot.contains("\"a\" -> \"b\" [label=\"100%\", weight=\"1.000\"]"));
        assert!(dot.contains("\"b\" -> \"c\" [label=\"25%\", weight=\"0.250\"]"));
        assert!(dot.trim_end().ends_with('}'));
    }

    #[test]
    fn to_dot_escapes_quotes_in_topology_name() {
        let topology = Topology::builder()
            .name("a\"b")
            .add_edge("a", "b", 0.5)
            .build();
        let dot = topology.to_dot();
        assert!(dot.contains("digraph \"a\\\"b\""));
    }

    #[test]
    fn to_mermaid_emits_graph_td_block() {
        let topology = Topology::builder()
            .name("cluster")
            .add_edge("a", "b", 1.0)
            .add_edge("b", "c", 0.5)
            .build();

        let mermaid = topology.to_mermaid();
        assert!(mermaid.starts_with("graph TD\n"));
        assert!(mermaid.contains("a[\"a\"]"));
        assert!(mermaid.contains("b[\"b\"]"));
        assert!(mermaid.contains("c[\"c\"]"));
        assert!(mermaid.contains("a -->|100%| b"));
        assert!(mermaid.contains("b -->|50%| c"));
    }

    #[test]
    fn to_mermaid_sanitises_node_ids() {
        let topology = Topology::builder()
            .name("n")
            .add_edge("a-b", "c.d", 0.5)
            .build();
        let mermaid = topology.to_mermaid();
        // Hyphens and dots must be replaced with underscores.
        assert!(mermaid.contains("a_b"));
        assert!(mermaid.contains("c_d"));
    }

    #[test]
    fn bayesian_cascade_from_topology_computes_marginals() {
        // Chain A -> B -> C, weights 1.0, 1.0; A clamped to
        // failed. All three nodes should fail with probability 1.
        let topology = Topology::builder()
            .name("chain")
            .add_edge("A", "B", 1.0)
            .add_edge("B", "C", 1.0)
            .build();
        let cascade = BayesianCascade::from_topology(&topology);
        let mut origins = Clamp::new();
        origins.insert("A".to_owned(), 1.0);
        let report = cascade.analyse(&origins, 1_000, 42);
        assert!(report.is_exact(), "chain is a DAG; expect exact");
        assert_eq!(report.node_marginal("A"), 1.0);
        assert_eq!(report.node_marginal("B"), 1.0);
        assert_eq!(report.node_marginal("C"), 1.0);
        // Blast-radius: exactly 3 nodes failed with probability 1.
        assert!(report.is_exact_blast_radius());
        assert_eq!(report.blast_radius.p_exactly(3), 1.0);
        assert_eq!(report.blast_radius.p_exactly(0), 0.0);
    }

    #[test]
    fn bayesian_cascade_noisy_or_two_parents() {
        // Two parents at weight 0.5; child should be 0.75.
        let topology = Topology::builder()
            .name("diamond")
            .add_edge("A", "C", 0.5)
            .add_edge("B", "C", 0.5)
            .build();
        let cascade = BayesianCascade::from_topology(&topology);
        let mut origins = Clamp::new();
        origins.insert("A".to_owned(), 1.0);
        origins.insert("B".to_owned(), 1.0);
        let report = cascade.analyse(&origins, 100, 0);
        assert!(report.is_exact(), "noisy-OR on a tree is exact");
        assert!(
            (report.node_marginal("C") - 0.75).abs() < 1e-12,
            "expected P(C) = 0.75, got {}",
            report.node_marginal("C")
        );
    }

    #[test]
    fn bayesian_cascade_monte_carlo_for_general_graph() {
        // Diamond A -> B, A -> C, B -> D, C -> D. In-degree of
        // D is 2, so the exact-DP path is skipped. Monte Carlo
        // is used.
        let topology = Topology::builder()
            .name("diamond")
            .add_edge("A", "B", 0.5)
            .add_edge("A", "C", 0.5)
            .add_edge("B", "D", 0.5)
            .add_edge("C", "D", 0.5)
            .build();
        let cascade = BayesianCascade::from_topology(&topology);
        let mut origins = Clamp::new();
        origins.insert("A".to_owned(), 1.0);
        let report = cascade.analyse(&origins, 5_000, 42);
        assert!(!report.is_exact_blast_radius());
        // Sum of marginals: 1 + 0.5 + 0.5 + 0.4375 = 2.4375.
        let expected = report.blast_radius.expected();
        assert!(
            (expected - 2.4375).abs() < 1e-9,
            "expected 2.4375, got {expected}"
        );
        // Distribution sums to ~1.
        let sum: f64 = report.blast_radius.distribution().iter().sum();
        assert!((sum - 1.0).abs() < 1e-9, "distribution sum = {sum}");
    }

    #[test]
    fn bayesian_cascade_determinism_same_seed_same_result() {
        let topology = Topology::builder()
            .name("chain")
            .add_edge("A", "B", 0.3)
            .add_edge("B", "C", 0.7)
            .build();
        let cascade1 = BayesianCascade::from_topology(&topology);
        let cascade2 = BayesianCascade::from_topology(&topology);
        let r1 = cascade1.analyse(&Clamp::new(), 1_000, 42);
        let r2 = cascade2.analyse(&Clamp::new(), 1_000, 42);
        assert_eq!(
            r1.blast_radius.distribution(),
            r2.blast_radius.distribution()
        );
    }
}
