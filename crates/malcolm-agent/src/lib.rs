//! # malcolm-agent
//!
//! Optional out-of-process fault adapters for the malcolm chaos
//! engineering library. **Real OS side effects live here, behind a
//! [`SafetyGuard`] — use on controlled hosts only.**
//!
//! ## What this crate adds
//!
//! The math core (`malcolm-core`) and the simulation library
//! (`malcolm`) stay side-effect-free. `malcolm-agent` is the only
//! crate in the workspace that performs real, out-of-process mutations
//! (process kills, network qdisc rewrites, cgroup limits, container
//! targeting). The dependency arrow points one way:
//!
//! ```text
//!   malcolm-core  ◄──  malcolm  ◄──  malcolm-agent
//!   (math)          (assembly)    (real OS side effects)
//! ```
//!
//! ## Port + adapters
//!
//! [`TargetAdapter`] is the single port trait every real-world
//! adapter implements. The default build ships [`NullAdapter`],
//! which always reports `dry_run: true` and never mutates the host.
//! Real adapters (T34–T38) live behind per-adapter feature flags
//! and are off by default.
//!
//! ## Safety interlocks
//!
//! Every adapter must consult [`SafetyGuard`] before applying a
//! `FaultPlan`. The guard refuses to arm unless **both** the
//! `MALCOLM_AGENT_ARM=1` environment flag is set **and** the caller
//! passed the explicit `i_understand_the_blast_radius: true` boolean
//! to [`SafetyGuard::arm`]. A small set of obviously dangerous
//! targets (pid 1, the current process, the host cgroup root) is
//! rejected by construction.
//!
//! The [`Cleanup`] registry reverts every registered applied fault
//! on `Drop`, on `SIGINT`, and on `SIGTERM`. A crashed test run
//! cannot leave a host partitioned or throttled.
//!
//! ## Unsafe-policy deviation
//!
//! The workspace sets `unsafe_code = "forbid"`. `malcolm-agent`
//! overrides to `unsafe_code = "deny"` because real OS adapters
//! transitively depend on crates that wrap syscalls via `unsafe`
//! (e.g. `nix`, `caps`, `cgroups-rs`, `rtnetlink`). Every direct
//! `unsafe` block in this crate carries a `// SAFETY:` justification
//! and is restricted to async-signal-safe operations. The deviation
//! is scoped to this crate; the rest of the workspace remains
//! `forbid(unsafe_code)`.
//!
//! # Example
//!
//! ```rust
//! use malcolm_agent::cleanup::Cleanup;
//! use malcolm_agent::null::NullAdapter;
//! use malcolm_agent::safety::SafetyGuard;
//! use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
//! use std::sync::Arc;
//!
//! // Build an unarmed guard. apply() always reports dry_run: true
//! // for NullAdapter, so the wiring can be exercised without the
//! // MALCOLM_AGENT_ARM env flag set.
//! let guard = SafetyGuard::new();
//! let adapter: Arc<dyn TargetAdapter> = Arc::new(NullAdapter::new());
//! let plan = FaultPlan {
//!     adapter: "null".to_owned(),
//!     payload: serde_json::json!({ "kind": "noop" }),
//!     reason: "smoke-test".to_owned(),
//! };
//! let mut cleanup = Cleanup::new();
//! let applied = adapter.apply(&plan, &guard).expect("null apply never fails");
//! assert!(applied.dry_run);
//! let id = cleanup.register(applied, Arc::clone(&adapter));
//! cleanup.revert(id).expect("null revert is a no-op");
//! assert!(cleanup.is_empty());
//! ```

#![warn(missing_docs)]
#![warn(unreachable_pub)]

pub mod adapter;
pub mod cleanup;
pub mod error;
pub mod null;
pub mod safety;

// Re-export the public surface at the crate root so consumers can
// write `malcolm_agent::TargetAdapter` rather than reaching into
// each module.
pub use adapter::{AppliedFault, FaultPlan, TargetAdapter};
pub use cleanup::{AppliedId, Cleanup};
pub use error::AgentError;
pub use null::NullAdapter;
pub use safety::{ARM_ENV_FLAG, SafetyGuard};
