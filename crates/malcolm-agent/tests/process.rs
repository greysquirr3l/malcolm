//! Integration tests for the process-control adapter. Compiled only
//! on Unix with the `process` feature enabled. The tests spawn their
//! own child processes and signal only those — they never touch
//! external pids, so the safety guard's "self / pid 1" rejection
//! path is covered by separate tests below.

#![cfg(all(unix, feature = "process"))]

use std::sync::Arc;
use std::time::Duration;

use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
use malcolm_agent::adapters::process::{ProcessAction, ProcessAdapter};
use malcolm_agent::cleanup::Cleanup;
use malcolm_agent::error::AgentError;
use malcolm_agent::safety::SafetyGuard;

#[expect(clippy::expect_used, reason = "test assertions")]
#[expect(clippy::panic, reason = "test assertions")]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::Command;

    /// Spawn a child that prints its pid and waits for a signal.
    /// The test sends the signal and then either reaps the child or
    /// inspects its `/proc/<pid>/status` to confirm state.
    fn spawn_long_running_child() -> std::process::Child {
        // `sleep 30` is portable across macOS and Linux. The test
        // signals the child before the 30 s elapses.
        Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("failed to spawn `sleep 30` child")
    }

    /// Wait up to `timeout` for a child to exit, returning the
    /// `ExitStatus`. Panics if the child does not exit in time.
    fn wait_with_timeout(
        mut child: std::process::Child,
        timeout: Duration,
    ) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status,
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        panic!("child did not exit within {timeout:?}");
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                // ECHILD means the child was already reaped. Treat
                // as "already exited" and synthesise a status from
                // the signal we sent; the test only cares that the
                // child is gone.
                Err(e) if e.raw_os_error() == Some(10) => {
                    return std::process::ExitStatus::from_raw(0);
                }
                Err(e) => panic!("try_wait failed: {e}"),
            }
        }
    }

    /// Build a fully armed `SafetyGuard` with the given pid on the
    /// allowlist, bypassing the env-flag check via
    /// [`SafetyGuard::arm_for_test`]. Production wiring should
    /// still use [`SafetyGuard::arm`] which requires the env flag.
    fn armed_guard_allowing(pid: u32) -> SafetyGuard {
        let mut guard = SafetyGuard::new();
        guard.allow_pid(pid);
        guard
            .arm_for_test(true)
            .expect("arm_for_test with i_understand_the_blast_radius=true")
    }

    /// Build a JSON plan for the process adapter.
    fn plan(payload: serde_json::Value) -> FaultPlan {
        FaultPlan {
            adapter: ProcessAdapter::KIND.to_owned(),
            payload,
            reason: "test".to_owned(),
        }
    }

    #[test]
    fn unarmed_guard_makes_process_adapter_dry_run() {
        // No `arm_for_test`; the adapter's `is_armed()` check is
        // what produces the dry-run result.
        let guard = SafetyGuard::new();
        let adapter = ProcessAdapter::new();
        let p = plan(serde_json::json!({
            "kind": "signal",
            "pid": 999_999u32,
            "signal": "SIGUSR1",
        }));
        let applied = adapter.apply(&p, &guard).expect("dry-run must not fail");
        assert!(applied.dry_run, "unarmed guard must produce dry_run: true");
        assert_eq!(applied.adapter, ProcessAdapter::KIND);
    }

    #[test]
    fn invalid_plan_payload_returns_invalid_plan_error() {
        let mut guard = SafetyGuard::new();
        let target_pid = 999_998u32;
        guard.allow_pid(target_pid);
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = ProcessAdapter::new();
        // Payload is not a JSON object.
        let err = adapter
            .apply(&plan(serde_json::json!("not an object")), &guard)
            .expect_err("non-object payload must error");
        assert!(
            matches!(err, AgentError::InvalidPlan { .. }),
            "expected InvalidPlan, got {err:?}"
        );
        // Unknown kind.
        let err = adapter
            .apply(
                &plan(serde_json::json!({"kind": "explode", "pid": target_pid})),
                &guard,
            )
            .expect_err("unknown kind must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
        // Unknown signal name. The decoder catches this before the
        // liveness probe, so it surfaces as InvalidPlan (not
        // AdapterFailure).
        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "signal",
                    "pid": target_pid,
                    "signal": "SIGBOGUS",
                })),
                &guard,
            )
            .expect_err("unknown signal must error");
        assert!(
            matches!(err, AgentError::InvalidPlan { .. }),
            "expected InvalidPlan for unknown signal, got {err:?}"
        );
    }

    #[test]
    fn target_allowlist_rejects_self_and_pid_1_when_armed() {
        let self_pid = std::process::id();
        let mut guard = SafetyGuard::new();
        // Allowlist everything we *might* want to allow — but the
        // built-in pid 1 / self rejection still fires.
        guard.allow_pid(1);
        guard.allow_pid(self_pid);
        let guard = guard.arm_for_test(true).expect("arm_for_test");
        let adapter = ProcessAdapter::new();

        for target in [1u32, self_pid] {
            let p = plan(serde_json::json!({
                "kind": "signal",
                "pid": target,
                "signal": "SIGUSR1",
            }));
            let err = adapter
                .apply(&p, &guard)
                .expect_err("forbidden target must be rejected");
            assert!(
                matches!(err, AgentError::TargetNotAllowed { .. }),
                "expected TargetNotAllowed for pid {target}, got {err:?}"
            );
        }
    }

    #[test]
    fn terminate_self_spawned_child_with_short_grace() {
        let child = spawn_long_running_child();
        let pid = child.id();
        let guard = armed_guard_allowing(pid);
        let adapter = ProcessAdapter::new();
        let p = plan(serde_json::json!({
            "kind": "terminate",
            "pid": pid,
            "grace_ms": 500u64,
        }));
        adapter
            .apply(&p, &guard)
            .expect("terminate must succeed on a spawned child");
        let status = wait_with_timeout(child, Duration::from_secs(5));
        // SIGTERM terminates `sleep` with signal 15. We don't care
        // exactly which mechanism reaped it as long as it is gone.
        assert!(!status.success(), "child should not exit cleanly");
    }

    #[test]
    fn pause_then_resume_self_spawned_child() {
        let mut child = spawn_long_running_child();
        let pid = child.id();
        let guard = armed_guard_allowing(pid);
        let adapter = Arc::new(ProcessAdapter::new());
        let pause_plan = plan(serde_json::json!({"kind": "pause", "pid": pid}));
        let applied = adapter
            .apply(&pause_plan, &guard)
            .expect("pause must succeed");
        assert!(!applied.dry_run);
        assert_eq!(applied.adapter, ProcessAdapter::KIND);

        // Give the signal a moment to land before we resume.
        std::thread::sleep(Duration::from_millis(50));
        let resume_plan = plan(serde_json::json!({"kind": "resume", "pid": pid}));
        adapter
            .apply(&resume_plan, &guard)
            .expect("resume must succeed");

        // The child should still be running. Wait briefly and
        // confirm the child has not been reaped.
        std::thread::sleep(Duration::from_millis(50));
        match child.try_wait() {
            Ok(Some(_)) => panic!("child should still be running after resume"),
            Ok(None) => {} // Expected.
            Err(e) => panic!("try_wait errored: {e}"),
        }

        // Tidy up so the child does not outlive the test.
        let kill_plan = plan(serde_json::json!({
            "kind": "terminate",
            "pid": pid,
            "grace_ms": 200u64,
        }));
        adapter
            .apply(&kill_plan, &guard)
            .expect("teardown terminate must succeed");
        let status = wait_with_timeout(child, Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn pause_revert_resumes_paused_child_via_cleanup_registry() {
        let mut child = spawn_long_running_child();
        let pid = child.id();
        let guard = armed_guard_allowing(pid);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(ProcessAdapter::new());
        let pause_plan = plan(serde_json::json!({"kind": "pause", "pid": pid}));
        let mut cleanup = Cleanup::new();
        let applied = adapter
            .apply(&pause_plan, &guard)
            .expect("pause must succeed");
        let id = cleanup.register(applied, Arc::clone(&adapter));
        // Revert the pause: cleanup calls adapter.revert, which
        // sends SIGCONT to the paused child.
        cleanup
            .revert(id)
            .expect("revert of pause must send SIGCONT");
        // Give the signal a moment to land.
        std::thread::sleep(Duration::from_millis(50));
        // Child should be running again.
        match child.try_wait() {
            Ok(Some(_)) => panic!("child should be running after revert"),
            Ok(None) => {} // Expected.
            Err(e) => panic!("try_wait errored: {e}"),
        }
        // Tidy up.
        let kill_plan = plan(serde_json::json!({
            "kind": "terminate",
            "pid": pid,
            "grace_ms": 200u64,
        }));
        adapter
            .apply(&kill_plan, &guard)
            .expect("teardown terminate must succeed");
        let status = wait_with_timeout(child, Duration::from_secs(5));
        assert!(!status.success());
    }

    #[test]
    fn signal_self_spawned_child_with_sigusr1() {
        let child = spawn_long_running_child();
        let pid = child.id();
        let guard = armed_guard_allowing(pid);
        let adapter = ProcessAdapter::new();
        let p = plan(serde_json::json!({
            "kind": "signal",
            "pid": pid,
            "signal": "SIGUSR1",
        }));
        adapter.apply(&p, &guard).expect("signal must succeed");
        // The child doesn't trap SIGUSR1, so it should be killed
        // with the default action (terminate). Reap it.
        let status = wait_with_timeout(child, Duration::from_secs(5));
        // Either it was killed by SIGUSR1 or it died some other
        // way; we only care that it is gone.
        assert!(
            !status.success() || status.signal().is_some_and(|s| s != 0) || status.code().is_none(),
            "child should not exit cleanly from SIGUSR1"
        );
    }

    #[test]
    fn from_payload_round_trip_for_each_action_kind() {
        // Parse / dump round-trip for every ProcessAction variant.
        // We do not run a real signal here; the JSON shape is what
        // we care about.
        let cases = vec![
            (
                serde_json::json!({"kind": "signal", "pid": 1234u32, "signal": "SIGUSR1"}),
                ProcessAction::Signal {
                    pid: 1234,
                    signal: "SIGUSR1".to_owned(),
                },
            ),
            (
                serde_json::json!({"kind": "terminate", "pid": 4321u32, "grace_ms": 250u64}),
                ProcessAction::Terminate {
                    pid: 4321,
                    grace: Duration::from_millis(250),
                },
            ),
            (
                serde_json::json!({"kind": "pause", "pid": 7u32}),
                ProcessAction::Pause { pid: 7 },
            ),
            (
                serde_json::json!({"kind": "resume", "pid": 8u32}),
                ProcessAction::Resume { pid: 8 },
            ),
        ];
        for (payload, expected) in cases {
            let parsed = ProcessAction::from_payload(&payload).expect("payload must parse");
            assert_eq!(parsed, expected);
        }
    }
}
