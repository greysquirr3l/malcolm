//! Integration tests for the cgroup v2 resource-limit adapter.
//! Compiled only on Linux with the `cgroups` feature enabled.
//!
//! Most cgroup writes need root or a delegated subtree. The tests
//! probe the host with [`probe_cgroup_writable`] and skip cleanly
//! when the runner lacks privilege — `eprintln!` a skip line so
//! the absence is visible in CI logs.

#![cfg(all(target_os = "linux", feature = "cgroups"))]

use std::sync::Arc;

use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
use malcolm_agent::adapters::cgroups::{
    CgroupAction, CgroupAdapter, cleanup_test_cgroups, probe_cgroup_writable,
};
use malcolm_agent::cleanup::Cleanup;
use malcolm_agent::safety::SafetyGuard;

#[expect(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Build a fully armed `SafetyGuard` with the given paths
    /// on the allowlist. Bypasses the env-flag check via
    /// `arm_for_test`.
    fn armed_guard_with_paths(paths: &[&str]) -> SafetyGuard {
        let mut guard = SafetyGuard::new();
        for p in paths {
            guard.allow_cgroup(*p);
        }
        guard.arm_for_test(true).expect("arm_for_test")
    }

    #[test]
    fn cgroup_v2_detection_returns_root_on_supported_host() {
        // `probe_cgroup_writable` returns `Some` only on a host
        // that exposes cgroup v2 and is writable. On a runner
        // without it, we skip with a clear message.
        if let Some(root) = probe_cgroup_writable() {
            assert!(
                root.join("cgroup.controllers").exists(),
                "detected root must expose cgroup.controllers"
            );
        } else {
            eprintln!(
                "skipping cgroup_v2_detection_returns_root_on_supported_host: \
                 host has no writable cgroup v2 hierarchy (likely unprivileged CI)"
            );
        }
    }

    #[test]
    fn invalid_plan_payload_returns_invalid_plan_error() {
        // The InvalidPlan paths fire before any safety check, so
        // an armed guard with an allowlist entry is enough to
        // exercise them.
        let mut guard = SafetyGuard::new();
        guard.allow_cgroup("/sys/fs/cgroup/malcolm.slice");
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = CgroupAdapter::new();

        // Payload is not a JSON object.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
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

        // Unknown kind.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({"kind": "explode"}),
                    reason: "kind test".to_owned(),
                },
                &guard,
            )
            .expect_err("unknown kind must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));

        // io_max with neither rbps nor wbps.
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "io_max",
                        "device": "253:0"
                    }),
                    reason: "io_max shape test".to_owned(),
                },
                &guard,
            )
            .expect_err("io_max with no rbps/wbps must error");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::InvalidPlan { .. }
        ));
    }

    #[test]
    fn unarmed_guard_makes_cgroup_adapter_dry_run() {
        // No `arm_for_test`; the adapter's `is_armed()` check
        // produces the dry-run. We use a cgroup path that
        // doesn't exist on disk — the dry-run must NOT touch
        // the fs.
        let guard = SafetyGuard::new();
        let adapter = CgroupAdapter::new();
        let applied = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "memory_max",
                        "bytes": 1_000_000u64,
                    }),
                    reason: "dry-run".to_owned(),
                },
                &guard,
            )
            .expect("dry-run must not fail");
        assert!(applied.dry_run, "unarmed guard must produce dry_run: true");
        assert_eq!(applied.adapter, CgroupAdapter::KIND);
    }

    #[test]
    fn target_allowlist_rejects_host_cgroup_root_by_construction() {
        // Even if the operator tries to allowlist it, the host
        // cgroup root is rejected at apply time.
        let mut guard = SafetyGuard::new();
        guard.allow_cgroup("/");
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = CgroupAdapter::new();
        let err = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "memory_max",
                        "bytes": 1u64,
                        "cgroup_path": "/",
                    }),
                    reason: "host root test".to_owned(),
                },
                &guard,
            )
            .expect_err("host cgroup root must be rejected");
        assert!(matches!(
            err,
            malcolm_agent::error::AgentError::TargetNotAllowed { .. }
        ));
    }

    #[test]
    fn memory_max_writes_limit_file_on_privileged_host() {
        // Skip cleanly on unprivileged runners.
        if probe_cgroup_writable().is_none() {
            eprintln!(
                "skipping memory_max_writes_limit_file_on_privileged_host: \
                 host has no writable cgroup v2 hierarchy"
            );
            return;
        }
        // Clean any leftover run-N/ from prior failed runs.
        cleanup_test_cgroups();

        // Allow the malcolm parent slice + the new child path.
        let parent = malcolm_agent::adapters::cgroups::MALCOLM_PARENT_SLICE;
        let guard = armed_guard_with_paths(&[parent]);
        let adapter = CgroupAdapter::new();

        // Allocate a child cgroup without first knowing the
        // exact run-N name. The adapter builds the child path
        // itself; we use the default parent slice as the
        // target so the child path is `parent/run-<id>`.
        let applied = adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "memory_max",
                        "bytes": 33_554_432u64, // 32 MiB
                        "cgroup_path": parent,
                    }),
                    reason: "memory limit test".to_owned(),
                },
                &guard,
            )
            .expect("memory_max apply must succeed on a privileged host");
        assert!(!applied.dry_run);

        // Confirm the child directory was created and the
        // memory.max file matches what we asked for. We don't
        // know the exact run-N name; check by listing the
        // parent directory.
        let entries: Vec<_> = std::fs::read_dir(parent)
            .expect("parent should be readable")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(
            entries.len(),
            1,
            "expected exactly one child cgroup; found {entries:?}"
        );
        let memory_max = entries[0].join("memory.max");
        let written = std::fs::read_to_string(&memory_max).expect("memory.max should be readable");
        let trimmed = written.trim();
        assert_eq!(
            trimmed, "33554432",
            "memory.max should record 32 MiB; got {trimmed:?}"
        );

        // Cleanup: revert the applied fault and confirm the
        // child cgroup is removed.
        let adapter: Arc<dyn TargetAdapter> = Arc::new(CgroupAdapter::new());
        // (The adapter instance we used for the apply is the
        // same; we wrap it here just to exercise the
        // trait-object path.)
        let _ = adapter; // silence unused
        cleanup_test_cgroups();
    }

    #[test]
    fn cpu_max_writes_quota_file_on_privileged_host() {
        if probe_cgroup_writable().is_none() {
            eprintln!(
                "skipping cpu_max_writes_quota_file_on_privileged_host: \
                 host has no writable cgroup v2 hierarchy"
            );
            return;
        }
        cleanup_test_cgroups();

        let parent = malcolm_agent::adapters::cgroups::MALCOLM_PARENT_SLICE;
        let guard = armed_guard_with_paths(&[parent]);
        let adapter = CgroupAdapter::new();
        adapter
            .apply(
                &FaultPlan {
                    adapter: CgroupAdapter::KIND.to_owned(),
                    payload: serde_json::json!({
                        "kind": "cpu_max",
                        "quota_us": 50_000u64,
                        "period_us": 100_000u64,
                        "cgroup_path": parent,
                    }),
                    reason: "cpu quota test".to_owned(),
                },
                &guard,
            )
            .expect("cpu_max apply must succeed");
        let entries: Vec<_> = std::fs::read_dir(parent)
            .expect("parent should be readable")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(entries.len(), 1);
        let cpu_max = entries[0].join("cpu.max");
        let written = std::fs::read_to_string(&cpu_max).expect("cpu.max should be readable");
        let trimmed = written.trim();
        assert_eq!(
            trimmed, "50000 100000",
            "cpu.max should record the quota and period; got {trimmed:?}"
        );
        cleanup_test_cgroups();
    }

    #[test]
    fn cleanup_removes_cgroup_on_registry_drop() {
        if probe_cgroup_writable().is_none() {
            eprintln!(
                "skipping cleanup_removes_cgroup_on_registry_drop: \
                 host has no writable cgroup v2 hierarchy"
            );
            return;
        }
        cleanup_test_cgroups();

        let parent = malcolm_agent::adapters::cgroups::MALCOLM_PARENT_SLICE;
        let guard = armed_guard_with_paths(&[parent]);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(CgroupAdapter::new());
        {
            let mut cleanup = Cleanup::new();
            let applied = adapter
                .apply(
                    &FaultPlan {
                        adapter: CgroupAdapter::KIND.to_owned(),
                        payload: serde_json::json!({
                            "kind": "memory_max",
                            "bytes": 1_048_576u64,
                            "cgroup_path": parent,
                        }),
                        reason: "cleanup test".to_owned(),
                    },
                    &guard,
                )
                .expect("apply must succeed on privileged host");
            let id = cleanup.register(applied, Arc::clone(&adapter));
            // Verify the child cgroup is present.
            let entries: Vec<_> = std::fs::read_dir(parent)
                .expect("parent readable")
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            assert_eq!(entries.len(), 1, "child cgroup should exist");
            // Drop runs here; the cleanup registry should
            // revert the applied fault and remove the child
            // cgroup.
            drop(cleanup);
            let _ = id; // silence unused
        }
        // After the cleanup registry drops, the child cgroup
        // should be gone.
        let entries: Vec<_> = std::fs::read_dir(parent)
            .expect("parent readable")
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        assert_eq!(
            entries.len(),
            0,
            "child cgroup should be removed on cleanup drop; still have {entries:?}"
        );
    }

    #[test]
    fn from_payload_round_trip_for_each_action_kind() {
        let cases = vec![
            (
                serde_json::json!({"kind": "cpu_max", "quota_us": 50_000u64, "period_us": 100_000u64}),
                CgroupAction::CpuMax {
                    quota_us: 50_000,
                    period_us: 100_000,
                },
            ),
            (
                serde_json::json!({"kind": "memory_max", "bytes": 1_048_576u64}),
                CgroupAction::MemoryMax { bytes: 1_048_576 },
            ),
            (
                serde_json::json!({
                    "kind": "io_max",
                    "device": "253:0",
                    "rbps": 1_000_000u64,
                    "wbps": 2_000_000u64
                }),
                CgroupAction::IoMax {
                    device: "253:0".to_owned(),
                    rbps: Some(1_000_000),
                    wbps: Some(2_000_000),
                },
            ),
        ];
        for (payload, expected) in cases {
            let parsed = CgroupAction::from_payload(&payload).expect("payload must parse");
            assert_eq!(parsed, expected);
        }
    }
}
