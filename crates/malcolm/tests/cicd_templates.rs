//! CI/CD template structure validation.
//!
//! The T32 deliverable ships three YAML artifacts (a GitHub composite
//! action, an example GitHub workflow, and a GitLab CI template) and a
//! shared shell script. This test enforces the surface area: every
//! file must parse, and each one must carry the inputs/outputs the
//! documentation promises. Failing this test means a refactor has
//! silently broken the public contract that downstream consumers
//! depend on.

// Integration tests assert invariants via `.expect()` and surface
// violations with labelled `panic!` messages. The lint contract
// forbids both in production code; here they are the right tool, so
// we scope the exceptions to this whole-file test module.
#![expect(clippy::expect_used, reason = "test assertions")]
#![expect(clippy::panic, reason = "test assertions")]

use std::path::{Path, PathBuf};

use serde_json::Value;

/// Workspace root, computed from this crate's manifest dir at compile time.
fn workspace_root() -> PathBuf {
    let manifest = env!("CARGO_MANIFEST_DIR");
    Path::new(manifest)
        .parent()
        .and_then(|p| p.parent())
        .map_or_else(|| PathBuf::from(manifest), PathBuf::from)
}

/// Read a YAML file from the workspace root and parse it as JSON.
/// `serde_yaml` produces a `Value` that is structurally a subset of
/// `serde_json::Value`; we convert via `serde_json::to_value` so we
/// can use `pointer()` navigation in tests.
fn parse_workspace_yaml(rel: &str) -> Value {
    let path = workspace_root().join(rel);
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let parsed: serde_yaml::Value =
        serde_yaml::from_str(&raw).unwrap_or_else(|e| panic!("yaml parse {}: {e}", path.display()));
    serde_json::to_value(parsed)
        .unwrap_or_else(|e| panic!("yaml→json convert {}: {e}", path.display()))
}

fn required_string<'a>(doc: &'a Value, pointer: &str, label: &str) -> &'a str {
    let value = doc
        .pointer(pointer)
        .unwrap_or_else(|| panic!("{label}: missing pointer {pointer}"));
    value
        .as_str()
        .unwrap_or_else(|| panic!("{label}: pointer {pointer} is not a string"))
}

fn required_seq<'a>(doc: &'a Value, pointer: &str, label: &str) -> &'a [Value] {
    let value = doc
        .pointer(pointer)
        .unwrap_or_else(|| panic!("{label}: missing pointer {pointer}"));
    value
        .as_array()
        .unwrap_or_else(|| panic!("{label}: pointer {pointer} is not a sequence"))
}

#[test]
fn action_yml_has_required_surface() {
    let doc = parse_workspace_yaml(".github/actions/malcolm-resilience/action.yml");

    // Top-level shape.
    assert_eq!(
        required_string(&doc, "/name", "action"),
        "malcolm-resilience"
    );
    assert_eq!(
        required_string(&doc, "/runs/using", "action.runs"),
        "composite"
    );

    // All inputs documented as required in the task spec are present.
    let inputs = required_seq(&doc, "/runs/steps", "action.steps");
    assert!(!inputs.is_empty(), "composite action must declare steps");

    // The composite must shell out to the gate step that invokes malcolm-run.
    let names: Vec<&str> = inputs
        .iter()
        .filter_map(|s| s.get("name").and_then(Value::as_str))
        .collect();
    assert!(
        names.iter().any(|n| n.contains("resilience gate")),
        "expected a step named 'resilience gate'; got {names:?}",
    );

    // Inputs block exposes preset, budget, junit, sarif — these are the
    // contracts a downstream workflow depends on.
    for pointer in [
        "/inputs/preset",
        "/inputs/budget",
        "/inputs/junit",
        "/inputs/sarif",
    ] {
        assert!(
            doc.pointer(pointer).is_some(),
            "action.yml: missing required input {pointer}; downstream consumers depend on it"
        );
    }

    // Outputs block advertises passed / violations-count / report-path so
    // call sites can read them.
    for pointer in [
        "/outputs/passed",
        "/outputs/violations-count",
        "/outputs/report-path",
    ] {
        assert!(
            doc.pointer(pointer).is_some(),
            "action.yml: missing required output {pointer}",
        );
    }
}

#[test]
fn resilience_workflow_has_required_jobs_and_permissions() {
    let doc = parse_workspace_yaml(".github/workflows/resilience.yml");

    assert_eq!(
        required_string(&doc, "/name", "workflow"),
        "Resilience gate"
    );
    assert!(
        doc.pointer("/permissions/contents").is_some(),
        "workflow must declare permissions: contents: read"
    );
    assert!(
        doc.pointer("/concurrency/group").is_some(),
        "workflow must declare a concurrency group"
    );

    // The job that runs the gate must invoke our composite action.
    let job = doc
        .pointer("/jobs/resilience-gate")
        .expect("resilience-gate job must exist");
    let uses_steps: Vec<&str> =
        job.get("steps")
            .and_then(Value::as_array)
            .map_or(Vec::new(), |steps| {
                steps
                    .iter()
                    .filter_map(|s| s.get("uses").and_then(Value::as_str))
                    .collect()
            });
    assert!(
        !uses_steps.is_empty(),
        "resilience-gate job must contain at least one `uses:` step"
    );
    assert!(
        uses_steps.iter().any(|u| u.contains("malcolm-resilience")),
        "job must invoke ./.github/actions/malcolm-resilience; got {uses_steps:?}"
    );

    // SARIF upload is conditional on `always()` so a failed gate still
    // surfaces in code-scanning.
    let uploads_sarif = job
        .get("steps")
        .and_then(Value::as_array)
        .is_some_and(|steps| {
            steps.iter().any(|s| {
                s.get("uses")
                    .and_then(Value::as_str)
                    .is_some_and(|u| u.contains("upload-sarif"))
            })
        });
    assert!(uploads_sarif, "workflow must upload SARIF to code-scanning");
}

#[test]
fn gitlab_template_defines_a_resilience_job() {
    let doc = parse_workspace_yaml("ci/malcolm-resilience.gitlab-ci.yml");

    let job = doc
        .pointer("/malcolm-resilience")
        .expect("malcolm-resilience job must exist");
    assert_eq!(required_string(job, "/stage", "job"), "test");
    assert!(job.pointer("/image").is_some(), "job must pin an image");
    assert!(job.pointer("/script").is_some(), "job must define script:");

    // The script body must mention the budget flag so operators can
    // spot-check that the budget is wired through.
    let script = required_seq(job, "/script", "job.script");
    let script_text: String = script
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        script_text.contains("--budget"),
        "gitlab script must pass --budget to malcolm-run"
    );
    assert!(
        script_text.contains("--junit"),
        "gitlab script must produce a junit report"
    );
    assert!(
        script_text.contains("--sarif"),
        "gitlab script must produce a sarif report"
    );
    assert!(
        job.pointer("/artifacts/reports/junit").is_some(),
        "gitlab artifacts must declare junit report so MR widget picks it up"
    );
}

#[test]
fn budget_sample_parses_and_is_valid() {
    // The sample budget that ships with the templates must itself be a
    // valid ResilienceBudget — otherwise the workflow fails on day one.
    let raw = std::fs::read_to_string(workspace_root().join("ci/budget.toml"))
        .unwrap_or_else(|e| panic!("read ci/budget.toml: {e}"));
    let budget: malcolm::assertions::ResilienceBudget = toml::from_str(&raw)
        .unwrap_or_else(|e| panic!("ci/budget.toml is not a valid budget: {e}"));

    // Sample budget: min_injected_total=1, max_injected_total=100.
    assert_eq!(budget.min_injected_total, Some(1));
    assert_eq!(budget.max_injected_total, Some(100));
    let per_type = budget
        .max_injected_per_fault_type
        .as_ref()
        .expect("sample budget should declare per-type caps");
    assert_eq!(per_type.get("packet_loss"), Some(&50));
    assert_eq!(per_type.get("network_partition"), Some(&50));
    assert!(
        budget
            .require_fault_types
            .as_ref()
            .is_some_and(|v| v.iter().any(|s| s == "packet_loss")),
        "sample budget must require packet_loss"
    );
    assert!(
        budget.forbid_regime.as_ref().is_some_and(|v| !v.is_empty()),
        "sample budget should forbid at least one regime"
    );
}

#[test]
fn shell_script_is_valid_bash_and_executable() {
    let path = workspace_root().join("scripts/resilience-gate.sh");
    let meta = std::fs::metadata(&path).unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
    assert!(meta.is_file(), "{} should be a file", path.display());

    // Executable bit must be set so `bash` invocation isn't required.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = meta.permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "{} must have at least one executable bit set; got mode {mode:o}",
            path.display()
        );
    }

    // `bash -n` parses without executing — catches any syntax drift.
    let raw =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let bash_check = std::process::Command::new("bash")
        .args(["-n", &path.to_string_lossy()])
        .output();
    match bash_check {
        Ok(out) => assert!(
            out.status.success(),
            "bash -n rejected {}:\nstderr: {}\nscript: {raw}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => panic!("could not spawn bash to syntax-check: {e}"),
    }
}
