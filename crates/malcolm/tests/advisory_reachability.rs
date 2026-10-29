//! Advisory reachability regression test (issue #66).
//!
//! Five `RustSec` advisories are ignored in `deny.toml` and `.cargo/audit.toml`.
//! The justification for every one of them is the same: the affected crate is
//! reachable only through the optional `bayesopt` feature, so a default build
//! never links it and the vulnerable code never runs.
//!
//! An advisory ignore is a promise that the vulnerable code is unreachable.
//! This test is what makes that promise checkable instead of aspirational: if
//! a future change makes `zip`, `bincode`, or `paste` reachable from a default
//! build, the ignore stops being justified and this test fails rather than
//! silently continuing to suppress the signal.
//!
//! The reachability model is deliberately conservative — it computes an
//! over-approximation of the default-feature dependency closure and asserts the
//! vulnerable crates are absent from it. Over-approximating the closure can
//! only make the test stricter (a false alarm on a crate that is genuinely
//! unreachable), never let a genuinely reachable crate slip through.

#![expect(clippy::panic, reason = "test assertions")]

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

/// Workspace root, computed from this crate's manifest dir at compile time.
fn workspace_root() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest)
        .parent()
        .and_then(Path::parent)
        .map_or_else(|| PathBuf::from(manifest), PathBuf::from)
}

/// The workspace members whose dependency graph the advisories ride on.
const MEMBERS: [&str; 4] = ["malcolm-core", "malcolm", "malcolm-lens", "malcolm-agent"];

/// Crates the five ignored advisories apply to.
///
/// * `zip` — RUSTSEC-2026-0367, RUSTSEC-2026-0368, RUSTSEC-2026-0369
/// * `bincode` — RUSTSEC-2025-0141
/// * `paste` — RUSTSEC-2024-0436
const ADVISORY_CRATES: [&str; 3] = ["zip", "bincode", "paste"];

/// The advisory IDs the ignore lists are expected to carry.
const ADVISORY_IDS: [&str; 5] = [
    "RUSTSEC-2024-0436",
    "RUSTSEC-2025-0141",
    "RUSTSEC-2026-0367",
    "RUSTSEC-2026-0368",
    "RUSTSEC-2026-0369",
];

/// A parsed `Cargo.lock`: package name -> the package names it depends on.
type LockGraph = BTreeMap<String, BTreeSet<String>>;

/// A workspace member's manifest, reduced to the facts the reachability
/// computation needs.
struct MemberManifest {
    /// Dependencies declared `optional = true`.
    optional: BTreeSet<String>,
    /// Dependencies reachable through the member's `default` feature set.
    default_enabled: BTreeSet<String>,
    /// Dev-dependencies, which never ship in a published build.
    dev: BTreeSet<String>,
}

/// Read a workspace file as text.
fn read_workspace_file(rel: &str) -> String {
    let path = workspace_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Read and parse a workspace TOML file.
fn read_workspace_toml(rel: &str) -> toml::Value {
    let raw = read_workspace_file(rel);
    toml::from_str(&raw).unwrap_or_else(|e| panic!("parse {rel}: {e}"))
}

/// Parse `Cargo.lock` into a package -> dependencies graph.
///
/// Lockfile dependency entries are either a bare package name (`"serde"`) or a
/// name qualified with a version when several versions of that crate are in
/// the graph (`"thiserror 2.0.19"`). Only the name matters for reachability.
fn parse_lock_graph() -> LockGraph {
    let value = read_workspace_toml("Cargo.lock");

    let packages = value
        .get("package")
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("Cargo.lock has no [[package]] entries"));

    let mut graph = LockGraph::new();
    for package in packages {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or_else(|| panic!("Cargo.lock package entry has no name"));
        let deps = package
            .get("dependencies")
            .and_then(toml::Value::as_array)
            .map(|deps| {
                deps.iter()
                    .filter_map(toml::Value::as_str)
                    .map(|dep| dep.split_whitespace().next().unwrap_or(dep).to_owned())
                    .collect()
            })
            .unwrap_or_default();
        graph.insert(name.to_owned(), deps);
    }
    graph
}

/// Parse one member's `Cargo.toml` into the facts the reachability computation
/// needs.
///
/// Both manifest spellings of an optional dependency are handled: the inline
/// form (`dep = { version = "1", optional = true }`) and the table form
/// (`[dependencies.dep]` with `optional = true` inside). Cargo normalises the
/// second into the first, but only because the TOML parser does — the
/// `optional` key can sit in either place relative to the dependency entry.
fn parse_member_manifest(crate_name: &str) -> MemberManifest {
    let value = read_workspace_toml(&format!("crates/{crate_name}/Cargo.toml"));

    let dependencies = value.get("dependencies").and_then(toml::Value::as_table);
    let optional_deps: BTreeSet<String> = dependencies
        .map(|deps| {
            deps.iter()
                .filter(|(_, spec)| {
                    spec.as_table()
                        .and_then(|table| table.get("optional"))
                        .and_then(toml::Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default();

    let dev_deps: BTreeSet<String> = value
        .get("dev-dependencies")
        .and_then(toml::Value::as_table)
        .map(|deps| deps.keys().cloned().collect())
        .unwrap_or_default();

    let default_enabled_deps = resolve_default_features(&value, &optional_deps);

    MemberManifest {
        optional: optional_deps,
        default_enabled: default_enabled_deps,
        dev: dev_deps,
    }
}

/// Expand a manifest's `default` feature into the set of optional dependencies
/// it enables.
///
/// Cargo's feature syntax has four shapes that matter here:
///
/// * `dep:name` — enables the optional dependency `name`.
/// * `name/feature` — enables the optional dependency `name` (and a feature of
///   it). The dependency is only enabled if it is actually optional.
/// * `name?/feature` — enables a feature of `name` *without* enabling the
///   dependency itself.
/// * `name` — refers to another entry in the `[features]` table, or to an
///   optional dependency of the same name (the implicit feature).
fn resolve_default_features(
    manifest: &toml::Value,
    optional_deps: &BTreeSet<String>,
) -> BTreeSet<String> {
    let features = manifest
        .get("features")
        .and_then(toml::Value::as_table)
        .cloned()
        .unwrap_or_default();

    let mut enabled = BTreeSet::new();
    let mut queue: VecDeque<String> = features
        .get("default")
        .and_then(toml::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(toml::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    while let Some(entry) = queue.pop_front() {
        if !seen.insert(entry.clone()) {
            continue;
        }
        if let Some(dep) = entry.strip_prefix("dep:") {
            enabled.insert(dep.to_owned());
            continue;
        }
        if let Some((dep, _feature)) = entry.split_once('/') {
            // `name?/feature` does not enable the dependency itself.
            if !dep.ends_with('?') && optional_deps.contains(dep) {
                enabled.insert(dep.to_owned());
            }
            continue;
        }
        if let Some(nested) = features.get(&entry).and_then(toml::Value::as_array) {
            queue.extend(
                nested
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(str::to_owned),
            );
            continue;
        }
        // A bare name is the implicit feature of an optional dependency.
        if optional_deps.contains(&entry) {
            enabled.insert(entry);
        }
    }

    enabled
}

/// Compute the set of packages reachable from the workspace members.
///
/// `cut_edges` holds `(from, to)` pairs that are not followed. The walk keeps
/// every other lockfile edge, including optional dependencies of third-party
/// crates that a given feature selection would not activate — so the result is
/// an over-approximation of the real build graph.
fn reachable(graph: &LockGraph, cut_edges: &BTreeSet<(String, String)>) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = MEMBERS.iter().map(|m| (*m).to_owned()).collect();

    while let Some(name) = queue.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        let Some(deps) = graph.get(&name) else {
            continue;
        };
        for dep in deps {
            if cut_edges.contains(&(name.clone(), dep.clone())) {
                continue;
            }
            if !seen.contains(dep) {
                queue.push_back(dep.clone());
            }
        }
    }

    seen
}

/// The edges that a default-feature build does not follow.
///
/// Two kinds are cut:
///
/// 1. A workspace member's optional dependency that its `default` feature set
///    does not enable. This is what severs the `bayesopt` chain.
/// 2. Every dev-dependency. Dev-dependencies are not part of a published
///    build, so a crate that is only ever a dev-dependency is not something a
///    consumer of `malcolm` can be attacked through.
///
/// Edges between third-party crates are never cut, even when they represent
/// an optional dependency that only a non-default feature would activate. The
/// result is therefore an over-approximation, which is the safe direction.
fn default_feature_cut_edges(
    manifests: &BTreeMap<String, MemberManifest>,
) -> BTreeSet<(String, String)> {
    let mut cut = BTreeSet::new();
    for (member, manifest) in manifests {
        for dep in &manifest.optional {
            if !manifest.default_enabled.contains(dep) {
                cut.insert((member.clone(), dep.clone()));
            }
        }
        for dep in &manifest.dev {
            cut.insert((member.clone(), dep.clone()));
        }
    }
    cut
}

/// Every workspace member's manifest, keyed by package name.
fn member_manifests() -> BTreeMap<String, MemberManifest> {
    MEMBERS
        .iter()
        .map(|name| ((*name).to_owned(), parse_member_manifest(name)))
        .collect()
}

/// The reverse-dependency map of the lockfile graph.
fn reverse_dependencies(graph: &LockGraph) -> BTreeMap<String, BTreeSet<String>> {
    let mut reverse: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for (name, deps) in graph {
        for dep in deps {
            reverse.entry(dep.clone()).or_default().insert(name.clone());
        }
    }
    reverse
}

/// The advisory IDs in a `deny.toml`-style ignore list.
///
/// `deny.toml` uses a table of `{ id, reason }`; `.cargo/audit.toml` uses a
/// plain list of strings. Both are read through the TOML parser so that a
/// mention in a prose comment can never be mistaken for a live entry.
fn ignored_advisory_ids(document: &toml::Value) -> BTreeSet<String> {
    let entries = document
        .get("advisories")
        .and_then(|advisories| advisories.get("ignore"))
        .and_then(toml::Value::as_array)
        .unwrap_or_else(|| panic!("no [[advisories]] ignore list found"));

    let mut ids = BTreeSet::new();
    for entry in entries {
        let id = match entry {
            toml::Value::String(id) => id.clone(),
            toml::Value::Table(table) => table
                .get("id")
                .and_then(toml::Value::as_str)
                .unwrap_or_else(|| panic!("advisory ignore entry has no `id`"))
                .to_owned(),
            other => panic!("unexpected advisory ignore entry: {other:?}"),
        };
        ids.insert(id);
    }
    ids
}

#[test]
fn advisory_crates_are_unreachable_from_a_default_build() {
    let graph = parse_lock_graph();
    let manifests = member_manifests();
    let cut = default_feature_cut_edges(&manifests);

    let default_closure = reachable(&graph, &cut);

    for crate_name in ADVISORY_CRATES {
        assert!(
            !default_closure.contains(crate_name),
            "`{crate_name}` is reachable from a default-feature build, so its advisory \
             ignore is no longer justified. Either the reachability changed (revisit the \
             ignore) or the ignore should be removed."
        );
    }
}

#[test]
fn advisory_crates_are_still_reachable_with_all_features() {
    // Guards the test above against silently degrading: if the cut logic ever
    // severed the `bayesopt` chain by accident, the "unreachable" assertion
    // would pass for the wrong reason. With every feature enabled the three
    // crates must come back.
    let graph = parse_lock_graph();

    let full_closure = reachable(&graph, &BTreeSet::new());

    for crate_name in ADVISORY_CRATES {
        assert!(
            full_closure.contains(crate_name),
            "`{crate_name}` is not in the all-features dependency closure, so the \
             advisory ignore refers to a crate the workspace no longer depends on."
        );
    }
}

#[test]
fn zip_is_reachable_only_through_the_bayesopt_chain() {
    // The `zip` advisories concern the archive *extraction* path
    // (`ZipArchive::extract` / `ZipStreamReader::extract`). The defence is that
    // `ndarray-npy` reads `.npz` payloads in memory via `by_index` / `by_name`
    // and never calls `extract()`. That defence only holds while `zip` has
    // exactly one path into the dependency graph: through `ndarray-npy`.
    //
    // If a second crate starts depending on `zip` — or `ndarray-npy` gains a
    // direct `malcolm` dependency — the "in-memory read only" reasoning no
    // longer covers every caller, and the ignore needs re-evaluating.
    let graph = parse_lock_graph();
    let reverse = reverse_dependencies(&graph);

    let zip_dependents = reverse
        .get("zip")
        .unwrap_or_else(|| panic!("`zip` has no dependents in Cargo.lock"));

    assert_eq!(
        zip_dependents
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        vec!["ndarray-npy"],
        "`zip` gained a dependent other than `ndarray-npy`; the advisory ignore is \
         justified by `ndarray-npy` reading archives in memory, which does not \
         cover a different caller."
    );

    let manifests = member_manifests();
    let malcolm = manifests
        .get("malcolm")
        .unwrap_or_else(|| panic!("`malcolm` manifest not parsed"));

    assert!(
        malcolm.optional.contains("egobox-ego"),
        "`malcolm` no longer declares `egobox-ego` as an optional dependency; the \
         `zip` reachability chain it anchored has changed."
    );
    assert!(
        malcolm.optional.contains("ndarray"),
        "`malcolm` no longer declares `ndarray` as an optional dependency; the \
         `zip` reachability chain it anchored has changed."
    );
    assert!(
        !malcolm.default_enabled.contains("egobox-ego")
            && !malcolm.default_enabled.contains("ndarray"),
        "the `bayesopt` feature is enabled by default, which puts `zip` in every \
         consumer's build and makes the advisory ignore unjustified."
    );
}

#[test]
fn advisory_ignore_lists_stay_in_sync() {
    // `deny.toml` and `.cargo/audit.toml` carry the same decisions so the
    // `cargo deny` and `cargo audit` gates agree. If one is updated and the
    // other is not, the gates disagree about what is acceptable — the exact
    // drift issue #66 was opened for.
    let deny_ids = ignored_advisory_ids(&read_workspace_toml("deny.toml"));
    let audit_ids = ignored_advisory_ids(&read_workspace_toml(".cargo/audit.toml"));

    assert_eq!(
        deny_ids, audit_ids,
        "`deny.toml` and `.cargo/audit.toml` disagree about the ignored advisories."
    );
    assert_eq!(
        deny_ids.iter().map(String::as_str).collect::<Vec<_>>(),
        ADVISORY_IDS.to_vec(),
        "the set of ignored advisories changed; update the reasoning in both files \
         and the issue #66 tracking notes."
    );
}
