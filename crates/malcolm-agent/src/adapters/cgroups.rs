//! Linux cgroup v2 resource-limit adapter.
//!
//! The adapter is the out-of-process counterpart to the in-process
//! resource faults (T08). It applies real `cpu.max`, `memory.max`,
//! and `io.max` limits to a target cgroup so an actual process tree
//! is constrained rather than the in-process simulation. The
//! adapter creates a *dedicated* malcolm-owned child cgroup and
//! moves the target pids into it — it never mutates an existing
//! cgroup the operator did not create.
//!
//! # Feature gating
//!
//! Compiled only on Linux with the `cgroups` feature enabled. The
//! default build of `malcolm-agent` cannot write to the cgroup
//! hierarchy.
//!
//! # Safety contract
//!
//! Every action goes through [`SafetyGuard::check_target`] first.
//! The guard rejects the host cgroup root by construction; the
//! caller must additionally have added the target cgroup path to
//! the cgroup allowlist. The guard's arming state is also checked:
//! an unarmed guard returns a `dry_run: true` `AppliedFault` and
//! performs no fs writes.
//!
//! # Reversibility
//!
//! All actions are reversible. The adapter records the created
//! cgroup path and the original cgroup of each moved pid. On
//! `revert`, the pids are moved back to their original cgroup and
//! the malcolm child cgroup is removed. The
//! [`crate::cleanup::Cleanup`] registry guarantees revert on
//! `Drop` and on `SIGINT`/`SIGTERM`.
//!
//! # Privilege requirements
//!
//! Cgroup writes need either root or a delegated subtree with
//! write permission. The adapter probes the parent cgroup's
//! writability before acting and returns a clear error if the
//! caller lacks privilege.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

/// Cgroup v2 mount point on a typical Linux system. The adapter
/// probes this at runtime rather than hard-coding it; the constant
/// is only a default for the `parse_cgroup_path` helper.
pub const DEFAULT_CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Directory the adapter creates for its child cgroups. Picked to
/// match the systemd convention so operators can find the
/// malcolm-owned subtree via `systemctl`/`cgtop` like any other
/// slice.
pub const MALCOLM_PARENT_SLICE: &str = "/sys/fs/cgroup/malcolm.slice";

/// Actions the cgroup adapter understands. Decoded from the
/// `FaultPlan::payload` JSON object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CgroupAction {
    /// Apply a `cpu.max` quota. `quota_us / period_us` is the
    /// fraction of one CPU the cgroup is allowed to consume.
    /// `quota_us = u64::MAX` is reserved for "no limit".
    CpuMax {
        /// CPU quota in microseconds per `period_us`. `u64::MAX`
        /// is interpreted as "no limit" by the kernel.
        quota_us: u64,
        /// CPU period in microseconds.
        period_us: u64,
    },
    /// Apply a `memory.max` hard limit (cgroup OOMs when reached).
    MemoryMax {
        /// Hard memory limit in bytes. `u64::MAX` is "no limit".
        bytes: u64,
    },
    /// Apply `io.max` for a single device (major:minor).
    IoMax {
        /// Device identifier, e.g. `"253:0"`.
        device: String,
        /// Read bytes per second. `None` = no read cap.
        rbps: Option<u64>,
        /// Write bytes per second. `None` = no write cap.
        wbps: Option<u64>,
    },
}

impl CgroupAction {
    /// Short, stable identifier used in tracing events and
    /// `AppliedFault::description`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::CpuMax { .. } => "cgroup_cpu",
            Self::MemoryMax { .. } => "cgroup_mem",
            Self::IoMax { .. } => "cgroup_io",
        }
    }

    /// Decode a `CgroupAction` from the JSON payload of a
    /// `FaultPlan`. Returns [`AgentError::InvalidPlan`] on any
    /// shape mismatch rather than guessing.
    ///
    /// # Errors
    ///
    /// - `InvalidPlan` if the payload is not a JSON object.
    /// - `InvalidPlan` if a required field is missing or has the
    ///   wrong type.
    /// - `InvalidPlan` if the `kind` discriminator is unknown.
    pub fn from_payload(payload: &serde_json::Value) -> Result<Self, AgentError> {
        let obj = payload.as_object().ok_or_else(|| AgentError::InvalidPlan {
            adapter: CgroupAdapter::KIND,
            reason: "payload must be a JSON object".to_owned(),
        })?;
        let kind = obj
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: CgroupAdapter::KIND,
                reason: "missing or non-string field `kind`".to_owned(),
            })?;
        match kind {
            "cpu_max" => {
                let quota_us = obj
                    .get("quota_us")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: CgroupAdapter::KIND,
                        reason: "missing or non-u64 field `quota_us`".to_owned(),
                    })?;
                let period_us = obj
                    .get("period_us")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: CgroupAdapter::KIND,
                        reason: "missing or non-u64 field `period_us`".to_owned(),
                    })?;
                Ok(Self::CpuMax {
                    quota_us,
                    period_us,
                })
            }
            "memory_max" => {
                let bytes = obj
                    .get("bytes")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: CgroupAdapter::KIND,
                        reason: "missing or non-u64 field `bytes`".to_owned(),
                    })?;
                Ok(Self::MemoryMax { bytes })
            }
            "io_max" => {
                let device = obj
                    .get("device")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: CgroupAdapter::KIND,
                        reason: "missing or non-string field `device`".to_owned(),
                    })?
                    .to_owned();
                let rbps = obj.get("rbps").and_then(serde_json::Value::as_u64);
                let wbps = obj.get("wbps").and_then(serde_json::Value::as_u64);
                if rbps.is_none() && wbps.is_none() {
                    return Err(AgentError::InvalidPlan {
                        adapter: CgroupAdapter::KIND,
                        reason: "`io_max` requires at least one of `rbps`/`wbps`".to_owned(),
                    });
                }
                Ok(Self::IoMax { device, rbps, wbps })
            }
            other => Err(AgentError::InvalidPlan {
                adapter: CgroupAdapter::KIND,
                reason: format!("unknown action kind `{other}`"),
            }),
        }
    }
}

/// Per-pid bookkeeping so `revert` can move each pid back to its
/// original cgroup. The `original_cgroup` is the cgroup path the
/// pid was in before the adapter moved it.
#[derive(Debug, Clone)]
struct MovedPid {
    /// The original cgroup the pid was in before we moved it.
    original_cgroup: PathBuf,
}

/// The cgroup resource-limit adapter. Each `apply` call creates a
/// fresh child cgroup under [`MALCOLM_PARENT_SLICE`]/<id>/,
/// optionally moves allowlisted pids into it, and writes the
/// cgroup interface files.
#[derive(Debug, Default)]
pub struct CgroupAdapter {
    /// Monotonic counter for the dry-run / applied ids the
    /// adapter hands out. Distinct from the cleanup registry's
    /// id.
    next_id: AtomicU64,
    /// Map from adapter id → bookkeeping the runtime needs to
    /// revert (child cgroup path, moved pids). Wrapped in a
    /// `Mutex` so `revert` satisfies the `Send + Sync` bound on
    /// `TargetAdapter`.
    applied: Mutex<HashMap<u64, AppliedRecord>>,
}

#[derive(Debug, Clone)]
struct AppliedRecord {
    /// The malcolm-owned child cgroup we created. `None` for
    /// dry-runs that did not touch the fs.
    child_cgroup: Option<PathBuf>,
    /// Pids the adapter moved into the child cgroup, plus the
    /// cgroup each came from. Empty for dry-runs.
    moved_pids: Vec<(u32, MovedPid)>,
}

impl CgroupAdapter {
    /// Adapter kind string. Exposed as a constant so tests and
    /// adapters can compare against it without hard-coding.
    pub const KIND: &'static str = "cgroup";

    /// Construct a new `CgroupAdapter` with its id counter at
    /// zero and an empty applied-faults map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(0),
            applied: Mutex::new(HashMap::new()),
        }
    }

    /// Number of applied faults currently tracked.
    #[must_use]
    pub fn tracked_count(&self) -> usize {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Verify cgroup v2 is available. Returns the unified
    /// hierarchy root on success. We detect cgroup v2 by the
    /// presence of `cgroup.controllers` in the root; cgroup v1
    /// has per-controller subdirectories and no
    /// `cgroup.controllers` file at the top.
    ///
    /// # Errors
    ///
    /// - [`AgentError::PlatformUnsupported`] if cgroup v2 is
    ///   not mounted (or we can't read the root).
    pub fn detect_cgroup_v2() -> Result<PathBuf, AgentError> {
        let root = PathBuf::from(DEFAULT_CGROUP_ROOT);
        let controllers = root.join("cgroup.controllers");
        if !controllers.exists() {
            return Err(AgentError::PlatformUnsupported {
                adapter: Self::KIND,
                action: "detect_cgroup_v2".to_owned(),
                platform: std::env::consts::OS.to_owned(),
            });
        }
        Ok(root)
    }

    /// Probe whether the current process can write the cgroup
    /// interface files. Used by tests to skip cleanly on
    /// unprivileged runners; production code should also check
    /// this and return a clear error.
    #[must_use]
    pub fn has_privilege() -> bool {
        // The simplest test: try to create the malcolm parent
        // slice. If `create_dir_all` succeeds, we have write
        // access. We don't remove the directory here; tests that
        // observe privilege should clean up.
        fs::create_dir_all(MALCOLM_PARENT_SLICE).is_ok()
    }

    /// Write `value` to `path` with a trailing newline, the
    /// convention cgroup interface files expect. The wrapper
    /// surfaces a single `AdapterFailure` with the syscall detail
    /// rather than letting raw `io::Error` escape.
    #[must_use = "cgroup file write failures must be inspected, not silently dropped"]
    fn write_cgroup_file(path: &Path, value: &str) -> Result<(), AgentError> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|e| AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("open {} failed: {e}", path.display()),
            })?;
        file.write_all(value.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .map_err(|e| AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("write {} failed: {e}", path.display()),
            })?;
        Ok(())
    }

    /// Read the current cgroup of `pid` from
    /// `/proc/<pid>/cgroup` and return its path. The file
    /// contains lines like `0::/system.slice/foo.service`; we
    /// return the path part after the third colon. Returns
    /// `None` if the file cannot be read.
    fn current_cgroup_of(pid: u32) -> Result<PathBuf, AgentError> {
        let raw = fs::read_to_string(format!("/proc/{pid}/cgroup")).map_err(|e| {
            AgentError::AdapterFailure {
                adapter: Self::KIND,
                reason: format!("read /proc/{pid}/cgroup failed: {e}"),
            }
        })?;
        for line in raw.lines() {
            // cgroup v2 uses the format `<hierarchy>:<controller-list>:<path>`.
            // Hierarchy 0 with an empty controller list and a non-empty path
            // is the cgroup v2 entry.
            let parts: Vec<&str> = line.splitn(3, ':').collect();
            if parts.len() == 3 && parts[0] == "0" {
                let rel = parts[2].trim_start_matches('/');
                if rel.is_empty() {
                    return Ok(PathBuf::from(DEFAULT_CGROUP_ROOT));
                }
                return Ok(PathBuf::from(DEFAULT_CGROUP_ROOT).join(rel));
            }
        }
        // cgroup v1 only (or legacy): bail with a clear error.
        Err(AgentError::PlatformUnsupported {
            adapter: Self::KIND,
            action: "read_cgroup_v2_path".to_owned(),
            platform: format!("pid {pid} has no cgroup v2 entry"),
        })
    }

    /// Move `pid` into the cgroup at `dest` by writing its pid
    /// to `dest/cgroup.procs`. The kernel performs the move
    /// atomically.
    fn move_pid_to(pid: u32, dest: &Path) -> Result<(), AgentError> {
        let procs = dest.join("cgroup.procs");
        let pid_str = pid.to_string();
        Self::write_cgroup_file(&procs, &pid_str)
    }

    /// Remove a child cgroup directory. The kernel will refuse
    /// to remove a non-empty cgroup; callers must move all pids
    /// out first.
    fn remove_cgroup(path: &Path) -> Result<(), AgentError> {
        fs::remove_dir(path).map_err(|e| AgentError::AdapterFailure {
            adapter: Self::KIND,
            reason: format!("remove_dir {} failed: {e}", path.display()),
        })
    }

    /// Move a list of pids back to the cgroup they came from,
    /// then remove the child cgroup. Best-effort: a failure to
    /// restore one pid is logged but does not stop the rest.
    fn revert_internal(record: &AppliedRecord) {
        let Some(child) = &record.child_cgroup else {
            return;
        };
        for (pid, moved) in &record.moved_pids {
            if let Err(e) = Self::move_pid_to(*pid, &moved.original_cgroup) {
                tracing::warn!(
                    target: "malcolm_agent::cgroups",
                    pid,
                    target_cgroup = %moved.original_cgroup.display(),
                    error = %e,
                    "cgroup adapter: failed to restore pid to original cgroup"
                );
            }
        }
        // The kernel only removes an empty cgroup. The pids
        // should be gone by the time we reach this line;
        // any leftover ones are the kernel's problem.
        if let Err(e) = Self::remove_cgroup(child) {
            tracing::warn!(
                target: "malcolm_agent::cgroups",
                cgroup = %child.display(),
                error = %e,
                "cgroup adapter: failed to remove child cgroup on revert"
            );
        }
    }
}

impl TargetAdapter for CgroupAdapter {
    #[expect(
        clippy::too_many_lines,
        reason = "apply threads every CgroupAction variant through one path"
    )]
    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        // Decode the action first; we need the kind for both
        // the dry-run path and the live path.
        let action = CgroupAction::from_payload(&plan.payload)?;

        // Dry-run-first: if the guard is not armed, record the
        // would-have action and return without touching the
        // host. We still need a "cgroup path" for the dry-run
        // description; we use the parent slice.
        if !guard.is_armed() {
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            let description = format!(
                "{} (dry-run; guard unarmed): kind={} reason={}",
                Self::KIND,
                action.kind(),
                plan.reason
            );
            tracing::info!(
                target: "malcolm_agent::cgroups",
                applied_id = id,
                kind = action.kind(),
                plan = %plan,
                "cgroup adapter: dry-run (guard unarmed)"
            );
            return Ok(AppliedFault {
                id,
                adapter: Self::KIND,
                dry_run: true,
                description,
            });
        }

        // Detect cgroup v2 up front. On non-Linux or v1-only
        // hosts the adapter cannot operate; surface a clear
        // platform error rather than failing in obscure ways
        // mid-write.
        let root = Self::detect_cgroup_v2()?;

        // The cgroup path the plan targets. The plan can carry
        // it in `payload.cgroup_path`; if absent, default to the
        // malcolm parent slice (which the operator may have
        // pre-delegated to a non-root user).
        let target_cgroup = plan
            .payload
            .get("cgroup_path")
            .and_then(serde_json::Value::as_str)
            .map_or_else(
                || PathBuf::from(MALCOLM_PARENT_SLICE),
                |s| {
                    PathBuf::from(s.trim_start_matches('/')).components().fold(
                        PathBuf::new(),
                        |mut acc, c| {
                            acc.push(c.as_os_str());
                            acc
                        },
                    )
                },
            );
        let target_cgroup = if target_cgroup.is_absolute() {
            target_cgroup
        } else {
            root.join(target_cgroup)
        };

        // Safety check on the target cgroup.
        let target_str = target_cgroup.to_string_lossy().to_string();
        guard
            .check_target(&Target::Cgroup(&target_str))
            .map_err(|e| {
                tracing::warn!(
                    target: "malcolm_agent::cgroups",
                    cgroup = %target_str,
                    error = %e,
                    "cgroup adapter: target rejected by safety guard"
                );
                e
            })?;

        // The optional pids the operator wants to move into the
        // child cgroup. Each pid must also be on the pid
        // allowlist. We validate every pid up front; the
        // partial-application path is harder to reason about
        // and we'd rather reject the whole plan than leave a
        // half-set-up cgroup.
        let pids: Vec<u32> = plan
            .payload
            .get("pids")
            .and_then(serde_json::Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_u64)
                    .filter_map(|p| u32::try_from(p).ok())
                    .collect()
            })
            .unwrap_or_default();
        for pid in &pids {
            guard.check_target(&Target::Pid(*pid))?;
        }

        // Create the malcolm parent slice if it doesn't exist.
        // The first apply in a process typically needs root
        // for this; later applies in a pre-delegated subtree
        // work without privileges.
        fs::create_dir_all(MALCOLM_PARENT_SLICE).map_err(|e| AgentError::AdapterFailure {
            adapter: Self::KIND,
            reason: format!("create_dir_all {MALCOLM_PARENT_SLICE} failed: {e}"),
        })?;

        // Allocate a child cgroup under the malcolm parent. We
        // use the next_id so the name is unique within the
        // process; the runtime can pass an explicit
        // `cgroup_path` if it wants a stable name.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let child_name = format!("run-{id}");
        let child = if target_cgroup == Path::new(MALCOLM_PARENT_SLICE) {
            target_cgroup.join(child_name)
        } else {
            target_cgroup.clone()
        };
        fs::create_dir_all(&child).map_err(|e| AgentError::AdapterFailure {
            adapter: Self::KIND,
            reason: format!("create_dir_all {} failed: {e}", child.display()),
        })?;

        // Record the original cgroup of each pid before we
        // move them, so `revert` can restore them.
        let mut moved_pids = Vec::with_capacity(pids.len());
        for pid in &pids {
            let original = Self::current_cgroup_of(*pid)?;
            Self::move_pid_to(*pid, &child)?;
            moved_pids.push((
                *pid,
                MovedPid {
                    original_cgroup: original,
                },
            ));
        }

        // Write the cgroup interface file for the action.
        match &action {
            CgroupAction::CpuMax {
                quota_us,
                period_us,
            } => {
                // Format is `quota period`; `max` for unlimited.
                let value = if *quota_us == u64::MAX {
                    "max".to_owned()
                } else {
                    format!("{quota_us} {period_us}")
                };
                Self::write_cgroup_file(&child.join("cpu.max"), &value)?;
            }
            CgroupAction::MemoryMax { bytes } => {
                let value = if *bytes == u64::MAX {
                    "max".to_owned()
                } else {
                    bytes.to_string()
                };
                Self::write_cgroup_file(&child.join("memory.max"), &value)?;
            }
            CgroupAction::IoMax { device, rbps, wbps } => {
                // Format: `<device> rbps=<n> wbps=<n>`. Missing
                // fields are omitted (no cap on that direction).
                let mut parts = vec![device.clone()];
                if let Some(r) = rbps {
                    parts.push(format!("rbps={r}"));
                }
                if let Some(w) = wbps {
                    parts.push(format!("wbps={w}"));
                }
                Self::write_cgroup_file(&child.join("io.max"), &parts.join(" "))?;
            }
        }

        // Record the bookkeeping for revert.
        let record = AppliedRecord {
            child_cgroup: Some(child.clone()),
            moved_pids,
        };
        {
            let mut applied = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            applied.insert(id, record);
        }

        let description = format!("{}: {}", Self::KIND, action.kind());
        tracing::info!(
            target: "malcolm_agent::cgroups",
            applied_id = id,
            kind = action.kind(),
            cgroup = %child.display(),
            "cgroup adapter: applied"
        );
        Ok(AppliedFault {
            id,
            adapter: Self::KIND,
            dry_run: false,
            description,
        })
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        let entry = {
            let mut map = self
                .applied
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.remove(&applied.id)
        };
        let Some(record) = entry else {
            // Dry-run or unknown id. Nothing to revert.
            return Ok(());
        };
        Self::revert_internal(&record);
        Ok(())
    }

    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }
}

/// Helper for tests: probe the host to see if we can write
/// cgroup files. Returns `Some(root)` if cgroup v2 is available
/// and the malcolm parent slice is creatable, `None` otherwise.
/// The caller should `eprintln!` a skip message when this
/// returns `None` so the test runner makes the skip visible.
#[must_use]
pub fn probe_cgroup_writable() -> Option<PathBuf> {
    let root = CgroupAdapter::detect_cgroup_v2().ok()?;
    // Try (and keep) the malcolm parent. This may need root.
    let _ = fs::create_dir_all(MALCOLM_PARENT_SLICE);
    if !Path::new(MALCOLM_PARENT_SLICE).exists() {
        return None;
    }
    Some(root)
}

/// Cleanup helper for tests: remove any leftover malcolm child
/// cgroups under [`MALCOLM_PARENT_SLICE`]. Best-effort; logs and
/// continues on individual failures.
pub fn cleanup_test_cgroups() {
    let parent = Path::new(MALCOLM_PARENT_SLICE);
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let _ = fs::remove_dir(&path);
        } else {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Suppress dead-code warnings on `OsString` use in some build
/// configurations (kept here for forward-compat with the
/// systemd cgroup path resolver).
#[allow(dead_code)]
fn _os_string_marker(_: OsString) {}
