//! Integration tests for the tc/netem adapter. Compiled only
//! on Linux with the `netem` feature enabled.
//!
//! Most `tc` operations need `CAP_NET_ADMIN`. The tests probe
//! the host with [`probe_netem_writable`] and skip cleanly when
//! the runner lacks privilege — `eprintln!` a skip line so the
//! absence is visible in CI logs.

#![cfg(all(target_os = "linux", feature = "netem"))]

use std::process::Command;
use std::sync::Arc;

use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
use malcolm_agent::adapters::netem::{NetemAction, NetemAdapter};
use malcolm_agent::cleanup::Cleanup;
use malcolm_agent::safety::SafetyGuard;

#[expect(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Probe whether the host can run `tc qdisc add`. Returns
    /// `true` only when `tc` is on `$PATH` AND the call
    /// succeeds with a no-op netem qdisc.
    #[must_use]
    pub fn probe_netem_writable() -> bool {
        // Create a throwaway veth pair if possible; if not,
        // probe a synthetic qdisc on lo (always allowed).
        let probe_iface = probe_iface();
        let output = Command::new("tc")
            .args([
                "qdisc",
                "add",
                "dev",
                &probe_iface,
                "root",
                "netem",
                "loss",
                "0%",
            ])
            .output();
        if let Ok(out) = output {
            if out.status.success() {
                // Clean up the probe.
                let _ = Command::new("tc")
                    .args(["qdisc", "del", "dev", &probe_iface, "root"])
                    .output();
                return true;
            }
        }
        false
    }

    /// Find a network interface we can probe. Prefers `lo`,
    /// falls back to any interface we can find. The actual
    /// production tests should use a veth pair; the probe
    /// only needs a writable iface.
    fn probe_iface() -> String {
        "lo".to_owned()
    }

    fn armed_guard_with_ifaces(ifaces: &[&str]) -> SafetyGuard {
        let mut guard = SafetyGuard::new();
        for i in ifaces {
            guard.allow_iface(*i);
        }
        guard.arm_for_test(true).expect("arm_for_test")
    }

    /// Test the `NetemAction::from_payload` decoder and the
    /// parameter validation in isolation. Does not touch the
    /// host.
    #[test]
    fn from_payload_round_trip_for_each_action_kind() {
        let cases = vec![
            (
                serde_json::json!({
                    "kind": "latency",
                    "mean_ms": 100u64,
                    "jitter_ms": 20u64,
                    "correlation": 25.0,
                }),
                NetemAction::Latency {
                    mean: std::time::Duration::from_millis(100),
                    jitter: Some(std::time::Duration::from_millis(20)),
                    correlation: Some(25.0),
                },
            ),
            (
                serde_json::json!({
                    "kind": "loss",
                    "percent": 5.0,
                    "correlation": 50.0,
                }),
                NetemAction::Loss {
                    percent: 5.0,
                    correlation: Some(50.0),
                },
            ),
            (
                serde_json::json!({"kind": "corrupt", "percent": 0.5}),
                NetemAction::Corrupt { percent: 0.5 },
            ),
            (
                serde_json::json!({
                    "kind": "reorder",
                    "percent": 2.0,
                    "correlation": 25.0,
                }),
                NetemAction::Reorder {
                    percent: 2.0,
                    correlation: Some(25.0),
                },
            ),
            (
                serde_json::json!({"kind": "rate", "bps": 1_000_000u64}),
                NetemAction::Rate { bps: 1_000_000 },
            ),
            (
                serde_json::json!({"kind": "partition"}),
                NetemAction::Partition,
            ),
        ];
        for (payload, expected) in cases {
            let parsed = NetemAction::from_payload(&payload).expect("payload must parse");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn invalid_plan_payload_returns_invalid_plan_error() {
        let mut guard = SafetyGuard::new();
        guard.allow_iface("lo");
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = NetemAdapter::new();

        // Payload is not a JSON object.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!("not an object"),
                    reason: "shape test".to_owned(),
                },
                &guard,
            )
            .expect_err("non-object payload must error");
        assert!(
            matches!(err, malcolm_agent::error::AgentError::InvalidPlan { .. }),
            "expected InvalidPlan, got {err:?}"
        );

        // Missing interface.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({"kind": "latency", "mean_ms": 100u64}),
                    reason: "missing iface".to_owned(),
                },
                &guard,
            )
            .expect_err("missing interface must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));

        // Unknown kind.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "explode",
                        "interface": "lo",
                    }),
                    reason: "unknown kind".to_owned(),
                },
                &guard,
            )
            .expect_err("unknown kind must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));

        // Out-of-range percent.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "loss",
                        "interface": "lo",
                        "percent": 150.0,
                    }),
                    reason: "bad percent".to_owned(),
                },
                &guard,
            )
            .expect_err("loss > 100 must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));

        // NaN percent.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "corrupt",
                        "interface": "lo",
                        "percent": f64::NAN,
                    }),
                    reason: "nan percent".to_owned(),
                },
                &guard,
            )
            .expect_err("NaN percent must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));
    }

    #[test]
    fn unarmed_guard_makes_netem_adapter_dry_run() {
        // No `arm_for_test`; the adapter's `is_armed()` check
        // produces the dry-run. The dry-run path must not
        // touch the host even if `tc` is available.
        let guard = SafetyGuard::new();
        let adapter = NetemAdapter::new();
        let applied = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "latency",
                        "interface": "lo",
                        "mean_ms": 100u64,
                    }),
                    reason: "dry-run".to_owned(),
                },
                &guard,
            )
            .expect("dry-run must not fail");
        assert!(applied.dry_run, "unarmed guard must produce dry_run: true");
        assert_eq!(applied.adapter, NetemAdapter::KIND);
        // The description carries the rendered command.
        assert!(
            applied.description.contains("tc qdisc add dev lo"),
            "dry-run description should mention the rendered command; got {:?}",
            applied.description
        );
    }

    #[test]
    fn safety_guard_rejects_interface_not_on_allowlist() {
        let guard = SafetyGuard::new();
        // Note: empty allowlist. Any interface is rejected.
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = NetemAdapter::new();
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "latency",
                        "interface": "eth0",
                        "mean_ms": 100u64,
                    }),
                    reason: "iface not on allowlist".to_owned(),
                },
                &guard,
            )
            .expect_err("non-allowlisted interface must be rejected");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::TargetNotAllowed { .. }
        ));
    }

    #[test]
    fn loss_applied_and_reverted_on_privileged_host() {
        if !probe_netem_writable() {
            eprintln!(
                "skipping loss_applied_and_reverted_on_privileged_host: \
                 host has no writable tc/netem capability"
            );
            return;
        }
        let iface = "lo";
        let guard = armed_guard_with_ifaces(&[iface]);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(NetemAdapter::new());
        let mut cleanup = Cleanup::new();
        let applied = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "loss",
                        "interface": iface,
                        "percent": 50.0,
                    }),
                    reason: "loss test".to_owned(),
                },
                &guard,
            )
            .expect("apply must succeed on privileged host");
        assert!(!applied.dry_run);
        let id = cleanup.register(applied, Arc::clone(&adapter));
        // Verify the qdisc is in place.
        let show = Command::new("tc")
            .args(["qdisc", "show", "dev", iface])
            .output()
            .expect("tc qdisc show");
        let stdout = String::from_utf8_lossy(&show.stdout);
        assert!(
            stdout.contains("netem"),
            "expected netem qdisc in `tc qdisc show`; got {stdout}"
        );
        // Revert via the cleanup registry.
        cleanup.revert(id).expect("revert must succeed");
        // After revert the netem qdisc should be gone.
        let show = Command::new("tc")
            .args(["qdisc", "show", "dev", iface])
            .output()
            .expect("tc qdisc show");
        let stdout = String::from_utf8_lossy(&show.stdout);
        assert!(
            !stdout.contains("netem"),
            "expected netem qdisc to be removed after revert; got {stdout}"
        );
    }

    #[test]
    fn cleanup_reverts_on_registry_drop() {
        if !probe_netem_writable() {
            eprintln!(
                "skipping cleanup_reverts_on_registry_drop: \
                 host has no writable tc/netem capability"
            );
            return;
        }
        let iface = "lo";
        let guard = armed_guard_with_ifaces(&[iface]);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(NetemAdapter::new());
        {
            let mut cleanup = Cleanup::new();
            let applied = adapter
                .apply(
                    &FaultPlan {
                        adapter: NetemAdapter::KIND.to_owned(),
                        payload: serde_json::json!({
                            "kind": "latency",
                            "interface": iface,
                            "mean_ms": 50u64,
                        }),
                        reason: "drop test".to_owned(),
                    },
                    &guard,
                )
                .expect("apply must succeed on privileged host");
            cleanup.register(applied, Arc::clone(&adapter));
            // Drop runs here; the cleanup registry should
            // revert the netem qdisc.
        }
        let show = Command::new("tc")
            .args(["qdisc", "show", "dev", iface])
            .output()
            .expect("tc qdisc show");
        let stdout = String::from_utf8_lossy(&show.stdout);
        assert!(
            !stdout.contains("netem"),
            "netem qdisc should be removed on cleanup drop; got {stdout}"
        );
    }

    #[test]
    fn watchdog_reverts_after_timeout() {
        if !probe_netem_writable() {
            eprintln!(
                "skipping watchdog_reverts_after_timeout: \
                 host has no writable tc/netem capability"
            );
            return;
        }
        let iface = "lo";
        let guard = armed_guard_with_ifaces(&[iface]);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(NetemAdapter::new());
        let _applied = adapter
            .apply(
                &FaultPlan {
                    adapter: NetemAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "loss",
                        "interface": iface,
                        "percent": 25.0,
                        "watchdog_ms": 500u64,
                    }),
                    reason: "watchdog test".to_owned(),
                },
                &guard,
            )
            .expect("apply must succeed on privileged host");
        // Poll for the netem qdisc to disappear. The watchdog
        // thread fires after 500ms; allow up to 5 s for the
        // poll to see the change.
        let start = std::time::Instant::now();
        loop {
            let show = Command::new("tc")
                .args(["qdisc", "show", "dev", iface])
                .output()
                .expect("tc qdisc show");
            let stdout = String::from_utf8_lossy(&show.stdout);
            if !stdout.contains("netem") {
                break;
            }
            if start.elapsed() > std::time::Duration::from_secs(5) {
                panic!("watchdog did not revert the netem qdisc within 5 s");
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }
}
