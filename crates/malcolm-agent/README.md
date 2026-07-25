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
| `cgroups`     | cgroup v2 resource-limit adapter              | T35  |
| `netem`       | Linux `tc`/`netem` real network fault adapter | T36  |
| `syscall`     | seccomp / syscall filter adapter              | T37  |
| `kubernetes`  | pod / namespace / container adapter           | T38  |

## Process control (feature `process`)

The process-control adapter turns the in-process `Fault` decisions
into real OS signals. Every action goes through `SafetyGuard`
*before* any signal is delivered. The adapter is Unix-only and
compiled behind the `process` feature.

### Enable

```toml
[dependencies]
malcolm-agent = { version = "0.6", features = ["process"] }
```

### Actions

The adapter consumes `FaultPlan` payloads of the form:

```json
{ "kind": "signal",     "pid": 1234, "signal": "SIGUSR1" }
{ "kind": "terminate",  "pid": 1234, "grace_ms": 500 }
{ "kind": "pause",      "pid": 1234 }
{ "kind": "resume",     "pid": 1234 }
```

`signal` and `terminate` are irreversible. `pause` is reversible —
`Cleanup::revert` (or `Drop` of the registry) sends `SIGCONT` to
the paused pid, so a test that crashes mid-pause never leaves a
target stopped. `resume` is its own action with no follow-up.

### Graceful terminate

`terminate` sends `SIGTERM`, polls liveness with `kill(pid, None)`
on a 10 ms backoff (never busy-spin), and escalates to `SIGKILL`
when `grace_ms` elapses. The total wait is bounded by `grace_ms`;
the adapter does not block longer than that.

### Privileges

Signaling processes you do not own needs either a matching uid or
`CAP_KILL`. The adapter does not escalate privileges; if the OS
denies a `kill(2)`, the adapter returns
`AgentError::AdapterFailure { .. }` with the kernel's error string.

## cgroups v2 resource limits (feature `cgroups`)

The cgroup adapter turns the in-process `CpuThrottle` /
`MemoryPressure` primitives (T08) into real OS limits. It writes
the cgroup v2 interface files (`cpu.max`, `memory.max`, `io.max`)
so a target process tree is constrained by the kernel rather than
the simulation. Linux-only.

### Enable

```toml
[dependencies]
malcolm-agent = { version = "0.6", features = ["cgroups"] }
```

### Actions

The adapter consumes `FaultPlan` payloads of the form:

```json
{ "kind": "cpu_max",    "quota_us": 50000, "period_us": 100000 }
{ "kind": "memory_max", "bytes":    33554432 }
{ "kind": "io_max",     "device": "253:0", "rbps": 1000000, "wbps": 2000000 }
```

The optional `cgroup_path` and `pids` fields route the action to a
specific malcolm-owned child cgroup and move a list of allowlisted
pids into it.

### Malcolm-owned parent slice

The adapter creates a *dedicated* child cgroup under
`/sys/fs/cgroup/malcolm.slice/run-N/` rather than mutating an
existing cgroup the operator did not create. The slice name matches
the systemd convention so operators can find the subtree via
`cgtop` or `systemd-cgtop`.

### Reversibility

`revert` moves pids back to their original cgroup and removes the
child cgroup. The `Cleanup` registry guarantees revert on `Drop`
and on `SIGINT`/`SIGTERM`, so a crashed run never leaves a
throttled subtree orphaned.

### Privilege requirements

Cgroup writes need either root or a delegated subtree with write
permission. The adapter probes the parent cgroup's writability
before acting and returns a clear error if the caller lacks
privilege. The integration tests skip cleanly on unprivileged
runners via the `probe_cgroup_writable` helper.

## Real network faults (feature `netem`)

The netem adapter turns the in-process T07 network faults
(`LatencySpike`, `PacketLoss`, `BandwidthThrottle`,
`NetworkPartition`) into real Linux traffic-control impairments
on a named interface. It shells out to the `tc` binary from
iproute2 — no `unsafe`, no new dependency. Linux-only.

### Enable

```toml
[dependencies]
malcolm-agent = { version = "0.6", features = ["netem"] }
```

### Actions

```json
{ "kind": "latency",   "interface": "eth0", "mean_ms": 100, "jitter_ms": 20, "correlation": 25 }
{ "kind": "loss",      "interface": "eth0", "percent": 5,  "correlation": 50 }
{ "kind": "corrupt",   "interface": "eth0", "percent": 0.5 }
{ "kind": "reorder",   "interface": "eth0", "percent": 2,  "correlation": 25 }
{ "kind": "rate",      "interface": "eth0", "bps": 1000000 }
{ "kind": "partition", "interface": "eth0" }
```

The optional `watchdog_ms` field spawns a background thread that
reverts the impairment after the timeout — a safety net for
wedged test processes that have not yet dropped their
`Cleanup` registry.

### Safety contract

- The interface is checked against the iface allowlist via
  `SafetyGuard::check_target(Target::Iface(iface))` before any
  `tc` call.
- The default-route interface is rejected by the guard unless
  the operator has explicitly added it.
- Parameters are validated (percentages in `[0, 100]`,
  correlations in `[0, 100]`, NaN/Inf rejected) before any
  shell-out. A malformed plan never reaches `tc`.

### Snapshot and restore

`apply` records the existing root qdisc of the interface
before any change. `revert` removes the netem qdisc and
replays the snapshot. The `Cleanup` registry guarantees
revert on `Drop` and on `SIGINT`/`SIGTERM`. `revert` is
idempotent: a missing qdisc is treated as success so a
double-revert from a noisy cleanup does not loop.

### Privilege requirements

`tc` requires `CAP_NET_ADMIN`. The adapter treats absence of
the binary or the privilege as a clean `PlatformUnsupported`
error; the integration tests skip cleanly on unprivileged
runners via `probe_netem_writable`.

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
- [`adapters/process.rs`](src/adapters/process.rs) — process control (kill / signal / pause / resume), feature `process`.
- [`adapters/cgroups.rs`](src/adapters/cgroups.rs) — cgroup v2 resource limits (cpu.max / memory.max / io.max), feature `cgroups`, Linux-only.
- [`adapters/netem.rs`](src/adapters/netem.rs) — Linux `tc`/`netem` real network fault adapter, feature `netem`, Linux-only.
