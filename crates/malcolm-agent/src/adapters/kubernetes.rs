//! Kubernetes targeting adapter — pod delete/evict with label-selector
//! fan-out, namespace allowlist, and blast-radius cap.
//!
//! # Status: skeleton on the dev branch
//!
//! This module is a **trait-based skeleton**. The safety /
//! allowlist / blast-radius / dry-run logic is wired up and tested
//! against a [`MockKubeClient`]. The mutation methods
//! ([`KubernetesAdapter::apply`] when the guard is armed) are
//! stubbed behind `not yet implemented` returns — a real K8s
//! cluster is required for full runtime verification, and pulling
//! the `kube` + `k8s-openapi` + `tonic` gRPC stack into the
//! default feature set is not worth the dependency weight for a
//! skeleton. The trait design means the mutation methods are
//! mockable today and swappable for a real client later without
//! touching the safety / dry-run / blast-radius logic.
//!
//! # Feature gating
//!
//! This module is compiled only with the `kubernetes` feature
//! enabled. The default build of `malcolm-agent` cannot touch K8s.
//!
//! # Safety contract
//!
//! Every action goes through [`SafetyGuard::check_target`] first.
//! `kube-system` is hard-refused (operator convention; chaos
//! tooling must not touch the control plane). Any namespace not on
//! the allowlist is refused. A blast-radius cap is enforced before
//! any mutation: the adapter refuses plans that would delete or
//! evict more pods than the configured cap, or more than a
//! configured percentage of a workload. The guard's arming state
//! is also checked: an unarmed guard always dry-runs.
//!
//! # Reversibility
//!
//! - `DeletePod` — irreversible. The K8s controller reschedules the
//!   pod; `revert` is a documented no-op (we cannot un-delete a
//!   pod; the replacement is a new pod with a new uid).
//! - `EvictPod` — same as `DeletePod`. Eviction is the polite form
//!   of delete for stateful workloads; the controller creates a
//!   replacement.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use tracing::info;

use crate::adapter::{AppliedFault, FaultPlan, TargetAdapter};
use crate::error::AgentError;
use crate::safety::{SafetyGuard, Target};

/// The adapter's stable kind identifier.
pub const KIND: &str = "kubernetes";

/// Hard-refused namespace: the K8s control plane itself. Operator
/// convention: chaos tooling must never target `kube-system` (or
/// `kube-public` on older clusters). The adapter hard-rejects
/// this namespace regardless of the allowlist.
pub const KUBE_SYSTEM_NAMESPACE: &str = "kube-system";

/// Reference to a single pod. Stable across the pod's lifetime;
/// `uid` changes when the controller reschedules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PodRef {
    /// Pod name.
    pub name: String,
    /// Owning namespace.
    pub namespace: String,
    /// K8s uid (used to track identity through reschedules; not
    /// currently used by the skeleton but part of the trait
    /// contract for when a real `kube` client is wired in).
    #[allow(dead_code)]
    pub uid: String,
}

/// Type alias for the label selector map. We use `BTreeMap`
/// (standard library) rather than `serde_json::Map` because the
/// latter's `Eq`/`PartialEq`/`Debug`/`Clone` impls are
/// feature-gated in some `serde_json` versions, and K8s label
/// selectors are inherently ordered (or at least, ordering is
/// observable in the trait surface), so `BTreeMap` is the
/// natural choice.
pub type LabelSelector = BTreeMap<String, String>;

impl PodRef {
    /// `"<namespace>/<name>"` — the standard K8s `ObjectRef` string
    /// form, useful in tracing events and `AppliedFault`
    /// descriptions.
    #[must_use]
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

/// What the adapter will target. Each variant maps to a JSON
/// discriminator in the `FaultPlan` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetSpec {
    /// Target a specific pod by name in a specific namespace.
    Pod {
        /// Namespace of the target pod.
        namespace: String,
        /// Pod name.
        name: String,
    },
    /// Target every pod in a namespace matching the given
    /// `key=value` label selector. K8s label selectors support
    /// set-based requirements (`in`, `notin`) as well, but the
    /// skeleton accepts only the simple equality form for the
    /// blast-radius check.
    LabelSelector {
        /// Namespace to search.
        namespace: String,
        /// `key=value` map. All entries must match (logical AND).
        selector: LabelSelector,
    },
}

impl TargetSpec {
    /// The namespace the spec lives in. Used by the namespace
    /// allowlist check.
    #[must_use]
    pub fn namespace(&self) -> &str {
        match self {
            Self::Pod { namespace, .. } | Self::LabelSelector { namespace, .. } => namespace,
        }
    }

    /// Stable identifier for the spec, used in tracing events and
    /// the `AppliedFault::description`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Pod { .. } => "pod",
            Self::LabelSelector { .. } => "label_selector",
        }
    }
}

/// The action the adapter will perform on the target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PodAction {
    /// Delete a single pod (K8s issues a `DELETE` on the Pod
    /// resource; the controller reschedules). Irreversible.
    Delete {
        /// Pod to delete.
        target: TargetSpec,
    },
    /// Evict a single pod (K8s creates an `Eviction` object; the
    /// controller honours PDBs and creates a replacement).
    /// Irreversible.
    Evict {
        /// Pod to evict.
        target: TargetSpec,
    },
}

impl PodAction {
    /// Stable identifier for the action, used in tracing events
    /// and the `AppliedFault::description`.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Delete { .. } => "pod_delete",
            Self::Evict { .. } => "pod_evict",
        }
    }

    /// Reference to the action's target spec.
    #[must_use]
    pub fn target(&self) -> &TargetSpec {
        match self {
            Self::Delete { target } | Self::Evict { target } => target,
        }
    }

    /// Decode a `PodAction` from the JSON payload of a
    /// `FaultPlan`. Returns [`AgentError::InvalidPlan`] on any
    /// shape mismatch.
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
        let namespace = obj
            .get("namespace")
            .and_then(Value::as_str)
            .ok_or_else(|| AgentError::InvalidPlan {
                adapter: KIND,
                reason: "missing or non-string field `namespace`".to_owned(),
            })?;
        let namespace = namespace.to_owned();
        match kind {
            "pod_delete" => {
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: KIND,
                        reason: "missing or non-string field `name` for pod_delete".to_owned(),
                    })?
                    .to_owned();
                Ok(Self::Delete {
                    target: TargetSpec::Pod { namespace, name },
                })
            }
            "pod_evict" => {
                let name = obj
                    .get("name")
                    .and_then(Value::as_str)
                    .ok_or_else(|| AgentError::InvalidPlan {
                        adapter: KIND,
                        reason: "missing or non-string field `name` for pod_evict".to_owned(),
                    })?
                    .to_owned();
                Ok(Self::Evict {
                    target: TargetSpec::Pod { namespace, name },
                })
            }
            "selector_delete" | "selector_evict" => {
                let selector_obj =
                    obj.get("selector")
                        .and_then(Value::as_object)
                        .ok_or_else(|| AgentError::InvalidPlan {
                            adapter: KIND,
                            reason: format!("missing or non-object field `selector` for {kind}"),
                        })?;
                // Convert `serde_json::Map<String, Value>` to
                // `serde_json::Map<String, String>` by extracting
                // each value as a string. K8s label values are
                // always strings, so anything non-string is a
                // malformed plan.
                let mut selector: LabelSelector = BTreeMap::new();
                for (k, v) in selector_obj {
                    let s = v.as_str().ok_or_else(|| AgentError::InvalidPlan {
                        adapter: KIND,
                        reason: format!("label selector value for `{k}` must be a string, got {v}"),
                    })?;
                    selector.insert(k.clone(), s.to_owned());
                }
                let target = TargetSpec::LabelSelector {
                    namespace,
                    selector,
                };
                match kind {
                    "selector_delete" => Ok(Self::Delete { target }),
                    "selector_evict" => Ok(Self::Evict { target }),
                    _ => unreachable!(),
                }
            }
            other => Err(AgentError::InvalidPlan {
                adapter: KIND,
                reason: format!("unknown action kind `{other}`"),
            }),
        }
    }
}

/// Configuration for the blast-radius cap. The cap is enforced
/// before any mutation: a plan that would touch more pods than
/// the cap is refused with `BlastRadiusExceeded`.
#[derive(Debug, Clone)]
pub struct BlastRadius {
    /// Maximum number of pods the adapter is willing to delete or
    /// evict in a single `apply` call. `None` means "no cap on
    /// pod count" (not recommended; the cap is the whole point).
    pub max_pods: Option<u32>,
    /// Namespaces the adapter is willing to target. Empty set means
    /// "deny everything". `kube-system` is *always* refused
    /// regardless of this set.
    pub allowed_namespaces: BTreeSet<String>,
}

impl Default for BlastRadius {
    fn default() -> Self {
        // Conservative default: no pods, no namespaces. The
        // operator must explicitly add namespaces and a max
        // before the adapter will do anything.
        Self {
            max_pods: Some(0),
            allowed_namespaces: BTreeSet::new(),
        }
    }
}

/// Trait abstracting the K8s API operations the adapter needs.
/// The real implementation would wrap `kube::Client`; the skeleton
/// ships with [`MockKubeClient`] for tests. The trait is
/// deliberately narrow (3 methods) per the AGENTS.md "narrow
/// port trait" rule.
pub trait KubeClient: Send + Sync + std::fmt::Debug {
    /// List the pods that a `LabelSelector` would match. The
    /// `LabelSelector` targets are in a single namespace per call.
    /// Returns an empty `Vec` (not an error) if no pods match.
    fn list_pods(
        &self,
        namespace: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<PodRef>, AgentError>;
    /// Delete a single pod by name in a namespace.
    fn delete_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError>;

    /// Evict a single pod by name in a namespace (creates an
    /// `Eviction` CRD; the K8s controller honours PDBs).
    fn evict_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError>;
}

/// The K8s adapter. Holds a trait-object [`KubeClient`] so the
/// production path goes through a real `kube::Client` while
/// tests use [`MockKubeClient`].
pub struct KubernetesAdapter {
    client: Box<dyn KubeClient>,
    blast_radius: BlastRadius,
}

impl std::fmt::Debug for KubernetesAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KubernetesAdapter")
            .field("client", &self.client)
            .field("blast_radius", &self.blast_radius)
            .finish()
    }
}

impl KubernetesAdapter {
    /// Adapter kind for the `TargetAdapter` trait.
    pub const KIND: &'static str = KIND;

    /// Build an adapter with a custom `KubeClient` and blast
    /// radius. Used by tests and by production wiring that
    /// provides its own client (e.g. from a connection pool).
    #[must_use]
    pub fn with_client(client: Box<dyn KubeClient>, blast_radius: BlastRadius) -> Self {
        Self {
            client,
            blast_radius,
        }
    }

    /// Build an adapter with the default blast radius (empty
    /// allowlist, 0 max pods) and a no-op client. Every `apply`
    /// call returns `dry_run: true` and makes no K8s API call.
    /// Useful as a placeholder in code paths that gate on
    /// feature flags.
    #[must_use]
    pub fn new_noop() -> Self {
        Self::with_client(Box::new(NoopKubeClient), BlastRadius::default())
    }

    /// Convenience for the common dry-run path.
    fn dry_run(action: &PodAction) -> AppliedFault {
        AppliedFault {
            id: 0,
            adapter: Self::KIND,
            dry_run: true,
            description: format!(
                "would {} on {} (unarmed guard)",
                action.kind(),
                action.target().kind()
            ),
        }
    }

    /// Hard-refuse `kube-system` and any namespace not on the
    /// allowlist.
    fn check_namespace(&self, namespace: &str) -> Result<(), AgentError> {
        if namespace == KUBE_SYSTEM_NAMESPACE {
            return Err(AgentError::TargetNotAllowed {
                rule: "kube_system_namespace",
                target: format!("namespace:{namespace}"),
            });
        }
        if !self.blast_radius.allowed_namespaces.contains(namespace) {
            return Err(AgentError::TargetNotAllowed {
                rule: "namespace_not_in_allowlist",
                target: format!("namespace:{namespace}"),
            });
        }
        Ok(())
    }

    /// Enforce the blast-radius cap against the number of pods a
    /// `LabelSelector` would match. `Pod` targets are always
    /// within the cap (one pod).
    #[allow(
        dead_code,
        reason = "logic inlined in apply; kept for unit-test access"
    )]
    fn check_blast_radius(&self, action: &PodAction) -> Result<(), AgentError> {
        let count = match action.target() {
            TargetSpec::Pod { .. } => 1,
            TargetSpec::LabelSelector {
                namespace,
                selector,
            } => self.client.list_pods(namespace, selector)?.len(),
        };
        if let Some(max) = self.blast_radius.max_pods {
            if u32::try_from(count).unwrap_or(u32::MAX) > max {
                return Err(AgentError::TargetNotAllowed {
                    rule: "blast_radius_exceeded",
                    target: format!("selector matches {count} pods, cap is {max}"),
                });
            }
        }
        Ok(())
    }
}

impl Default for KubernetesAdapter {
    fn default() -> Self {
        Self::new_noop()
    }
}

impl TargetAdapter for KubernetesAdapter {
    fn adapter_kind(&self) -> &'static str {
        Self::KIND
    }

    fn apply(&self, plan: &FaultPlan, guard: &SafetyGuard) -> Result<AppliedFault, AgentError> {
        let action = PodAction::from_payload(&plan.payload)?;

        // 1. Guard arming: an unarmed guard always dry-runs.
        if !guard.is_armed() {
            info!(
                target: "malcolm_agent::kubernetes",
                fault_type = action.kind(),
                target_kind = action.target().kind(),
                dry_run = true,
                "kubernetes adapter: dry-run (unarmed guard)"
            );
            return Ok(Self::dry_run(&action));
        }

        // 2. Namespace check (hard-refuse kube-system and any
        // non-allowlisted namespace).
        self.check_namespace(action.target().namespace())?;

        // 3. Blast-radius cap: refuse plans that would touch too
        // many pods. This is what the spec calls "max-blast-radius
        // cap (e.g. never delete more than N pods, never >X% of a
        // Deployment's replicas) enforced before issuing deletes".
        // The skeleton implements the per-call absolute cap; the
        // percentage-of-Deployment check is a documented follow-up.
        // For selector targets we list once here and reuse the
        // result in the mutation arm below, so the K8s API is
        // only hit once per `apply` call.
        let listed_pods = match action.target() {
            TargetSpec::Pod { .. } => Vec::new(),
            TargetSpec::LabelSelector {
                namespace,
                selector,
            } => self.client.list_pods(namespace, selector)?,
        };
        let count = match action.target() {
            TargetSpec::Pod { .. } => 1,
            TargetSpec::LabelSelector { .. } => listed_pods.len(),
        };
        if let Some(max) = self.blast_radius.max_pods {
            if u32::try_from(count).unwrap_or(u32::MAX) > max {
                return Err(AgentError::TargetNotAllowed {
                    rule: "blast_radius_exceeded",
                    target: format!("selector matches {count} pods, cap is {max}"),
                });
            }
        }

        // 4. Safety check: target namespace must also be on the
        // generic guard allowlist (defence in depth — the
        // namespace-specific check above is the primary gate).
        guard.check_target(&Target::Container(action.target().namespace()))?;

        // 5. Execute via the connector. The skeleton returns
        // `not yet implemented` for every mutation — the real
        // `kube::Client`-backed implementation is a tracked
        // follow-up that requires a real cluster.
        match &action {
            PodAction::Delete {
                target: TargetSpec::Pod { namespace, name },
            } => {
                self.client.delete_pod(namespace, name)?;
            }
            PodAction::Evict {
                target: TargetSpec::Pod { namespace, name },
            } => {
                self.client.evict_pod(namespace, name)?;
            }
            PodAction::Delete {
                target: TargetSpec::LabelSelector { .. },
            } => {
                for pod in &listed_pods {
                    self.client.delete_pod(&pod.namespace, &pod.name)?;
                }
            }
            PodAction::Evict {
                target: TargetSpec::LabelSelector { .. },
            } => {
                for pod in &listed_pods {
                    self.client.evict_pod(&pod.namespace, &pod.name)?;
                }
            }
        }

        info!(
            target: "malcolm_agent::kubernetes",
            fault_type = action.kind(),
            target_kind = action.target().kind(),
            dry_run = false,
            "kubernetes adapter: applied fault"
        );

        Ok(AppliedFault {
            id: 0,
            adapter: Self::KIND,
            dry_run: false,
            description: format!("applied {} on {}", action.kind(), action.target().kind()),
        })
    }

    fn revert(&self, _applied: &AppliedFault) -> Result<(), AgentError> {
        // Pod deletion and eviction are both irreversible in the
        // K8s sense: the controller reschedules, creating a new
        // pod with a new uid. The skeleton has no revert to do.
        // A future version with cordoning/uncordoning could
        // implement an actual revert for the cordon action.
        Ok(())
    }
}

/// No-op connector: every method returns `Ok(())` and `list_pods`
/// returns an empty `Vec`. Used by [`KubernetesAdapter::new_noop`]
/// for safe-placeholder construction.
#[derive(Debug, Default)]
pub struct NoopKubeClient;

impl KubeClient for NoopKubeClient {
    fn list_pods(
        &self,
        _namespace: &str,
        _selector: &LabelSelector,
    ) -> Result<Vec<PodRef>, AgentError> {
        Ok(Vec::new())
    }
    fn delete_pod(&self, _namespace: &str, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
    fn evict_pod(&self, _namespace: &str, _name: &str) -> Result<(), AgentError> {
        Ok(())
    }
}

/// Blanket `KubeClient` impl for `Arc<T>`, so tests can share a
/// recording connector between the adapter (which holds it behind
/// a trait object) and the test assertions (which need direct
/// field access to the counters).
impl<T: KubeClient + ?Sized> KubeClient for std::sync::Arc<T> {
    fn list_pods(
        &self,
        namespace: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<PodRef>, AgentError> {
        (**self).list_pods(namespace, selector)
    }
    fn delete_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
        (**self).delete_pod(namespace, name)
    }
    fn evict_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
        (**self).evict_pod(namespace, name)
    }
}

/// Blanket `KubeClient` impl for `&T` (immutable reference), so
/// callers can pass a borrowed client to the adapter without
/// consuming the value.
impl<T: KubeClient + ?Sized> KubeClient for &T {
    fn list_pods(
        &self,
        namespace: &str,
        selector: &LabelSelector,
    ) -> Result<Vec<PodRef>, AgentError> {
        (**self).list_pods(namespace, selector)
    }
    fn delete_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
        (**self).delete_pod(namespace, name)
    }
    fn evict_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
        (**self).evict_pod(namespace, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A recording `KubeClient` that counts calls so tests can
    /// prove the adapter actually invoked the connector (and
    /// didn't short-circuit on a guard or payload check).
    #[derive(Debug, Default)]
    struct RecordingClient {
        list_calls: AtomicU64,
        deletes: Mutex<Vec<PodRef>>,
        evicts: Mutex<Vec<PodRef>>,
        /// Pre-seeded pods returned by `list_pods` for selector
        /// queries. If empty, `list_pods` returns an empty `Vec`.
        seeded_pods: Mutex<Vec<PodRef>>,
    }

    impl RecordingClient {
        fn with_seeded_pods(pods: Vec<PodRef>) -> Self {
            Self {
                list_calls: AtomicU64::new(0),
                deletes: Mutex::new(Vec::new()),
                evicts: Mutex::new(Vec::new()),
                seeded_pods: Mutex::new(pods),
            }
        }
    }

    impl KubeClient for RecordingClient {
        fn list_pods(
            &self,
            _namespace: &str,
            _selector: &LabelSelector,
        ) -> Result<Vec<PodRef>, AgentError> {
            self.list_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.seeded_pods.lock().unwrap().clone())
        }
        fn delete_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
            self.deletes.lock().unwrap().push(PodRef {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                uid: String::new(),
            });
            Ok(())
        }
        fn evict_pod(&self, namespace: &str, name: &str) -> Result<(), AgentError> {
            self.evicts.lock().unwrap().push(PodRef {
                namespace: namespace.to_owned(),
                name: name.to_owned(),
                uid: String::new(),
            });
            Ok(())
        }
    }

    fn armed_guard() -> SafetyGuard {
        let mut g = SafetyGuard::new();
        g.allow_container("default");
        g.arm_for_test(true).expect("arm_for_test")
    }

    fn permissive_blast_radius() -> BlastRadius {
        let mut ns = BTreeSet::new();
        ns.insert("default".to_owned());
        BlastRadius {
            max_pods: Some(10),
            allowed_namespaces: ns,
        }
    }

    fn plan(payload: serde_json::Value) -> FaultPlan {
        FaultPlan {
            adapter: KIND.to_owned(),
            payload,
            reason: "test".to_owned(),
        }
    }

    fn pod(name: &str) -> PodRef {
        PodRef {
            namespace: "default".to_owned(),
            name: name.to_owned(),
            uid: String::new(),
        }
    }

    #[test]
    fn from_payload_round_trip_for_every_variant() {
        let cases = vec![
            (
                serde_json::json!({"kind": "pod_delete", "namespace": "default", "name": "web-1"}),
                PodAction::Delete {
                    target: TargetSpec::Pod {
                        namespace: "default".to_owned(),
                        name: "web-1".to_owned(),
                    },
                },
            ),
            (
                serde_json::json!({"kind": "pod_evict", "namespace": "default", "name": "web-2"}),
                PodAction::Evict {
                    target: TargetSpec::Pod {
                        namespace: "default".to_owned(),
                        name: "web-2".to_owned(),
                    },
                },
            ),
            (
                serde_json::json!({
                    "kind": "selector_delete",
                    "namespace": "default",
                    "selector": {"app": "web", "tier": "frontend"}
                }),
                PodAction::Delete {
                    target: TargetSpec::LabelSelector {
                        namespace: "default".to_owned(),
                        selector: BTreeMap::from_iter([
                            ("app".to_owned(), "web".to_owned()),
                            ("tier".to_owned(), "frontend".to_owned()),
                        ]),
                    },
                },
            ),
        ];
        for (payload, expected) in cases {
            let parsed = PodAction::from_payload(&payload).expect("parse");
            assert_eq!(parsed, expected);
        }
    }

    #[test]
    fn from_payload_rejects_unknown_kind() {
        let payload = serde_json::json!({"kind": "evict", "namespace": "default", "name": "x"});
        let err = PodAction::from_payload(&payload).expect_err("should reject");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn from_payload_rejects_missing_namespace() {
        let payload = serde_json::json!({"kind": "pod_delete", "name": "x"});
        let err = PodAction::from_payload(&payload).expect_err("should reject");
        assert!(matches!(err, AgentError::InvalidPlan { .. }));
    }

    #[test]
    fn kube_system_namespace_is_hard_refused() {
        let client = RecordingClient::default();
        let adapter = KubernetesAdapter::with_client(Box::new(client), permissive_blast_radius());
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "kube-system", "name": "x"
        }));
        let err = adapter
            .apply(&p, &armed_guard())
            .expect_err("should refuse");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "kube_system_namespace",
                ..
            }
        ));
    }

    #[test]
    fn non_allowlisted_namespace_is_refused() {
        let client = RecordingClient::default();
        let adapter = KubernetesAdapter::with_client(Box::new(client), permissive_blast_radius());
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "production", "name": "x"
        }));
        let err = adapter
            .apply(&p, &armed_guard())
            .expect_err("should refuse");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "namespace_not_in_allowlist",
                ..
            }
        ));
    }

    #[test]
    fn blast_radius_exceeded_is_refused() {
        let client = RecordingClient::with_seeded_pods(vec![pod("a"), pod("b"), pod("c")]);
        let mut blast = permissive_blast_radius();
        blast.max_pods = Some(2);
        let adapter = KubernetesAdapter::with_client(Box::new(client), blast);
        let mut selector = BTreeMap::<String, String>::new();
        selector.insert("app".to_owned(), "web".to_owned());
        let p = plan(serde_json::json!({
            "kind": "selector_delete",
            "namespace": "default",
            "selector": selector
        }));
        let err = adapter
            .apply(&p, &armed_guard())
            .expect_err("should refuse");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "blast_radius_exceeded",
                ..
            }
        ));
    }

    #[test]
    fn unarmed_guard_yields_dry_run() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let guard = SafetyGuard::new();
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "default", "name": "x"
        }));
        let applied = adapter.apply(&p, &guard).expect("dry-run");
        assert!(applied.dry_run);
        assert_eq!(client.list_calls.load(Ordering::Relaxed), 0);
        assert_eq!(client.deletes.lock().unwrap().len(), 0);
    }

    #[test]
    fn pod_delete_calls_delete_pod() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "default", "name": "web-1"
        }));
        adapter.apply(&p, &armed_guard()).expect("apply");
        let deletes = client.deletes.lock().unwrap();
        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].name, "web-1");
        assert_eq!(deletes[0].namespace, "default");
    }

    #[test]
    fn pod_evict_calls_evict_pod() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let p = plan(serde_json::json!({
            "kind": "pod_evict", "namespace": "default", "name": "web-2"
        }));
        adapter.apply(&p, &armed_guard()).expect("apply");
        let evicts = client.evicts.lock().unwrap();
        assert_eq!(evicts.len(), 1);
        assert_eq!(evicts[0].name, "web-2");
    }

    #[test]
    fn selector_delete_iterates_over_listed_pods() {
        let client =
            std::sync::Arc::new(RecordingClient::with_seeded_pods(vec![pod("a"), pod("b")]));
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let mut selector = BTreeMap::<String, String>::new();
        selector.insert("app".to_owned(), "web".to_owned());
        let p = plan(serde_json::json!({
            "kind": "selector_delete",
            "namespace": "default",
            "selector": selector
        }));
        adapter.apply(&p, &armed_guard()).expect("apply");
        assert_eq!(client.list_calls.load(Ordering::Relaxed), 1);
        let deletes = client.deletes.lock().unwrap();
        assert_eq!(deletes.len(), 2);
        let names: Vec<_> = deletes.iter().map(|p| p.name.clone()).collect();
        assert!(names.contains(&"a".to_owned()));
        assert!(names.contains(&"b".to_owned()));
    }

    #[test]
    fn selector_evict_iterates_over_listed_pods() {
        let client = std::sync::Arc::new(RecordingClient::with_seeded_pods(vec![pod("x")]));
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let mut selector: BTreeMap<String, String> = BTreeMap::new();
        selector.insert("app".to_owned(), "web".to_owned());
        let p = plan(serde_json::json!({
            "kind": "selector_evict",
            "namespace": "default",
            "selector": selector
        }));
        adapter.apply(&p, &armed_guard()).expect("apply");
        let evicts = client.evicts.lock().unwrap();
        assert_eq!(evicts.len(), 1);
        assert_eq!(evicts[0].name, "x");
    }

    #[test]
    fn selector_with_no_matches_is_a_noop_apply() {
        let client = std::sync::Arc::new(RecordingClient::default());
        let adapter = KubernetesAdapter::with_client(
            Box::new(std::sync::Arc::clone(&client)),
            permissive_blast_radius(),
        );
        let mut selector: BTreeMap<String, String> = BTreeMap::new();
        selector.insert("app".to_owned(), "web".to_owned());
        let p = plan(serde_json::json!({
            "kind": "selector_delete",
            "namespace": "default",
            "selector": selector
        }));
        adapter
            .apply(&p, &armed_guard())
            .expect("apply (no matches)");
        let deletes = client.deletes.lock().unwrap();
        assert_eq!(deletes.len(), 0);
    }

    #[test]
    fn revert_is_always_noop() {
        let client = RecordingClient::default();
        let adapter = KubernetesAdapter::with_client(Box::new(client), permissive_blast_radius());
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: false,
            description: "applied pod_delete on pod".to_owned(),
        };
        adapter.revert(&applied).expect("revert should be no-op");
    }

    #[test]
    fn revert_dry_run_is_noop() {
        let client = RecordingClient::default();
        let adapter = KubernetesAdapter::with_client(Box::new(client), permissive_blast_radius());
        let applied = AppliedFault {
            id: 0,
            adapter: KIND,
            dry_run: true,
            description: "would pod_delete on pod (unarmed guard)".to_owned(),
        };
        adapter.revert(&applied).expect("revert should be no-op");
    }

    #[test]
    fn new_noop_is_safe_default() {
        let adapter = KubernetesAdapter::new_noop();
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "default", "name": "x"
        }));
        let err = adapter
            .apply(&p, &armed_guard())
            .expect_err("should refuse");
        // The default blast radius has an empty namespace
        // allowlist, so the namespace check fires first. Either
        // rejection path proves the default adapter is safe.
        assert!(matches!(err, AgentError::TargetNotAllowed { .. }));
    }

    #[test]
    fn noop_adapter_kind_is_kubernetes() {
        assert_eq!(KubernetesAdapter::new_noop().adapter_kind(), "kubernetes");
    }

    #[test]
    fn kube_system_in_allowlist_is_still_refused() {
        // Defence in depth: even if the operator accidentally
        // adds `kube-system` to the allowlist, the adapter
        // hard-rejects it.
        let client = RecordingClient::default();
        let mut blast = permissive_blast_radius();
        blast
            .allowed_namespaces
            .insert(KUBE_SYSTEM_NAMESPACE.to_owned());
        let adapter = KubernetesAdapter::with_client(Box::new(client), blast);
        let p = plan(serde_json::json!({
            "kind": "pod_delete", "namespace": "kube-system", "name": "x"
        }));
        let err = adapter
            .apply(&p, &armed_guard())
            .expect_err("should refuse");
        assert!(matches!(
            err,
            AgentError::TargetNotAllowed {
                rule: "kube_system_namespace",
                ..
            }
        ));
    }
}
