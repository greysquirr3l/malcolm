//! Declarative helpers for building [`crate::scenario::ChaosScenario`] values inline.
//!
//! The `malcolm!` macro keeps test code short when the full builder chain is
//! distracting. It expands to the same scenario builder API used elsewhere in
//! the crate.
//!
//! # Example
//!
//! ```rust
//! use malcolm::faults::network::PacketLoss;
//! use malcolm::malcolm;
//! use malcolm::scenario::ScenarioRegime;
//! use malcolm_core::bifurcation::BifurcationProfile;
//!
//! let scenario = malcolm! {
//!     name: "inline-demo",
//!     seed: 7,
//!     profile: BifurcationProfile::network_partition(),
//!     faults: [
//!         PacketLoss::builder().seed(11).intensity(0.8).build(),
//!     ],
//! };
//!
//! assert_eq!(scenario.name(), "inline-demo");
//! assert_eq!(scenario.seed(), 7);
//! assert_eq!(scenario.profile().label, "network_partition");
//! let _ = ScenarioRegime::Stable;
//! ```

#[macro_export]
/// Build a `ChaosScenario` inline from a compact, declarative literal.
///
/// The `malcolm!` macro expands to the same builder chain used by the
/// `ChaosScenario::builder()` API, so its behaviour is identical and stays in
/// sync with the rest of the crate. Use it in tests or in short operator
/// scripts where the full builder chain is distracting.
///
/// Two forms are supported: one without a topology and one with an attached
/// `Topology`. See the module-level documentation for an example.
macro_rules! malcolm {
    (
        name: $name:expr,
        seed: $seed:expr,
        profile: $profile:expr,
        faults: [$( $fault:expr ),* $(,)?] $(,)?
    ) => {
        $crate::malcolm!(@build
            name: $name,
            seed: $seed,
            profile: $profile,
            topology: None,
            faults: [$( $fault ),*]
        )
    };
    (
        name: $name:expr,
        seed: $seed:expr,
        profile: $profile:expr,
        topology: $topology:expr,
        faults: [$( $fault:expr ),* $(,)?] $(,)?
    ) => {
        $crate::malcolm!(@build
            name: $name,
            seed: $seed,
            profile: $profile,
            topology: Some($topology),
            faults: [$( $fault ),*]
        )
    };
    (@build
        name: $name:expr,
        seed: $seed:expr,
        profile: $profile:expr,
        topology: $topology:expr,
        faults: [$( $fault:expr ),*]
    ) => {{
        let builder = $crate::scenario::ChaosScenario::builder()
            .name($name)
            .seed($seed)
            $(.add_fault($fault))*
            .profile($profile);

        let builder = if let Some(topology) = $topology {
            builder.topology(topology)
        } else {
            builder
        };

        builder.build()
    }};
}

#[cfg(test)]
mod tests {
    use crate::fault::FaultContext;
    use crate::faults::network::PacketLoss;
    use crate::topology::Topology;
    use malcolm_core::bifurcation::BifurcationProfile;

    fn make_ctx(seed: u64, node_id: &str) -> FaultContext {
        FaultContext {
            seed,
            timestamp_ms: 0,
            node_id: node_id.to_owned(),
            profile: BifurcationProfile::network_partition(),
        }
    }

    #[test]
    fn builds_inline_scenario_without_topology() {
        let scenario = crate::malcolm! {
            name: "macro-demo",
            seed: 19,
            profile: BifurcationProfile::network_partition(),
            faults: [
                PacketLoss::builder().seed(3).intensity(0.7).build(),
            ],
        };

        assert_eq!(scenario.name(), "macro-demo");
        assert_eq!(scenario.seed(), 19);
        assert_eq!(scenario.profile().label, "network_partition");
        assert!(scenario.topology().is_none());

        let mut ctx = make_ctx(19, "node-a");
        let report = scenario.run(&mut ctx);
        assert_eq!(report.name, "macro-demo");
        assert_eq!(report.events.len(), 1);
    }

    #[test]
    fn builds_inline_scenario_with_topology() {
        let topology = Topology::builder()
            .name("cluster")
            .add_edge("a", "b", 1.0)
            .build();

        let scenario = crate::malcolm! {
            name: "macro-topology",
            seed: 23,
            profile: BifurcationProfile::network_partition(),
            topology: topology,
            faults: [],
        };

        assert_eq!(scenario.name(), "macro-topology");
        assert_eq!(scenario.seed(), 23);
        let Some(attached_topology) = scenario.topology() else {
            return;
        };
        assert_eq!(attached_topology.name(), "cluster");

        let mut ctx = make_ctx(23, "a");
        let report = scenario.run(&mut ctx);
        assert!(report.events.is_empty());
    }
}
