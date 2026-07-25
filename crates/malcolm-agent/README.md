# malcolm-agent

> **Real side effects — opt-in, use on controlled hosts only.**

`malcolm-agent` is the optional out-of-process fault-injection layer
for the [malcolm](../README.md) chaos engineering library. It bridges
malcolm's in-process fault *primitives* to **real, out-of-process side
effects** on an actual target — kill a process, apply real network
latency, throttle real CPU/memory, target a container.

## Dependency arrow

The arrow points one way. The math core (`malcolm-core`) and the
simulation library (`malcolm`) stay side-effect-free; this crate is
the only place where real OS mutations land.

```text
  malcolm-core  ◄──  malcolm  ◄──  malcolm-agent
  (math)          (assembly)    (real OS side effects)
```

`malcolm` does **not** depend on `malcolm-agent`. To use the agent,
add it as a separate dependency in your `Cargo.toml` and gate it on a
feature flag in your own build.

## Safety interlocks

Every adapter in this crate must consult
[`SafetyGuard`](src/safety.rs) before touching the host. The guard
refuses to arm unless **both** are true:

1. The environment variable `MALCOLM_AGENT_ARM=1` is set.
2. The caller passed `i_understand_the_blast_radius: true` to
   `SafetyGuard::arm`. The named-parameter pattern is part of the
   contract; a bare `true` from an `if`-expression does not satisfy
   it.

Without either, the guard reports `AgentError::NotArmed` and any
adapter that consults it must return a `dry_run: true` `AppliedFault`
and perform no side effect — mirroring the in-process `Fault::dry_run`
contract.

The guard also refuses a small set of obviously dangerous targets by
construction:

- **pid 1** (the init process).
- The current process and its parent.
- The host cgroup root (`"/"`).
- The default route interface, unless explicitly named.

Adapters consult `SafetyGuard::require_pid` / `require_cgroup` /
`require_iface` / `require_container` to enforce these rules.

## Dead-man cleanup

Every applied fault is registered with the
[`Cleanup`](src/cleanup.rs) registry. On `Drop`, on `SIGINT`, and on
`SIGTERM`, the registry iterates registered faults in reverse-insertion
order and calls each adapter's `revert` method. A crashed test run
cannot leave a host partitioned or throttled.

The signal handler performs only an `AtomicBool::store`, which is
async-signal-safe. The actual revert runs in a normal thread and at
registry-drop time.

## Unsafe-policy deviation

The workspace sets `unsafe_code = "forbid"`. `malcolm-agent` overrides
to `unsafe_code = "deny"` because real OS adapters transitively depend
on crates that wrap syscalls via `unsafe` (`nix`, `caps`, `cgroups-rs`,
`rtnetlink`, etc.). The deviation is scoped to this crate; the rest of
the workspace remains `forbid(unsafe_code)`. Every direct `unsafe`
block in this crate carries a `// SAFETY:` justification and is
restricted to async-signal-safe operations.

## Feature flags

All real adapters are off by default. The default build compiles only
the `TargetAdapter` port, `SafetyGuard`, `Cleanup`, and `NullAdapter`
— zero OS side effects.

| Feature       | Adds                                          | Task |
|---------------|-----------------------------------------------|------|
| `process`     | Process kill / signal adapter                 | T34  |
| `cgroups`     | cgroup-v1 / cgroup-v2 limit adapter           | T35  |
| `netem`       | Linux `tc` qdisc adapter (latency, loss, …)   | T36  |
| `syscall`     | seccomp / syscall filter adapter              | T37  |
| `kubernetes`  | pod / namespace / container adapter           | T38  |

## Example

```rust
use std::sync::Arc;
use malcolm_agent::adapter::{FaultPlan, TargetAdapter};
use malcolm_agent::cleanup::Cleanup;
use malcolm_agent::null::NullAdapter;
use malcolm_agent::safety::SafetyGuard;

let guard = SafetyGuard::new();
let adapter: Arc<dyn TargetAdapter> = Arc::new(NullAdapter::new());
let plan = FaultPlan {
    adapter: "null".to_owned(),
    payload: serde_json::json!({ "kind": "noop" }),
    reason: "smoke-test".to_owned(),
};
let mut cleanup = Cleanup::new();
let applied = adapter.apply(&plan, &guard)?;
assert!(applied.dry_run);
let id = cleanup.register(applied, Arc::clone(&adapter));
cleanup.revert(id)?;
# Ok::<(), malcolm_agent::error::AgentError>(())
```

## Layout

- [`adapter.rs`](src/adapter.rs) — `TargetAdapter` port + `FaultPlan` / `AppliedFault`.
- [`safety.rs`](src/safety.rs) — `SafetyGuard` + `ARM_ENV_FLAG` + target checks.
- [`cleanup.rs`](src/cleanup.rs) — `Cleanup` registry + signal handling.
- [`error.rs`](src/error.rs) — `AgentError` enum.
- [`null.rs`](src/null.rs) — `NullAdapter`, the always-dry-run default adapter.
