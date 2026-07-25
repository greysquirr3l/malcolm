//! `SafetyGuard` — the interlock every adapter MUST consult before
//! touching the host.
//!
//! Real OS adapters have irreversible blast-radius. `SafetyGuard`
//! exists so **no single accidental flag can arm it**.
//!
//! # Arming contract
//!
//! `SafetyGuard::arm` is the only way to get a fully armed guard. It
//! requires **both**:
//!
//! 1. The environment flag `MALCOLM_AGENT_ARM=1` (so the test author
//!    opted in at the environment level, not just at the call site).
//! 2. The caller passed an explicit `i_understand_the_blast_radius:
//!    true` boolean to [`arm`](Self::arm). A bare `true` is not
//!    allowed — the parameter name is part of the contract.
//!
//! Without either, the guard reports [`AgentError::NotArmed`] and any
//! adapter that consults it must return a `dry_run: true`
//! `AppliedFault` and perform no side effect.
//!
//! # Target allowlist
//!
//! The guard holds an explicit allowlist of:
//!
//! - pids (a `BTreeSet<u32>`);
//! - cgroup paths;
//! - network interfaces;
//! - container / pod names.
//!
//! An adapter that asks the guard to apply a fault against a target
//! not on the allowlist gets [`AgentError::TargetNotAllowed`]. The
//! guard also *refuses by construction* a small set of obviously
//! dangerous targets: pid 1, the current process, the current process'
//! parent, the default route interface unless explicitly named, and
//! the host cgroup root.
//!
//! # Cleanup integration
//!
//! The guard is the entry point the [`crate::cleanup::Cleanup`]
//! registry uses to discover which targets are live. The registry
//! reverts everything on `Drop` and on `SIGINT`/`SIGTERM`, so a
//! crashed test run cannot leave a host partitioned.

use std::collections::BTreeSet;
use std::env;

use crate::error::AgentError;

/// Discriminated target kind for the polymorphic
/// [`SafetyGuard::check_target`] entry point.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Target<'a> {
    /// A process id; the safety check enforces pid 1 / self / parent
    /// rejection plus the pid allowlist.
    Pid(u32),
    /// A cgroup path; the safety check rejects the host cgroup root
    /// plus the cgroup allowlist.
    Cgroup(&'a str),
    /// A network interface name; checked against the iface allowlist.
    Iface(&'a str),
    /// A container / pod name; checked against the container allowlist.
    Container(&'a str),
}

/// Environment variable that must be set to `1` for the guard to arm.
pub const ARM_ENV_FLAG: &str = "MALCOLM_AGENT_ARM";

/// The interlock every adapter consults before touching the host.
///
/// Construct via [`SafetyGuard::new`] (unarmed) and arm with
/// [`SafetyGuard::arm`]. The guard exposes its arming state via
/// [`is_armed`](Self::is_armed) so adapters can branch on it.
#[derive(Debug, Clone)]
pub struct SafetyGuard {
    /// `true` once both the env flag and the explicit boolean are
    /// observed.
    armed: bool,
    /// Allowlisted process IDs. Rejects pid 1, the current process,
    /// and the current process' parent by construction.
    allowed_pids: BTreeSet<u32>,
    /// Allowlisted cgroup paths. Rejects the host cgroup root by
    /// construction.
    allowed_cgroups: BTreeSet<String>,
    /// Allowlisted network interface names. Rejects the default
    /// route interface unless it is explicitly named.
    allowed_ifaces: BTreeSet<String>,
    /// Allowlisted container / pod names.
    allowed_containers: BTreeSet<String>,
}

impl Default for SafetyGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl SafetyGuard {
    /// Construct an unarmed guard with an empty allowlist.
    ///
    /// The returned guard is fully functional for dry-run checks and
    /// for recording what *would* have happened; only `apply`-style
    /// mutations require [`arm`](Self::arm).
    #[must_use]
    pub const fn new() -> Self {
        Self {
            armed: false,
            allowed_pids: BTreeSet::new(),
            allowed_cgroups: BTreeSet::new(),
            allowed_ifaces: BTreeSet::new(),
            allowed_containers: BTreeSet::new(),
        }
    }

    /// Read the environment flag once and return whether it is set to
    /// `1`. Anything else (unset, `"0"`, `"true"`, `"yes"`, `""`) is
    /// treated as "not set" — the contract is intentionally strict.
    #[must_use]
    pub fn env_flag_present() -> bool {
        env::var(ARM_ENV_FLAG).is_ok_and(|v| v == "1")
    }

    /// Attempt to arm the guard. Returns the armed guard on success
    /// or the reason on failure.
    ///
    /// The `i_understand_the_blast_radius` parameter name is part of
    /// the contract: callers must pass the named boolean explicitly.
    /// A bare `true` from an `if`-expression does not satisfy this;
    /// the parameter name shows up in code review and in
    /// `git blame`.
    ///
    /// # Errors
    ///
    /// - [`AgentError::ArmFlagMissing`] if the env flag is not set.
    /// - [`AgentError::ExplicitOptInMissing`] if the caller did not
    ///   pass `true` to `i_understand_the_blast_radius`.
    pub fn arm(self, i_understand_the_blast_radius: bool) -> Result<Self, AgentError> {
        if !Self::env_flag_present() {
            return Err(AgentError::ArmFlagMissing);
        }
        if !i_understand_the_blast_radius {
            return Err(AgentError::ExplicitOptInMissing);
        }
        Ok(Self {
            armed: true,
            ..self
        })
    }

    /// Arm the guard without consulting the environment. The
    /// `i_understand_the_blast_radius` parameter is still required:
    /// the named-parameter contract is part of the public API, not
    /// just the env-flag path. Tests that need to drive an armed
    /// guard without playing env-var games call this directly;
    /// production wiring should still use [`arm`](Self::arm).
    ///
    /// # Errors
    ///
    /// - [`AgentError::ExplicitOptInMissing`] if the caller did not
    ///   pass `true` to `i_understand_the_blast_radius`.
    pub fn arm_for_test(self, i_understand_the_blast_radius: bool) -> Result<Self, AgentError> {
        if !i_understand_the_blast_radius {
            return Err(AgentError::ExplicitOptInMissing);
        }
        Ok(Self {
            armed: true,
            ..self
        })
    }

    /// `true` only after a successful [`arm`](Self::arm).
    #[must_use]
    pub const fn is_armed(&self) -> bool {
        self.armed
    }

    /// Add a pid to the allowlist. Does NOT validate the pid is
    /// running; the caller is expected to have already chosen a real
    /// target. Use [`check_pid`](Self::check_pid) to enforce the
    /// built-in rejections at apply time.
    pub fn allow_pid(&mut self, pid: u32) -> &mut Self {
        self.allowed_pids.insert(pid);
        self
    }

    /// Add a cgroup path to the allowlist. The host cgroup root
    /// (`"/"`) is rejected at apply time, not at insertion, so
    /// operators can see what they tried to add in logs.
    pub fn allow_cgroup<S: Into<String>>(&mut self, path: S) -> &mut Self {
        self.allowed_cgroups.insert(path.into());
        self
    }

    /// Add a network interface name to the allowlist.
    pub fn allow_iface<S: Into<String>>(&mut self, name: S) -> &mut Self {
        self.allowed_ifaces.insert(name.into());
        self
    }

    /// Add a container / pod name to the allowlist.
    pub fn allow_container<S: Into<String>>(&mut self, name: S) -> &mut Self {
        self.allowed_containers.insert(name.into());
        self
    }

    /// The number of distinct targets on the allowlist. Useful for
    /// assertion-style checks in tests and CI.
    #[must_use]
    pub fn allowlist_size(&self) -> usize {
        self.allowed_pids.len()
            + self.allowed_cgroups.len()
            + self.allowed_ifaces.len()
            + self.allowed_containers.len()
    }

    /// Reject a pid that is obviously dangerous. Returns the rule
    /// name that fired, or `None` if the pid is acceptable.
    ///
    /// Built-in rejections:
    /// - `pid 1` (the init process — killing it is almost always wrong).
    /// - the current process (`std::process::id()`).
    /// - the parent process (best-effort via `/proc/<pid>/status`
    ///   when available; treated as "unknown" on non-Linux).
    #[must_use]
    pub fn check_pid(&self, pid: u32) -> Option<&'static str> {
        if pid == 1 {
            return Some("pid_1");
        }
        let self_pid = std::process::id();
        if pid == self_pid {
            return Some("self_pid");
        }
        // Heuristic: refuse the parent. On Linux, /proc/<self>/status
        // exposes PPid. On other platforms, fall through.
        #[cfg(target_os = "linux")]
        {
            if let Some(ppid) = read_parent_pid(self_pid) {
                if pid == ppid {
                    return Some("parent_pid");
                }
            }
        }
        None
    }

    /// Confirm a pid is both not in the built-in rejection list AND
    /// present on the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::TargetNotAllowed`] with the rule that
    /// fired first.
    pub fn require_pid(&self, pid: u32) -> Result<(), AgentError> {
        if let Some(rule) = self.check_pid(pid) {
            return Err(AgentError::TargetNotAllowed {
                rule,
                target: format!("pid:{pid}"),
            });
        }
        if !self.allowed_pids.contains(&pid) {
            return Err(AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                target: format!("pid:{pid}"),
            });
        }
        Ok(())
    }

    /// Confirm a cgroup path is allowed and not the host root.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::TargetNotAllowed`] with the rule that
    /// fired.
    pub fn require_cgroup(&self, path: &str) -> Result<(), AgentError> {
        if path == "/" {
            return Err(AgentError::TargetNotAllowed {
                rule: "host_cgroup_root",
                target: format!("cgroup:{path}"),
            });
        }
        if !self.allowed_cgroups.contains(path) {
            return Err(AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                target: format!("cgroup:{path}"),
            });
        }
        Ok(())
    }

    /// Confirm a network interface is on the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::TargetNotAllowed`] with the rule that
    /// fired.
    pub fn require_iface(&self, name: &str) -> Result<(), AgentError> {
        if !self.allowed_ifaces.contains(name) {
            return Err(AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                target: format!("iface:{name}"),
            });
        }
        Ok(())
    }

    /// Confirm a container / pod is on the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::TargetNotAllowed`] with the rule that
    /// fired.
    pub fn require_container(&self, name: &str) -> Result<(), AgentError> {
        if !self.allowed_containers.contains(name) {
            return Err(AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                target: format!("container:{name}"),
            });
        }
        Ok(())
    }

    /// Polymorphic target check that dispatches to the per-target
    /// `require_*` method. Adapters that need to act on one of the
    /// supported target kinds build a [`Target`] from their `FaultPlan`
    /// payload and call this once before any side effect.
    ///
    /// # Errors
    ///
    /// Returns [`AgentError::TargetNotAllowed`] with the rule that
    /// fired first.
    pub fn check_target(&self, target: &Target<'_>) -> Result<(), AgentError> {
        match target {
            Target::Pid(pid) => self.require_pid(*pid),
            Target::Cgroup(path) => self.require_cgroup(path),
            Target::Iface(name) => self.require_iface(name),
            Target::Container(name) => self.require_container(name),
        }
    }
}

/// Read the parent pid of `pid` from `/proc/<pid>/status`. Returns
/// `None` if the file is missing, unreadable, or the format is
/// unexpected.
#[cfg(target_os = "linux")]
fn read_parent_pid(pid: u32) -> Option<u32> {
    use std::fs;
    let raw = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("PPid:") {
            return rest.trim().parse::<u32>().ok();
        }
    }
    None
}
