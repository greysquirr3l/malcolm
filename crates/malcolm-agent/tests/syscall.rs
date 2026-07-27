//! Integration tests for the `ptrace`-based syscall interception
//! adapter. Compiled only on `target_os = "linux"`, `target_arch =
//! "x86_64"`, with the `syscall` feature enabled.
//!
//! `ptrace` needs either root, a matching uid with a permissive
//! `ptrace_scope`, or `CAP_SYS_PTRACE` — and containerised CI runners
//! often deny it via seccomp regardless of capabilities. Every test
//! that actually traces a process probes the host first with
//! [`probe_ptrace_available`] and skips cleanly (`eprintln!`, no
//! panic) when it is unavailable, matching the `cgroups`/`netem`
//! adapters' test convention.
//!
//! # Observing the traced child's syscalls from outside
//!
//! `SyscallAdapter::apply` spawns its own child internally and never
//! exposes its stdio, so tests can't pipe the traced process's stdout.
//! Instead, every helper script here redirects its output to a file
//! path baked into the command string ([`unique_out_path`]) — the test
//! reads that file directly from the filesystem afterward. Each write
//! to the file is still a real `write(2)` syscall from the traced
//! process, so injection is exercised exactly as it would be for any
//! other target.

#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "syscall"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
use malcolm_agent::adapters::syscall::{SyscallAdapter, probe_ptrace_available};
use malcolm_agent::cleanup::Cleanup;
use malcolm_agent::error::AgentError;
use malcolm_agent::safety::SafetyGuard;

#[expect(clippy::expect_used, reason = "test assertions")]
mod tests {
    use super::*;

    /// Build a fully armed `SafetyGuard`, optionally allowlisting a
    /// pid for attach-mode tests. Bypasses the env-flag check via
    /// `arm_for_test`, matching the other adapters' test convention.
    fn armed_guard(allow_pid: Option<u32>) -> SafetyGuard {
        let mut guard = SafetyGuard::new();
        if let Some(pid) = allow_pid {
            guard.allow_pid(pid);
        }
        guard.arm_for_test(true).expect("arm_for_test")
    }

    /// A `FaultPlan` for the syscall adapter.
    fn plan(payload: serde_json::Value) -> FaultPlan {
        FaultPlan {
            adapter: SyscallAdapter::KIND.to_owned(),
            payload,
            reason: "test".to_owned(),
        }
    }

    /// A fresh path under the OS temp dir, unique per call (test
    /// binaries run tests in parallel threads within one process, so a
    /// counter is needed on top of the pid).
    fn unique_out_path(label: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "malcolm-syscall-test-{label}-{}-{n}.out",
            std::process::id()
        ))
    }

    /// Read `path`, returning an empty `Vec` if it does not exist yet
    /// (e.g. every write to it was injected-failed before any data
    /// landed, though the redirecting shell still creates the file).
    fn read_out(path: &Path) -> Vec<u8> {
        std::fs::read(path).unwrap_or_default()
    }

    /// A shell command that performs exactly `count` single-byte
    /// `write(2)` syscalls (via `printf x`) to `out_path`, opened once
    /// for the whole loop via a brace-group redirection. Pure
    /// `dash`/`bash` builtins (`printf`, `[`, arithmetic) — no forked
    /// descendants, so every syscall it makes is made by the traced
    /// pid itself.
    fn write_loop_command(count: u32, out_path: &Path) -> Vec<String> {
        vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!(
                "{{ i=0; while [ $i -lt {count} ]; do printf x; i=$((i+1)); done; }} >> {}",
                out_path.display()
            ),
        ]
    }

    #[test]
    fn ptrace_availability_probe_does_not_panic() {
        // Just exercises the probe; result depends on the runner.
        let _ = probe_ptrace_available();
    }

    #[test]
    fn unarmed_guard_makes_syscall_adapter_dry_run() {
        // No privilege needed: the unarmed path never touches ptrace.
        let guard = SafetyGuard::new();
        let adapter = SyscallAdapter::new();
        let p = plan(serde_json::json!({
            "kind": "fail_with",
            "mode": "spawn",
            "command": ["/bin/true"],
            "syscall": "write",
            "errno": 28,
        }));
        let applied = adapter.apply(&p, &guard).expect("dry-run must not fail");
        assert!(applied.dry_run, "unarmed guard must produce dry_run: true");
        assert_eq!(applied.adapter, SyscallAdapter::KIND);
        assert!(
            applied.description.contains("write"),
            "dry-run description should name the trapped syscall: {}",
            applied.description
        );
    }

    #[test]
    fn invalid_plan_payload_returns_invalid_plan_error() {
        let guard = armed_guard(None);
        let adapter = SyscallAdapter::new();

        let err = adapter
            .apply(&plan(serde_json::json!("not an object")), &guard)
            .expect_err("non-object payload must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));

        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "explode", "mode": "spawn",
                    "command": ["/bin/true"], "syscall": "write",
                })),
                &guard,
            )
            .expect_err("unknown kind must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));

        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "fail_with", "mode": "teleport",
                    "syscall": "write", "errno": 1,
                })),
                &guard,
            )
            .expect_err("unknown mode must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));

        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "fail_with", "mode": "spawn", "command": ["/bin/true"],
                    "syscall": "definitely_not_a_syscall", "errno": 1,
                })),
                &guard,
            )
            .expect_err("unknown syscall name must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));

        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "fail_with", "mode": "spawn", "command": ["/bin/true"],
                    "syscall": "write", "errno": 1, "probability": 1.5,
                })),
                &guard,
            )
            .expect_err("out-of-range probability must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));

        let err = adapter
            .apply(
                &plan(serde_json::json!({
                    "kind": "fail_with", "mode": "spawn", "command": [],
                    "syscall": "write", "errno": 1,
                })),
                &guard,
            )
            .expect_err("empty command must error");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn attach_mode_without_explicit_opt_in_is_rejected() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping attach_mode_without_explicit_opt_in_is_rejected: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        // A long-lived, ordinary (untraced) child the test owns
        // directly, playing the role of "some other process". A tight
        // sleep loop rather than a single long `sleep` so nothing here
        // depends on interrupting an in-progress blocking syscall.
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("i=0; while [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done")
            .spawn()
            .expect("failed to spawn helper");
        let pid = child.id();
        let guard = armed_guard(Some(pid));
        // Attach mode disabled by default (`SyscallAdapter::new()`).
        let adapter = SyscallAdapter::new();
        let p = plan(serde_json::json!({
            "kind": "delay", "mode": "attach", "pid": pid,
            "syscall": "write", "delay_ms": 1,
        }));
        let err = adapter
            .apply(&p, &guard)
            .expect_err("attach mode must be rejected without explicit opt-in");
        assert!(matches!(err, AgentError::AdapterFailure { .. }));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn attach_to_non_allowlisted_pid_is_rejected() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping attach_to_non_allowlisted_pid_is_rejected: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg("i=0; while [ $i -lt 100 ]; do sleep 0.05; i=$((i+1)); done")
            .spawn()
            .expect("failed to spawn helper");
        let pid = child.id();
        // Guard armed but the pid is deliberately NOT allowlisted.
        let guard = armed_guard(None);
        let adapter = SyscallAdapter::new().with_attach_enabled(true);
        let p = plan(serde_json::json!({
            "kind": "delay", "mode": "attach", "pid": pid,
            "syscall": "write", "delay_ms": 1,
        }));
        let err = adapter
            .apply(&p, &guard)
            .expect_err("non-allowlisted attach target must be rejected");
        assert!(matches!(err, AgentError::TargetNotAllowed { .. }));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn spawn_under_supervision_fail_with_probability_one_skips_every_write() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping spawn_under_supervision_fail_with_probability_one_skips_every_write: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let out = unique_out_path("failwith");
        let guard = armed_guard(None);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(SyscallAdapter::new());
        let p = plan(serde_json::json!({
            "kind": "fail_with",
            "mode": "spawn",
            "command": write_loop_command(50, &out),
            "syscall": "write",
            "errno": 28, // ENOSPC
            "probability": 1.0,
        }));
        let mut cleanup = Cleanup::new();
        let applied = adapter
            .apply(&p, &guard)
            .expect("apply must succeed on a host that permits ptrace");
        assert!(!applied.dry_run);
        let id = cleanup.register(applied, Arc::clone(&adapter));

        // The child is parked at its post-execve trap until the
        // supervisor thread we just started issues its first
        // continue; by the time `apply` returned, that thread was
        // already spawned, so there is no race here — the loop cannot
        // have produced a single successful write yet regardless of
        // scheduling. Give the (fast, tightly-looping) child generous
        // time to run to completion under ptrace's per-syscall
        // overhead.
        std::thread::sleep(Duration::from_millis(1500));
        cleanup.revert(id).expect("revert must detach cleanly");

        let bytes = read_out(&out);
        assert!(
            bytes.is_empty(),
            "probability 1.0 FailWith must skip every write; got {} bytes: {bytes:?}",
            bytes.len()
        );
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn attach_mode_fails_writes_while_active_then_reverts_to_let_writes_succeed() {
        const PHASE_LEN: u32 = 20;
        if !probe_ptrace_available() {
            eprintln!(
                "skipping attach_mode_fails_writes_while_active_then_reverts_to_let_writes_succeed: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let out = unique_out_path("attach-before-after");
        // Phase 1 (20 writes) happens only after a 400ms head start,
        // comfortably longer than PTRACE_SEIZE+PTRACE_INTERRUPT takes
        // to arm interception. A 1s gap separates phase 1 from phase 2
        // (another 20 writes), giving a wide, non-flaky margin for the
        // test to call `revert` between the two phases.
        let script = format!(
            "sleep 0.4; {{ i=0; while [ $i -lt {PHASE_LEN} ]; do printf x; i=$((i+1)); done; \
             sleep 1; i=0; while [ $i -lt {PHASE_LEN} ]; do printf x; i=$((i+1)); done; }} >> {}",
            out.display()
        );
        let mut child = Command::new("/bin/sh")
            .arg("-c")
            .arg(&script)
            .spawn()
            .expect("failed to spawn helper");
        let pid = child.id();

        let guard = armed_guard(Some(pid));
        let adapter = SyscallAdapter::new().with_attach_enabled(true);
        let p = plan(serde_json::json!({
            "kind": "fail_with",
            "mode": "attach",
            "pid": pid,
            "syscall": "write",
            "errno": 28,
            "probability": 1.0,
        }));
        // Attach immediately (well within the child's initial 400ms
        // sleep), so tracing is fully armed before phase 1 starts.
        let applied = adapter.apply(&p, &guard).expect("attach must succeed");

        // Phase 1 runs entirely while attached; wait past its window
        // but well before phase 2 begins (~1.4s+ from script start).
        std::thread::sleep(Duration::from_millis(900));
        assert!(
            read_out(&out).is_empty(),
            "phase 1 writes must all be injected-failed while attached"
        );

        adapter
            .revert(&applied)
            .expect("revert must detach cleanly");

        // Phase 2 begins around the 1.4s mark and finishes quickly;
        // wait comfortably past it, then reap the (now-finished) child.
        let status = child.wait().expect("child must exit");
        assert!(
            status.success(),
            "helper script must exit cleanly: {status:?}"
        );

        let bytes = read_out(&out);
        assert_eq!(
            bytes.len(),
            PHASE_LEN as usize,
            "expected exactly phase 2's writes to have succeeded after revert; got {bytes:?}"
        );
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn delay_measurably_slows_syscall_entry() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping delay_measurably_slows_syscall_entry: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let out = unique_out_path("delay");
        let guard = armed_guard(None);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(SyscallAdapter::new());
        let delay_ms = 500u64;
        let p = plan(serde_json::json!({
            "kind": "delay",
            "mode": "spawn",
            "command": ["/bin/sh", "-c", format!("printf x >> {}", out.display())],
            "syscall": "write",
            "delay_ms": delay_ms,
            "probability": 1.0,
        }));
        let mut cleanup = Cleanup::new();
        let applied = adapter.apply(&p, &guard).expect("apply must succeed");
        let id = cleanup.register(applied, Arc::clone(&adapter));

        // Well before the delay elapses, the write must not have
        // landed yet.
        std::thread::sleep(Duration::from_millis(delay_ms / 2));
        assert!(
            read_out(&out).is_empty(),
            "write must still be held at its entry stop before delay_ms elapses"
        );

        // Well after the delay elapses, it must have landed.
        std::thread::sleep(Duration::from_millis(delay_ms));
        assert_eq!(
            read_out(&out),
            b"x",
            "delayed write must eventually succeed and land exactly once"
        );

        cleanup.revert(id).expect("revert must detach cleanly");
        let _ = std::fs::remove_file(&out);
    }

    #[test]
    fn probability_with_fixed_seed_reproduces_same_output_across_runs() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping probability_with_fixed_seed_reproduces_same_output_across_runs: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let run = |label: &str| -> Vec<u8> {
            let out = unique_out_path(label);
            let guard = armed_guard(None);
            let adapter: Arc<dyn TargetAdapter> = Arc::new(SyscallAdapter::new());
            let p = plan(serde_json::json!({
                "kind": "fail_with",
                "mode": "spawn",
                "command": write_loop_command(200, &out),
                "syscall": "write",
                "errno": 28,
                "probability": 0.5,
                "seed": 1234,
            }));
            let mut cleanup = Cleanup::new();
            let applied = adapter.apply(&p, &guard).expect("apply must succeed");
            let id = cleanup.register(applied, Arc::clone(&adapter));
            std::thread::sleep(Duration::from_millis(2500));
            cleanup.revert(id).expect("revert must detach cleanly");
            let bytes = read_out(&out);
            let _ = std::fs::remove_file(&out);
            bytes
        };

        let out_a = run("seed-a");
        let out_b = run("seed-b");

        assert_eq!(
            out_a, out_b,
            "same seed + probability must reproduce the identical inject/skip sequence"
        );
        // Sanity: probability 0.5 should not degenerate to "always" or
        // "never" over 200 occurrences (astronomically unlikely by
        // chance; a bug that ignored `probability` would reliably
        // produce all-or-nothing here).
        assert!(
            !out_a.is_empty() && out_a.len() < 200,
            "probability 0.5 over 200 occurrences should be neither empty nor full, got {} bytes",
            out_a.len()
        );
    }

    #[test]
    fn cleanup_registry_drop_reverts_without_hanging() {
        if !probe_ptrace_available() {
            eprintln!(
                "skipping cleanup_registry_drop_reverts_without_hanging: \
                 host does not permit ptrace (likely unprivileged/seccomp-restricted CI)"
            );
            return;
        }
        let out = unique_out_path("drop-revert");
        let guard = armed_guard(None);
        let adapter: Arc<dyn TargetAdapter> = Arc::new(SyscallAdapter::new());
        let p = plan(serde_json::json!({
            "kind": "fail_with",
            "mode": "spawn",
            "command": write_loop_command(500, &out),
            "syscall": "write",
            "errno": 28,
            "probability": 1.0,
        }));
        let mut cleanup = Cleanup::new();
        let applied = adapter.apply(&p, &guard).expect("apply must succeed");
        let id = cleanup.register(applied, Arc::clone(&adapter));
        // Drop the registry (rather than calling revert explicitly) to
        // exercise the `Drop`-time path. If this hangs or panics, the
        // test process itself never reaches the assertion below —
        // that absence of a hang/panic is the actual assertion.
        drop(cleanup);
        let _ = id;
        let _ = std::fs::remove_file(&out);
    }
}
