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

#[cfg(test)]
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
}
