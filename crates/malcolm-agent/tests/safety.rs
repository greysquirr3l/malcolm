//! Integration tests for the malcolm-agent safety interlocks.
//!
//! These tests cover the T33 spec: `SafetyGuard` refuses to arm without
//! both the env flag and the explicit opt-in boolean, refuses
//! obviously dangerous targets by construction, and the Cleanup
//! registry reverts registered faults on drop. They live in
//! `tests/` (not in `#[cfg(test)] mod tests {}` inside `lib.rs`) so
//! they exercise the public API as a downstream consumer would.

// The lint exception for `clippy::expect_used` is scoped to a
// module wrapper. Whole-file `#![expect(...)]` inner attributes do
// not reliably apply to a test file with multiple top-level items
// in Rust 2024, so we collect the assertions into a single
// `mod tests` and scope the exception to the module.
#[expect(clippy::expect_used, reason = "test assertions")]
mod tests {
    //! The actual test bodies live inside this module so the
    //! `#![expect(...)]` attributes are scoped to a single item.

    use std::sync::Arc;

    use malcolm_agent::adapter::{AppliedFault, FaultPlan, TargetAdapter};
    use malcolm_agent::cleanup::Cleanup;
    use malcolm_agent::error::AgentError;
    use malcolm_agent::null::NullAdapter;
    use malcolm_agent::safety::{ARM_ENV_FLAG, SafetyGuard};

    fn sample_plan(adapter: &str) -> FaultPlan {
        FaultPlan {
            adapter: adapter.to_owned(),
            payload: serde_json::json!({}),
            reason: "test".to_owned(),
        }
    }

    /// Single mutex shared by every test that mutates
    /// `MALCOLM_AGENT_ARM`. Two separate `static` mutexes (one per
    /// function) would not serialise the env-var mutations against
    /// each other when cargo runs tests in parallel. The single
    /// module-level mutex makes the race observable and controllable.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    #[expect(
        unsafe_code,
        reason = "test needs to control MALCOLM_AGENT_ARM env var"
    )]
    fn safety_guard_refuses_to_arm_without_env_flag() {
        // Ensure the env flag is NOT set for this test.
        let snapshot = std::env::var(ARM_ENV_FLAG).ok();
        // SAFETY: cargo runs tests on multiple threads by default. We
        // hold the module-level `ENV_LOCK` for the whole test, so no
        // other test in this file can read or write the env var while
        // we hold it.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: see mutex contract above.
        unsafe {
            std::env::remove_var(ARM_ENV_FLAG);
        }

        let mut guard = SafetyGuard::new();
        guard.allow_pid(42);
        let result = guard.arm(true);

        // Restore the original env (if any) so we don't leak state into
        // sibling tests in the same process. The lock is still held.
        if let Some(value) = snapshot {
            // SAFETY: lock still held; no concurrent env-var reader.
            unsafe {
                std::env::set_var(ARM_ENV_FLAG, value);
            }
        }

        let err = result.expect_err("arm should fail when env flag is missing");
        assert!(
            matches!(err, AgentError::ArmFlagMissing),
            "expected ArmFlagMissing, got {err:?}"
        );
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "test needs to control MALCOLM_AGENT_ARM env var"
    )]
    fn safety_guard_refuses_to_arm_without_explicit_boolean() {
        // SAFETY: lock held for the whole test, so no concurrent env
        // reader in this process can race the set/remove sequence.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // SAFETY: see mutex contract above.
        unsafe {
            std::env::set_var(ARM_ENV_FLAG, "1");
        }

        let mut guard = SafetyGuard::new();
        guard.allow_pid(7);
        // Pass `false` to the named boolean. The contract is "the named
        // parameter must be `true`", so `false` is the rejection path.
        let result = guard.arm(false);

        // SAFETY: see SAFETY note above.
        unsafe {
            std::env::remove_var(ARM_ENV_FLAG);
        }

        let err = result.expect_err("arm should fail when explicit boolean is false");
        assert!(
            matches!(err, AgentError::ExplicitOptInMissing),
            "expected ExplicitOptInMissing, got {err:?}"
        );
    }

    #[test]
    #[expect(
        unsafe_code,
        reason = "test needs to control MALCOLM_AGENT_ARM env var"
    )]
    fn unarmed_guard_makes_null_adapter_return_dry_run() {
        // The env flag is intentionally not set.
        // SAFETY: this test does not race with the two env-flag
        // tests above because it does not need the flag to be set;
        // it only asserts the default (unarmed) behaviour.
        unsafe {
            std::env::remove_var(ARM_ENV_FLAG);
        }

        let guard = SafetyGuard::new();
        let adapter = NullAdapter::new();
        let plan = sample_plan("null");

        let applied = adapter
            .apply(&plan, &guard)
            .expect("null adapter apply never fails");
        assert!(applied.dry_run, "null adapter must report dry_run: true");
        assert_eq!(applied.adapter, "null");
    }

    #[test]
    fn target_allowlist_rejects_pid_1_and_current_pid_by_construction() {
        // Self-pid rejection is observable without arming.
        let guard = SafetyGuard::new();
        let self_pid = std::process::id();
        assert_eq!(guard.check_pid(1), Some("pid_1"));
        assert_eq!(guard.check_pid(self_pid), Some("self_pid"));

        // After allowlisting, require_pid still rejects 1 and self.
        let mut guard = SafetyGuard::new();
        guard.allow_pid(self_pid);
        guard.allow_pid(1);
        assert!(matches!(
            guard.require_pid(1),
            Err(AgentError::TargetNotAllowed { rule: "pid_1", .. })
        ));
        assert!(matches!(
            guard.require_pid(self_pid),
            Err(AgentError::TargetNotAllowed {
                rule: "self_pid",
                ..
            })
        ));

        // A pid that is not in the rejection set nor the allowlist.
        let unknown = self_pid.saturating_add(99_999);
        assert!(matches!(
            guard.require_pid(unknown),
            Err(AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                ..
            })
        ));

        // An explicitly-allowlisted pid (not in the rejection set) passes.
        let safe_pid = 65_536u32; // high enough to be neither pid 1 nor self
        guard.allow_pid(safe_pid);
        guard
            .require_pid(safe_pid)
            .expect("explicitly-allowlisted pid should pass");
    }

    #[test]
    fn target_allowlist_rejects_host_cgroup_root_by_construction() {
        let mut guard = SafetyGuard::new();
        // Even if the operator tries to allowlist it, the host cgroup
        // root is rejected at apply time.
        guard.allow_cgroup("/");
        let err = guard
            .require_cgroup("/")
            .expect_err("host cgroup root must be rejected");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "host_cgroup_root",
                ..
            }
        ));

        // A non-root cgroup that is not on the allowlist is rejected
        // with a different rule.
        let err = guard
            .require_cgroup("/sys/fs/cgroup/malcolm/test.slice")
            .expect_err("non-allowlisted cgroup must be rejected");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                ..
            }
        ));

        // And an explicitly-allowlisted cgroup passes.
        guard.allow_cgroup("/sys/fs/cgroup/malcolm/test.slice");
        guard
            .require_cgroup("/sys/fs/cgroup/malcolm/test.slice")
            .expect("allowlisted cgroup should pass");
    }

    #[test]
    fn cleanup_registry_reverts_registered_faults_on_drop() {
        // Use a recording adapter that counts apply/revert calls so we
        // can prove the drop path ran.
        #[derive(Default)]
        struct Recorder {
            applied: std::sync::Mutex<u32>,
            reverted: std::sync::Mutex<u32>,
        }
        impl TargetAdapter for Recorder {
            fn apply(
                &self,
                _plan: &FaultPlan,
                _guard: &SafetyGuard,
            ) -> Result<AppliedFault, AgentError> {
                *self
                    .applied
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
                Ok(AppliedFault {
                    id: 0,
                    adapter: "recorder",
                    dry_run: true,
                    description: "test".to_owned(),
                })
            }
            fn revert(&self, _applied: &AppliedFault) -> Result<(), AgentError> {
                *self
                    .reverted
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
                Ok(())
            }
            fn adapter_kind(&self) -> &'static str {
                "recorder"
            }
        }

        let recorder = Arc::new(Recorder::default());
        {
            let mut cleanup = Cleanup::new();
            for _ in 0..3 {
                let plan = sample_plan("recorder");
                let applied = recorder
                    .apply(&plan, &SafetyGuard::new())
                    .expect("recorder apply is infallible");
                cleanup.register(applied, recorder.clone() as Arc<dyn TargetAdapter>);
            }
            assert_eq!(cleanup.len(), 3);
            // Drop runs here.
        }
        let reverted = *recorder
            .reverted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            reverted, 3,
            "expected every registered fault to be reverted on drop"
        );
    }

    #[test]
    fn cleanup_registry_revert_unknown_id_is_error() {
        let mut cleanup = Cleanup::new();
        let err = cleanup
            .revert(malcolm_agent::cleanup::AppliedId(99_999))
            .expect_err("revert of unknown id must error");
        assert!(matches!(
            err,
            AgentError::UnknownAppliedFault { id: 99_999 }
        ));
    }

    #[test]
    fn agent_error_messages_render_distinct_non_secret_strings() {
        // The blast-radius of `AgentError::Display` is bounded: no
        // variant should include a secret-shaped substring. We assert
        // the messages are distinct and free of common secret patterns.
        let cases: Vec<AgentError> = vec![
            AgentError::NotArmed,
            AgentError::ArmFlagMissing,
            AgentError::ExplicitOptInMissing,
            AgentError::TargetNotAllowed {
                rule: "self_pid",
                target: "pid:1".to_owned(),
            },
            AgentError::AdapterFailure {
                adapter: "process",
                reason: "permission denied".to_owned(),
            },
            AgentError::UnknownAppliedFault { id: 7 },
        ];
        let mut rendered: Vec<String> = cases.iter().map(ToString::to_string).collect();
        rendered.sort();
        rendered.dedup();
        assert_eq!(
            rendered.len(),
            cases.len(),
            "every AgentError variant must produce a distinct message; got {rendered:?}"
        );
        for msg in &rendered {
            let lowered = msg.to_ascii_lowercase();
            assert!(
                !lowered.contains("password")
                    && !lowered.contains("secret")
                    && !lowered.contains("token")
                    && !lowered.contains("api_key"),
                "error message must not embed a secret-shaped substring: {msg}"
            );
        }
    }

    #[test]
    fn null_adapter_revert_is_idempotent() {
        let adapter = NullAdapter::new();
        let plan = sample_plan("null");
        let applied = adapter
            .apply(&plan, &SafetyGuard::new())
            .expect("null apply never fails");
        // Two consecutive reverts both succeed — the runtime may
        // re-issue revert during shutdown.
        adapter.revert(&applied).expect("first revert succeeds");
        adapter.revert(&applied).expect("second revert succeeds");
    }
}
