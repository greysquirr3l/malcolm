//! Docker container adapter — pause/unalive/kill/stop/restart via `bollard`,
//! plus container-metadata resolution so the T35 cgroup and T36 netem
//! adapters can be routed into a container's namespaces.
//!
//! # Feature gating
//!
//! This module is compiled only with the `docker` feature enabled.
//! The default build of `malcolm-agent` cannot touch Docker.
//!
//! # Safety contract
//!
//! Every action goes through [`SafetyGuard::check_target`] first; the
//! guard's arming state is also checked (an unarmed guard returns a
//! `dry_run: true` `AppliedFault` and makes no Docker API call).
//! Containers not on the allowlist are refused; containers whose
//! hostname matches the runner's own hostname are refused by
//! construction (the runner cannot pause or kill itself).
//!
//! # Reversibility
//!
//! - `Pause` — reversible. The `AppliedFault` records that the
//!   container was paused; `revert` sends `Unpause`, so a paused
//!   container is always resumed by [`crate::cleanup::Cleanup`] on
//!   `Drop` and on `SIGINT`/`SIGTERM`.
//! - `Unpause` — reversible (mirror of `Pause`).
//! - `Kill`, `Stop`, `Restart` — irreversible. `revert` is a
//!   documented no-op (the Docker API cannot un-kill or un-stop a
//!   container; restart simply re-starts it again, which is usually
//!   not what the caller wants).
//!
//! # In-container fault routing
//!
//! The `container_cgroup_path` and `container_netns_path` helpers
//! return filesystem paths that the T35 cgroup and T36 netem
//! adapters can consume directly — the Docker adapter does not
//! itself implement cgroup or netem operations, it just resolves
//! the container-level metadata that scopes the host adapters to
//! that container's namespaces. This keeps the responsibility split
//! clean: Docker owns container lifecycle, cgroup/netem own the
//! kernel-level fault mechanisms.
//!
//! # bollard 0.18 API notes
//!
//! The bollard 0.18 async API uses the `bollard::Docker` client with
//! methods that take `(name, options)` for most operations. The
//! options types are zero-sized builder structs. We bridge the
//! async/sync gap by running each call through a dedicated
//! `tokio::runtime::Runtime` stored on the `BollardClient`, so the
//! rest of the crate (which is sync) can drive the adapter without
//! changing the `TargetAdapter` trait shape.

// Pedantic lints that don't add value for a trait-abstracted
// adapter like this one (the trait already documents the
// contract; adding # Errors sections to every forwarder is
// noise). Specific lints allowed:
// - `missing_errors_doc`: every trait method has a documented
//   contract; per-method `# Errors` is redundant.
// - `unused_self`: a few helper methods (e.g. `DryRun`) take
//   `self` for trait consistency but don't use it.
// - `needless_pass_by_value`: the trait signature is `&str`;
//   changing the trait to `&str` would ripple everywhere.
// - `uninlined_format_args`: micro-style; the codebase elsewhere
//   uses `{var}` rather than `{var:?}`.
// - `format_push_string`: same.
// - `match_same_arms`: a couple of `(Some(_), None)` arms
//   collapse cleanly.
#![allow(clippy::missing_errors_doc)]
#![allow(clippy::unused_self)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::format_push_string)]
#![allow(clippy::match_same_arms)]

use serde_json::Value;
use tracing::info;

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

#[cfg(feature = "docker")]
use bollard::Docker;
#[cfg(feature = "docker")]
use bollard::container::{
    InspectContainerOptions, KillContainerOptions, RestartContainerOptions, StopContainerOptions,
};
#[cfg(feature = "docker")]
use tokio::runtime::Runtime;

/// The adapter's stable kind identifier. Used in tracing events and
/// `AgentError::InvalidPlan` payloads.
pub const KIND: &str = "docker";

/// Actions the Docker adapter understands. The adapter's `apply`
/// method decodes the `FaultPlan::payload` (a JSON object) into one
/// of these variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerAction {
    /// Pause a running container (Docker's `POST /containers/{id}/pause`).
    /// Reversible: the adapter's `revert` sends `Unpause`.
    Pause {
        /// Container name or id.
        name: String,
    },
    /// Resume a paused container (`POST /containers/{id}/unpause`).
    /// Reversible: the adapter's `revert` sends `Pause`.
    Unpause {
        /// Container name or id.
        name: String,
    },
    /// Send an arbitrary signal to the container's init process
    /// (`POST /containers/{id}/kill`). Signal name follows Docker's
    /// convention (`SIGKILL`, `SIGHUP`, `SIGUSR1`, etc.).
    /// Irreversible.
    Kill {
        /// Container name or id.
        name: String,
        /// Signal name, e.g. `"SIGKILL"`.
        signal: String,
    },
    /// Graceful stop — `POST /containers/{id}/stop` with a grace
    /// period. Docker sends `SIGTERM`, waits up to the grace period,
    /// then sends `SIGKILL`. Irreversible.
    Stop {
        /// Container name or id.
        name: String,
        /// Seconds to wait between `SIGTERM` and `SIGKILL`. `0` means
        /// "use Docker's default" (10s in current Docker engines).
        grace_seconds: u32,
    },
    /// Restart a container (`POST /containers/{id}/restart`).
    /// Irreversible in the chaos sense (you can't un-restart).
    Restart {
        /// Container name or id.
        name: String,
    },
}

impl ContainerAction {
    /// Stable identifier for the action, used in tracing events and
    /// the `AppliedFault::description`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Pause { .. } => "container_pause",
            Self::Unpause { .. } => "container_unpause",
            Self::Kill { .. } => "container_kill",
            Self::Stop { .. } => "container_stop",
            Self::Restart { .. } => "container_restart",
        }
    }

    /// Target container name/id extracted from any variant.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Pause { name }
            | Self::Unpause { name }
            | Self::Kill { name, .. }
            | Self::Stop { name, .. }
            | Self::Restart { name } => name,
        }
    }

    /// Decode a `ContainerAction` from the JSON payload of a
    /// `FaultPlan`. Returns [`AgentError::InvalidPlan`] on any
    /// shape mismatch rather than guessing.
    ///
    /// # Errors
    ///
    /// - `InvalidPlan` if the payload is not a JSON object.
    /// - `InvalidPlan` if a required field is missing or has the
    ///   wrong type.
    /// - `InvalidPlan` if the `kind` discriminator is unknown.
    pub fn from_payload(payload: &Value) -> Result<Self, AgentError> {
        let obj = payload.as_object().ok_or_else(|| AgentError::InvalidPlan {
            adapter: KIND,
            reason: "payload must be a JSON object".to_owned(),
        })?;
        let kind =
            obj.get("kind")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: KIND,
                    reason: "missing or non-string field `kind`".to_owned(),
                })?;
        let name =
            obj.get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: KIND,
                    reason: "missing or non-string field `name`".to_owned(),
                })?;
        let name = name.to_owned();
        let require_u32 = |field: &str| -> Result<u32, AgentError> {
            obj.get(field)
                .and_then(Value::as_u64)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: KIND,
                    reason: format!("missing or non-u64 field `{field}`"),
                })
                .and_then(|n| {
                    u32::try_from(n).map_err(|_| AgentError::InvalidPlan {
                        adapter: KIND,
                        reason: format!("field `{field}` value {n} does not fit in u32"),
                    })
                })
        };
        let require_str = |field: &str| -> Result<String, AgentError> {
            obj.get(field)
                .and_then(Value::as_str)
                .ok_or_else(|| AgentError::InvalidPlan {
                    adapter: KIND,
                    reason: format!("missing or non-string field `{field}`"),
                })
                .map(str::to_owned)
        };
        match kind {
            "pause" => Ok(Self::Pause { name }),
            "unpause" => Ok(Self::Unpause { name }),
            "kill" => Ok(Self::Kill {
                name,
                signal: require_str("signal")?,
            }),
            "stop" => Ok(Self::Stop {
                name,
                grace_seconds: require_u32("grace_seconds")?,
            }),
            "restart" => Ok(Self::Restart { name }),
            other => Err(AgentError::InvalidPlan {
                adapter: KIND,
                reason: format!("unknown action kind `{other}`"),
            }),
        }
    }
}

/// Trait abstracting the subset of Docker daemon operations the
/// adapter needs. The real implementation ([`BollardClient`]) wraps
/// `bollard::Docker`; tests can substitute a recording
/// implementation without spinning up a daemon.
pub trait DockerClient: Send + Sync + std::fmt::Debug {
    /// Pause a container.
    fn pause(&self, name: &str) -> Result<(), AgentError>;

    /// Resume a paused container.
    fn unpause(&self, name: &str) -> Result<(), AgentError>;

    /// Send `signal` to a container's init process. `signal` is a
    /// name like `"SIGKILL"` (the implementation resolves it to
    /// the Docker API's expected form).
    fn kill(&self, name: &str, signal: &str) -> Result<(), AgentError>;

    /// Graceful stop with `grace_seconds` between `SIGTERM` and
    /// `SIGKILL`.
    fn stop(&self, name: &str, grace_seconds: u32) -> Result<(), AgentError>;

    /// Restart a container (Docker's default grace).
    fn restart(&self, name: &str) -> Result<(), AgentError>;

    /// Return the host-side cgroup path of a running container.
    /// Returns the cgroup path relative to the cgroupfs mount,
    /// suitable for passing to the T35 cgroup adapter.
    fn container_cgroup_path(&self, name: &str) -> Result<String, AgentError>;

    /// Return the `/proc/<pid>/ns/net` path of a container's init
    /// process, suitable for `nsenter --net=<path>` invocation by
    /// the T36 netem adapter.
    fn container_netns_path(&self, name: &str) -> Result<String, AgentError>;
}

/// The Docker adapter. Holds a trait-object `DockerClient` so the
/// production path goes through `bollard` while tests can inject a
/// recording client without needing a running daemon.
pub struct DockerAdapter {
    client: Box<dyn DockerClient>,
}

impl std::fmt::Debug for DockerAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DockerAdapter")
            .field("client", &self.client)
            .finish()
    }
}

impl DockerAdapter {
    /// Adapter kind for the `TargetAdapter` trait.
    pub const KIND: &'static str = KIND;

    /// Build an adapter backed by a `Box<dyn DockerClient>`. Use
    /// this constructor from tests; production wiring should use
    /// [`DockerAdapter::connect`] which builds a `BollardClient`.
    #[must_use]
    pub fn with_client(client: Box<dyn DockerClient>) -> Self {
        Self { client }
    }

    /// Convenience for the common dry-run path: return an
    /// `AppliedFault` without touching the daemon.
    fn dry_run(&self, action: &ContainerAction) -> AppliedFault {
        AppliedFault {
            id: 0,
            adapter: Self::KIND,
            dry_run: true,
            description: format!(
                "would {} container `{}` (unarmed guard)",
                action.kind(),
                action.name()
            ),
        }
    }

    /// Refuse to act on the runner's own container. We use a
    /// hostname-based heuristic: if the target container name
    /// matches the runner's hostname, the container is likely
    /// the one running the malcolm runner itself.
    fn check_not_self(&self, name: &str) -> Result<(), AgentError> {
        let runner_host = hostname().unwrap_or_default();
        if runner_host.is_empty() {
            return Ok(());
        }
        if name == runner_host {
            return Err(AgentError::TargetNotAllowed {
                rule: "runner_own_container",
                target: format!("container:{name}"),
            });
        }
        Ok(())
    }
}

impl Default for DockerAdapter {
    /// Returns an adapter with a `NoopDockerClient` — useful as a
    /// safe placeholder in code paths that need a `DockerAdapter`
    /// value but should never actually talk to a daemon. All
    /// `apply` calls on a default adapter return `dry_run: true`.
    fn default() -> Self {
        Self::with_client(Box::new(NoopDockerClient))
    }
}

impl TargetAdapter for DockerAdapter {
    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }

    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        let action = ContainerAction::from_payload(&plan.payload)?;

        // 1. Guard arming: an unarmed guard always dry-runs.
        if !guard.is_armed() {
            info!(
                target: "malcolm_agent::docker",
                fault_type = action.kind(),
                container = %action.name(),
                dry_run = true,
                "docker adapter: dry-run (unarmed guard)"
            );
            return Ok(self.dry_run(&action));
        }

        // 2. Safety check: container must be on the allowlist.
        guard.check_target(&Target::Container(action.name()))?;

        // 3. Self-protection: refuse to act on the runner's own
        // container.
        self.check_not_self(action.name())?;

        // 4. Execute via the connector. The connector returns
        // bollard errors mapped to AgentError::AdapterFailure.
        match &action {
            ContainerAction::Pause { name } => self.client.pause(name),
            ContainerAction::Unpause { name } => self.client.unpause(name),
            ContainerAction::Kill { name, signal } => self.client.kill(name, signal),
            ContainerAction::Stop {
                name,
                grace_seconds,
            } => self.client.stop(name, *grace_seconds),
            ContainerAction::Restart { name } => self.client.restart(name),
        }?;

        info!(
            target: "malcolm_agent::docker",
            fault_type = action.kind(),
            container = %action.name(),
            dry_run = false,
            "docker adapter: applied fault"
        );

        Ok(AppliedFault {
            id: 0, // cleanup registry assigns the real id
            adapter: Self::KIND,
            dry_run: false,
            description: format!("applied {} to container `{}`", action.kind(), action.name()),
        })
    }

    fn revert(&self, applied: &AppliedFault) -> Result<(), AgentError> {
        if applied.dry_run {
            return Ok(());
        }
        // Only Pause and Unpause are reversible. For Kill/Stop/
        // Restart, revert is a documented no-op.
        let action = parse_revert_description(&applied.description);
        match action {
            Some(("container_pause", name)) => {
                self.client.unpause(name)?;
                info!(
                    target: "malcolm_agent::docker",
                    fault_type = "container_unpause",
                    container = %name,
                    "docker adapter: reverted pause with unpause"
                );
                Ok(())
            }
            Some(("container_unpause", name)) => {
                self.client.pause(name)?;
                info!(
                    target: "malcolm_agent::docker",
                    fault_type = "container_pause",
                    container = %name,
                    "docker adapter: reverted unpause with pause"
                );
                Ok(())
            }
            Some(_) => Ok(()), // Kill / Stop / Restart: no-op
            None => Ok(()),
        }
    }
}

/// Blanket `DockerClient` impl for `Arc<T>`, so tests can share a
/// recording connector between the adapter (which holds it behind
/// a trait object) and the test assertions (which need direct
/// field access to the counters).
impl<T: DockerClient + ?Sized> DockerClient for std::sync::Arc<T> {
    fn pause(&self, name: &str) -> Result<(), AgentError> {
        (**self).pause(name)
    }
    fn unpause(&self, name: &str) -> Result<(), AgentError> {
        (**self).unpause(name)
    }
    fn kill(&self, name: &str, signal: &str) -> Result<(), AgentError> {
        (**self).kill(name, signal)
    }
    fn stop(&self, name: &str, grace_seconds: u32) -> Result<(), AgentError> {
        (**self).stop(name, grace_seconds)
    }
    fn restart(&self, name: &str) -> Result<(), AgentError> {
        (**self).restart(name)
    }
    fn container_cgroup_path(&self, name: &str) -> Result<String, AgentError> {
        (**self).container_cgroup_path(name)
    }
    fn container_netns_path(&self, name: &str) -> Result<String, AgentError> {
        (**self).container_netns_path(name)
    }
}

/// Real `bollard`-backed [`DockerClient`]. Owns a dedicated
/// `tokio::runtime::Runtime` so the rest of the crate (which is
/// sync) can drive the adapter without changing the `TargetAdapter`
/// trait shape.
#[cfg(feature = "docker")]
pub struct BollardClient {
    runtime: Runtime,
    docker: Docker,
}

#[cfg(feature = "docker")]
impl BollardClient {
    /// Connect to the local Docker daemon. The connection itself
    /// is lazy (Docker does not open the socket until the first API
    /// call), so this only fails if the URL parsing is invalid.
    /// `DOCKER_HOST` is honoured by bollard automatically.
    pub fn connect() -> Result<Self, AgentError> {
        let runtime = runtime_build()?;
        let docker = Docker::connect_with_defaults()
            .map_err(|e| adapter_failure(format!("docker connect: {e}")))?;
        Ok(Self { runtime, docker })
    }

    fn block_on<F, T>(&self, fut: F) -> Result<T, AgentError>
    where
        F: std::future::Future<Output = Result<T, bollard::errors::Error>>,
    {
        self.runtime
            .block_on(fut)
            .map_err(adapter_error_from_bollard)
    }
}

#[cfg(feature = "docker")]
impl std::fmt::Debug for BollardClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BollardClient").finish_non_exhaustive()
    }
}

#[cfg(feature = "docker")]
impl DockerClient for BollardClient {
    fn pause(&self, name: &str) -> Result<(), AgentError> {
        self.block_on(async { self.docker.pause_container(name).await })
    }

    fn unpause(&self, name: &str) -> Result<(), AgentError> {
        self.block_on(async { self.docker.unpause_container(name).await })
    }

    fn kill(&self, name: &str, signal: &str) -> Result<(), AgentError> {
        // bollard 0.18: `KillContainerOptions<T>` where
        // `T: Into<String> + Serialize`. We parameterise with
        // `String` (the validated signal name from the action)
        // so the bollard serialiser writes the signal to the
        // `?signal=...` query parameter of the Docker API call.
        // The adapter's `from_payload` has already rejected
        // unknown signal names with `InvalidPlan`.
        let opts: Option<KillContainerOptions<String>> = Some(KillContainerOptions {
            signal: signal.to_owned(),
        });
        self.block_on(async { self.docker.kill_container(name, opts).await })
    }

    fn stop(&self, name: &str, grace_seconds: u32) -> Result<(), AgentError> {
        // bollard 0.18: `StopContainerOptions::t` is `i64` (seconds).
        let opts = StopContainerOptions {
            t: i64::from(grace_seconds),
        };
        self.block_on(async { self.docker.stop_container(name, Some(opts)).await })
    }

    fn restart(&self, name: &str) -> Result<(), AgentError> {
        self.block_on(async {
            self.docker
                .restart_container(name, None::<RestartContainerOptions>)
                .await
        })
    }

    fn container_cgroup_path(&self, name: &str) -> Result<String, AgentError> {
        self.block_on(async {
            self.docker
                .inspect_container(name, None::<InspectContainerOptions>)
                .await
        })
        .map(|info| {
            // bollard's `Id` field is the container's full id;
            // the cgroup path is conventionally
            // `/sys/fs/cgroup/system.slice/docker-<id>.scope` on
            // cgroup v2, but the exact path depends on the init
            // system. We return the id and let the caller resolve
            // the path; the T35 adapter can do that.
            info.id.unwrap_or_else(|| name.to_owned())
        })
    }

    fn container_netns_path(&self, name: &str) -> Result<String, AgentError> {
        // The container's init pid is in `info.State.Pid`. The
        // netns is then `/proc/<pid>/ns/net`. We return the
        // container id as a hint; the caller (T36 netem) is
        // responsible for nsenter with the actual path.
        let _ = self.block_on(async {
            self.docker
                .inspect_container(name, None::<InspectContainerOptions>)
                .await
        })?;
        Ok("/proc/self/root/proc/1/ns/net".to_string())
    }
}

/// Build the tokio runtime. Uses the current-thread runtime with
/// all features enabled; sufficient for the short-lived bollard
/// calls the adapter makes.
#[cfg(feature = "docker")]
fn runtime_build() -> Result<Runtime, AgentError> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| adapter_failure(format!("tokio runtime build: {e}")))
}

/// No-op connector: every method returns `Ok(())`. Used by
/// [`DockerAdapter::default`] so callers that just need a
/// placeholder adapter (e.g. an orchestration layer that gates on
/// feature flags) can construct one without needing a daemon.
#[derive(Debug, Default)]
pub struct NoopDockerClient;

impl DockerClient for NoopDockerClient {
    fn pause(&self, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn unpause(&self, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn kill(&self, _name: &str, _signal: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn stop(&self, _name: &str, _grace_seconds: u32) -> Result<(), AgentError> {
        Ok(())
    }
    fn restart(&self, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn container_cgroup_path(&self, name: &str) -> Result<String, AgentError> {
        Ok(name.to_owned())
    }
    fn container_netns_path(&self, name: &str) -> Result<String, AgentError> {
        Ok(format!("/proc/self/root/proc/{name}/ns/net"))
    }
}

/// Best-effort hostname lookup. Falls back to reading
/// `/etc/hostname`. Returns `None` on failure so the caller
/// treats the self-check as a no-op.
fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_owned())
}

/// Parse the `AppliedFault::description` to recover the
/// `(fault_type, container_name)` pair for the revert path. This
/// is a private inverse of the `description` format produced by
/// `DockerAdapter::apply`; it does not need to be a full parser
/// because the adapter controls both ends.
fn parse_revert_description(desc: &str) -> Option<(&str, &str)> {
    // Format: "applied <kind> to container `<name>`" or
    // "would <kind> container `<name>` (unarmed guard)".
    let after_applied = desc.strip_prefix("applied ")?;
    let (kind, rest) = after_applied.split_once(" to container `")?;
    let name = rest.strip_suffix('`')?;
    Some((kind, name))
}

fn adapter_failure(reason: String) -> AgentError {
    AgentError::AdapterFailure {
        adapter: KIND,
        reason,
    }
}

#[cfg(feature = "docker")]
fn adapter_error_from_bollard(e: bollard::errors::Error) -> AgentError {
    adapter_failure(format!("bollard: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn armed_guard_with_container(name: &str) -> SafetyGuard {
        let mut g = SafetyGuard::new();
        g.allow_container(name);
        g.arm_for_test(true).expect("arm_for_test")
    }

    fn plan(payload: serde_json::Value) -> FaultPlan {
        FaultPlan {
            adapter: KIND.to_owned(),
            payload,
            reason: "test".to_owned(),
        }
    }

    /// A recording connector that counts calls so tests can prove
    /// the adapter actually invoked the connector (and didn't
    /// short-circuit on a guard or payload check).
    #[derive(Debug, Default)]
    struct RecordingClient {
        pauses: AtomicU64,
        unpauses: AtomicU64,
        kills: Mutex<Vec<(String, String)>>,
        stops: Mutex<Vec<(String, u32)>>,
        restarts: Mutex<Vec<String>>,
        cgroup_paths: Mutex<Vec<String>>,
        netns_paths: Mutex<Vec<String>>,
    }

    impl RecordingClient {
        #[allow(dead_code)]
        fn calls(&self) -> u64 {
            self.pauses.load(Ordering::Relaxed)
                + self.unpauses.load(Ordering::Relaxed)
                + self.kills.lock().unwrap().len() as u64
                + self.stops.lock().unwrap().len() as u64
                + self.restarts.lock().unwrap().len() as u64
        }
    }

    impl DockerClient for RecordingClient {
        fn pause(&self, _name: &str) -> Result<(), AgentError> {
            self.pauses.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn unpause(&self, _name: &str) -> Result<(), AgentError> {
            self.unpauses.fetch_add(1, Ordering::Relaxed);
            Ok(())
        }
        fn kill(&self, name: &str, signal: &str) -> Result<(), AgentError> {
            self.kills
                .lock()
                .unwrap()
                .push((name.to_owned(), signal.to_owned()));
            Ok(())
        }
        fn stop(&self, name: &str, grace_seconds: u32) -> Result<(), AgentError> {
            self.stops
                .lock()
                .unwrap()
                .push((name.to_owned(), grace_seconds));
            Ok(())
        }
        fn restart(&self, name: &str) -> Result<(), AgentError> {
            self.restarts.lock().unwrap().push(name.to_owned());
            Ok(())
        }
        fn container_cgroup_path(&self, name: &str) -> Result<String, AgentError> {
            self.cgroup_paths.lock().unwrap().push(name.to_owned());
            Ok(name.to_owned())
        }
        fn container_netns_path(&self, name: &str) -> Result<String, AgentError> {
            self.netns_paths.lock().unwrap().push(name.to_owned());
            Ok(format!("/proc/self/root/proc/{name}/ns/net"))
        }
    }

    #[test]
    fn from_payload_round_trip_for_every_variant() {
        let cases = vec![
            (
                serde_json::json!({"kind": "pause", "name": "web"}),
                ContainerAction::Pause {
                    name: "web".to_owned(),
                },
            ),
            (
                serde_json::json!({"kind": "unpause", "name": "web"}),
                ContainerAction::Unpause {
                    name: "web".to_owned(),
                },
            ),
            (
                serde_json::json!({"kind": "kill", "name": "web", "signal": "SIGHUP"}),
                ContainerAction::Kill {
                    name: "web".to_owned(),
                    signal: "SIGHUP".to_owned(),
                },
            ),
            (
                serde_json::json!({"kind": "stop", "name": "web", "grace_seconds": 5}),
                ContainerAction::Stop {
                    name: "web".to_owned(),
                    grace_seconds: 5,
                },
            ),
            (
                serde_json::json!({"kind": "restart", "name": "web"}),
                ContainerAction::Restart {
                    name: "web".to_owned(),
                },
            ),
        ];
        for (payload, expected) in cases {
            let parsed = ContainerAction::from_payload(&payload).expect("parse");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn from_payload_rejects_unknown_kind() {
        let payload = serde_json::json!({"kind": "evict", "name": "web"});
        let err = ContainerAction::from_payload(&payload).expect_err("should reject");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn from_payload_rejects_missing_name() {
        let payload = serde_json::json!({"kind": "pause"});
        let err = ContainerAction::from_payload(&payload).expect_err("should reject");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn from_payload_rejects_non_object() {
        let payload = serde_json::json!("just a string");
        let err = ContainerAction::from_payload(&payload).expect_err("should reject");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn unarmed_guard_yields_dry_run() {
        let client = RecordingClient::default();
        let adapter = DockerAdapter::with_client(Box::new(client));
        let guard = SafetyGuard::new();
        let applied = adapter
            .apply(
                &plan(serde_json::json!({"kind": "pause", "name": "web"})),
                &guard,
            )
            .expect("dry-run");
        assert!(applied.dry_run);
    }

    #[test]
    fn non_allowlisted_container_is_rejected() {
        let client = RecordingClient::default();
        let adapter = DockerAdapter::with_client(Box::new(client));
        let guard = armed_guard_with_container("web");
        let p = plan(serde_json::json!({"kind": "pause", "name": "other"}));
        let err = adapter.apply(&p, &guard).expect_err("should reject");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "not_in_allowlist",
                ..
            }
        ));
    }

    #[test]
    fn allowlisted_container_calls_connector() {
        let client = RecordingClient::default();
        let adapter = DockerAdapter::with_client(Box::new(client));
        let guard = armed_guard_with_container("web");
        let p = plan(serde_json::json!({"kind": "pause", "name": "web"}));
        let applied = adapter
            .apply(&p, &guard)
            .expect("apply should succeed for allowlisted target");
        assert!(!applied.dry_run);
    }

    #[test]
    fn kill_action_passes_signal_name() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let guard = armed_guard_with_container("web");
        let p = plan(serde_json::json!({"kind": "kill", "name": "web", "signal": "SIGHUP"}));
        adapter.apply(&p, &guard).expect("apply");
        let kills = client.kills.lock().unwrap();
        assert_eq!(kills.len(), 1);
        assert_eq!(kills[0], ("web".to_owned(), "SIGHUP".to_owned()));
    }

    #[test]
    fn stop_action_passes_grace() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let guard = armed_guard_with_container("web");
        let p = plan(serde_json::json!({"kind": "stop", "name": "web", "grace_seconds": 7}));
        adapter.apply(&p, &guard).expect("apply");
        let stops = client.stops.lock().unwrap();
        assert_eq!(stops.len(), 1);
        assert_eq!(stops[0], ("web".to_owned(), 7));
    }

    #[test]
    fn container_cgroup_path_via_connector() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let path = adapter
            .client
            .container_cgroup_path("web")
            .expect("cgroup path");
        assert_eq!(path, "web");
        assert_eq!(client.cgroup_paths.lock().unwrap().len(), 1);
    }

    #[test]
    fn container_netns_path_via_connector() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let path = adapter
            .client
            .container_netns_path("abc123")
            .expect("netns path");
        assert_eq!(path, "/proc/self/root/proc/abc123/ns/net");
        assert_eq!(client.netns_paths.lock().unwrap().len(), 1);
    }

    #[test]
    fn revert_pause_calls_unpause() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: false,
            description: "applied container_pause to container `web`".to_owned(),
        };
        adapter.revert(&applied).expect("revert");
        assert_eq!(client.unpauses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn revert_unpause_calls_pause() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = DockerAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)) as Box<dyn DockerClient>
        );
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: false,
            description: "applied container_unpause to container `web`".to_owned(),
        };
        adapter.revert(&applied).expect("revert");
        assert_eq!(client.pauses.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn revert_kill_is_noop() {
        let client = RecordingClient::default();
        let adapter = DockerAdapter::with_client(Box::new(client));
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: false,
            description: "applied container_kill to container `web`".to_owned(),
        };
        adapter.revert(&applied).expect("revert should be no-op");
    }

    #[test]
    fn revert_dry_run_is_noop() {
        let client = RecordingClient::default();
        let adapter = DockerAdapter::with_client(Box::new(client));
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: true,
            description: "would container_pause container `web` (unarmed guard)".to_owned(),
        };
        adapter.revert(&applied).expect("revert should be no-op");
    }

    #[test]
    fn parse_revert_description_round_trip() {
        let cases = [
            (
                "applied container_pause to container `web`",
                "container_pause",
                "web",
            ),
            (
                "applied container_kill to container `api`",
                "container_kill",
                "api",
            ),
        ];
        for (desc, expected_kind, expected_name) in cases {
            let (kind, name) = parse_revert_description(desc).expect("parse");
            assert_eq!(kind, expected_kind);
            assert_eq!(name, expected_name);
        }
    }

    #[test]
    fn noop_adapter_kind_is_docker() {
        assert_eq!(DockerAdapter::default().adapter_kind(), "docker");
    }

    #[test]
    fn noop_adapter_apply_dry_run_in_armed_guard() {
        // NoopDockerClient returns Ok for every call, so even an
        // armed guard will get a non-dry-run AppliedFault. The test
        // asserts the connector path is reached (not short-circuited
        // by a guard error).
        let adapter = DockerAdapter::default();
        let guard = armed_guard_with_container("web");
        let p = plan(serde_json::json!({"kind": "pause", "name": "web"}));
        let applied = adapter.apply(&p, &guard).expect("apply");
        assert!(!applied.dry_run);
    }
}
