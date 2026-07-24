//! `malcolm-run` — command-line scenario runner.
//!
//! Loads a named scenario preset, applies user-supplied overrides, runs the
//! scenario, and writes a JSON or YAML report. With `--record` the run is
//! persisted as a [`ScenarioRecord`] for later replay. With `--budget` the
//! run is evaluated against a `ResilienceBudget` and (on breach) the binary
//! exits with code `3` so CI can distinguish a policy failure from a crash.
//!
//! # Usage
//!
//! ```text
//! # List all built-in presets.
//! malcolm-run --list-presets
//!
//! # Run a preset with a custom seed, write the JSON report to stdout.
//! malcolm-run --preset flaky_net --seed 7
//!
//! # Run, evaluate against a budget, fail the pipeline on breach.
//! malcolm-run --preset flaky_net --budget ci/budget.toml
//!
//! # Dry-run against a specific node id.
//! malcolm-run --preset slow_disk --node "db-0" --dry-run
//!
//! # Run, then record the run for deterministic replay.
//! malcolm-run --preset byzantine_cluster --seed 19 --record run.yaml
//! ```

use std::error::Error;
use std::fs;
use std::io::{self, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use malcolm::assertions::{BudgetError, BudgetOutcome, ResilienceBudget, format_outcome};
use malcolm::fault::FaultContext;
use malcolm::presets::{PRESET_NAMES, preset};
use malcolm::replay::{RecordingHarness, ScenarioRecord};
use malcolm::scenario::{ChaosScenario, ScenarioReport};
use malcolm_core::bifurcation::BifurcationProfile;
use malcolm_core::types::DryRunReport;

const USAGE: &str = "\
malcolm-run — execute a named scenario preset and report the result

USAGE:
    malcolm-run [OPTIONS]

OPTIONS:
    --list-presets           Print built-in preset names and exit.
    --preset <NAME>          Preset to execute (see --list-presets).
    --seed <N>               Override the scenario seed (default: 42).
    --node <ID>              Target node id (default: node-0).
    --profile <LABEL>        Override the bifurcation profile label.
                             One of: network_partition, memory_pressure,
                             latency_cascade, byzantine_node, clock_skew.
    --dry-run                Run dry-run mode; do not actually inject.
    --output <FILE>          Write the JSON report to FILE (default: stdout).
    --record <FILE>          Also write a ScenarioRecord to FILE.
                             Format chosen from extension: .yaml/.yml or .json.
    --budget <FILE>          Load a ResilienceBudget from FILE
                             (.toml, .json, .yaml, or .yml) and evaluate
                             it after the run. Use --fail-fast to stop on
                             the first violation (default: accumulate all).
    --assert-min-injected N  Inline shortcut: merge `min_injected_total = N`
                             into the budget.
    --assert-max-injected N  Inline shortcut: merge `max_injected_total = N`
                             into the budget.
    --fail-fast              Stop evaluating budget rules after the first
                             violation (default: report every violation).
    --junit <FILE>           Write a JUnit XML report to FILE (consumed by
                             GitHub Actions, GitLab, Jenkins, Buildkite).
    --sarif <FILE>           Write a SARIF 2.1.0 report to FILE (consumed
                             by GitHub code-scanning Checks annotations).
    -h, --help               Print this help text and exit.

EXIT CODES:
    0  success / budget satisfied
    1  argument error
    2  unknown preset / profile
    3  budget violated (only when --budget, --assert-min-injected, or
       --assert-max-injected is supplied)
    4  i/o error
";

struct Args {
    list_presets: bool,
    help: bool,
    preset: Option<String>,
    seed: u64,
    node: String,
    profile: Option<String>,
    dry_run: bool,
    output: Option<PathBuf>,
    record: Option<PathBuf>,
    /// Path to a `ResilienceBudget` file (TOML / JSON / YAML).
    budget: Option<PathBuf>,
    /// Inline `--assert-min-injected N` shortcut.
    assert_min_injected: Option<u64>,
    /// Inline `--assert-max-injected N` shortcut.
    assert_max_injected: Option<u64>,
    /// Stop evaluating budget rules after the first violation.
    fail_fast: bool,
    /// Path to write a JUnit XML report to.
    junit: Option<PathBuf>,
    /// Path to write a SARIF 2.1.0 report to.
    sarif: Option<PathBuf>,
}

impl Args {
    fn parse() -> Result<Self, String> {
        let mut args = Self {
            list_presets: false,
            help: false,
            preset: None,
            seed: 42,
            node: "node-0".to_owned(),
            profile: None,
            dry_run: false,
            output: None,
            record: None,
            budget: None,
            assert_min_injected: None,
            assert_max_injected: None,
            fail_fast: false,
            junit: None,
            sarif: None,
        };

        let mut iter = std::env::args().skip(1);
        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--list-presets" => args.list_presets = true,
                "-h" | "--help" => args.help = true,
                "--preset" => {
                    args.preset = Some(value_of(&mut iter, "--preset")?);
                }
                "--seed" => {
                    let raw = value_of(&mut iter, "--seed")?;
                    args.seed = raw.parse().map_err(|_| {
                        format!("--seed expects a non-negative integer, got {raw:?}")
                    })?;
                }
                "--node" => {
                    args.node = value_of(&mut iter, "--node")?;
                }
                "--profile" => {
                    args.profile = Some(value_of(&mut iter, "--profile")?);
                }
                "--dry-run" => args.dry_run = true,
                "--output" => {
                    let raw = value_of(&mut iter, "--output")?;
                    args.output = Some(PathBuf::from(raw));
                }
                "--record" => {
                    let raw = value_of(&mut iter, "--record")?;
                    args.record = Some(PathBuf::from(raw));
                }
                "--budget" => {
                    let raw = value_of(&mut iter, "--budget")?;
                    args.budget = Some(PathBuf::from(raw));
                }
                "--assert-min-injected" => {
                    let raw = value_of(&mut iter, "--assert-min-injected")?;
                    args.assert_min_injected = Some(raw.parse().map_err(|_| {
                        format!("--assert-min-injected expects a non-negative integer, got {raw:?}")
                    })?);
                }
                "--assert-max-injected" => {
                    let raw = value_of(&mut iter, "--assert-max-injected")?;
                    args.assert_max_injected = Some(raw.parse().map_err(|_| {
                        format!("--assert-max-injected expects a non-negative integer, got {raw:?}")
                    })?);
                }
                "--fail-fast" => args.fail_fast = true,
                "--junit" => {
                    let raw = value_of(&mut iter, "--junit")?;
                    args.junit = Some(PathBuf::from(raw));
                }
                "--sarif" => {
                    let raw = value_of(&mut iter, "--sarif")?;
                    args.sarif = Some(PathBuf::from(raw));
                }
                other => return Err(format!("unrecognised argument: {other}")),
            }
        }
        Ok(args)
    }

    /// Build the effective budget: file-based (if `--budget` was supplied)
    /// merged with `--assert-min-injected` / `--assert-max-injected` overrides.
    /// Returns `Ok(None)` if no budget was requested at all.
    fn build_budget(&self) -> Result<Option<ResilienceBudget>, BudgetError> {
        if self.budget.is_none()
            && self.assert_min_injected.is_none()
            && self.assert_max_injected.is_none()
        {
            return Ok(None);
        }
        let mut budget = if let Some(path) = &self.budget {
            ResilienceBudget::from_file(path)?
        } else {
            ResilienceBudget::default()
        };
        if let Some(n) = self.assert_min_injected {
            budget.min_injected_total = Some(n);
        }
        if let Some(n) = self.assert_max_injected {
            budget.max_injected_total = Some(n);
        }
        Ok(Some(budget))
    }
}

fn value_of(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn profile_for(label: &str) -> Result<BifurcationProfile, String> {
    match label {
        "network_partition" => Ok(BifurcationProfile::network_partition()),
        "memory_pressure" => Ok(BifurcationProfile::memory_pressure()),
        "latency_cascade" => Ok(BifurcationProfile::latency_cascade()),
        "byzantine_node" => Ok(BifurcationProfile::byzantine_node()),
        "clock_skew" => Ok(BifurcationProfile::clock_skew()),
        other => Err(format!("unknown profile label: {other}")),
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let args = Args::parse().map_err(|err| {
        eprintln!("error: {err}\n\n{USAGE}");
        err
    })?;

    if args.help {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    if args.list_presets {
        for name in PRESET_NAMES {
            println!("{name}");
        }
        return Ok(ExitCode::SUCCESS);
    }

    let preset_name = args
        .preset
        .as_deref()
        .ok_or_else(|| "missing --preset (try --list-presets)".to_string())
        .map_err(|err| {
            eprintln!("error: {err}\n\n{USAGE}");
            err
        })?;

    let builder = preset(preset_name).ok_or_else(|| {
        let err = format!("unknown preset: {preset_name}");
        eprintln!("error: {err}");
        err
    })?;

    let profile = match &args.profile {
        Some(label) => profile_for(label)?,
        None => BifurcationProfile::network_partition(),
    };

    let scenario: ChaosScenario = builder.seed(args.seed).profile(profile).build();

    let mut ctx = FaultContext {
        seed: args.seed,
        timestamp_ms: 0,
        node_id: args.node.clone(),
        profile: scenario.profile(),
    };

    // Optional StatsD wiring: when the `statsd` feature is enabled, build a
    // recorder from environment variables and route the scenario through
    // `run_with_metrics` so per-fault emission reaches the collector. The
    // StatsD destination is intentionally UDP/fire-and-forget; construction
    // failures surface as a non-zero exit so CI does not silently drop
    // telemetry.
    #[cfg(feature = "statsd")]
    let statsd_recorder = match malcolm::metrics::statsd::StatsdRecorder::with_config(
        malcolm::metrics::statsd::StatsdConfig::from_env()?,
    ) {
        Ok(recorder) => recorder,
        Err(error) => {
            // Exit code 4 is reserved for I/O / exporter init failures; using
            // 3 here would be misclassified as a budget violation by CI
            // dashboards that branch on exit code 3.
            eprintln!("error: statsd recorder construction failed: {error}");
            return Ok(ExitCode::from(4));
        }
    };
    #[cfg(feature = "statsd")]
    let hub = malcolm::metrics::MetricsHub::new().with_recorder(statsd_recorder.clone());

    // Run the scenario (or dry-run) and capture the report so we can both
    // emit it AND evaluate the budget against it.
    let report: ScenarioReport = if args.dry_run {
        let dry = scenario.dry_run(&ctx);
        ScenarioReport {
            name: dry.name.clone(),
            seed: dry.seed,
            regime: malcolm::scenario::ScenarioRegime::Stable,
            events: Vec::new(),
            total_duration_ms: 0,
        }
    } else {
        #[cfg(feature = "statsd")]
        {
            scenario.run_with_metrics(&mut ctx, &hub)
        }
        #[cfg(not(feature = "statsd"))]
        {
            scenario.run(&mut ctx)
        }
    };

    let json_payload = if args.dry_run {
        let dry = scenario.dry_run(&ctx);
        serde_json::to_string_pretty(&DryRunReportJson::from(&dry))?
    } else {
        serde_json::to_string_pretty(&ReportJson::from(&report))?
    };

    // Evaluate the budget (if any). Dry-run reports always have zero events
    // so a budget with `min_injected_total > 0` would always fail; we
    // document that as a warning on stderr instead of an exit code.
    let budget = args.build_budget().map_err(|err| {
        eprintln!("error: {err}");
        err
    })?;

    let outcome = budget.as_ref().map(|b| {
        if args.dry_run && budget.as_ref().is_some() {
            eprintln!(
                "warning: budget evaluation against a dry-run report; \
                 dry-run never produces events, so min_injected_total > 0 \
                 will always fail and max_injected_total will always pass"
            );
        }
        let mut outcome = b.evaluate(&report);
        if args.fail_fast && outcome.violations.len() > 1 {
            outcome.violations.truncate(1);
        }
        outcome
    });

    let final_payload = if let Some(outcome) = &outcome {
        wrap_report_with_budget(&json_payload, outcome)
    } else {
        json_payload
    };

    if let Some(path) = &args.output {
        fs::write(path, &final_payload)?;
        eprintln!("wrote report to {}", path.display());
    } else {
        let mut stdout = io::stdout().lock();
        stdout.write_all(final_payload.as_bytes())?;
        stdout.write_all(b"\n")?;
    }

    if let Some(outcome) = &outcome {
        eprintln!("{}", format_outcome(outcome));
    }

    // JUnit XML and SARIF are pure functions over the report + outcome;
    // write them *after* the stdout payload so a failure here does not
    // corrupt the JSON report. I/O errors here propagate up to main()
    // as exit code 4.
    if let Some(junit_path) = &args.junit {
        let xml = malcolm::report_formats::to_junit_xml(&report, outcome.as_ref());
        fs::write(junit_path, xml)?;
        eprintln!("wrote junit report to {}", junit_path.display());
    }
    if let Some(sarif_path) = &args.sarif {
        let sarif = malcolm::report_formats::to_sarif(&report, outcome.as_ref());
        let json = serde_json::to_string_pretty(&sarif)?;
        fs::write(sarif_path, json)?;
        eprintln!("wrote sarif report to {}", sarif_path.display());
    }

    #[cfg(feature = "statsd")]
    statsd_recorder.shutdown();

    if let Some(record_path) = &args.record {
        if args.dry_run {
            eprintln!("warning: --dry-run with --record captures the run only on a real run");
        } else {
            let record = RecordingHarness::new(&scenario).record(&mut ctx);
            write_record(&record, record_path)?;
            eprintln!("wrote record to {}", record_path.display());
        }
    }

    // Exit code 3 (policy) only when a budget was requested AND violated.
    if let Some(outcome) = outcome {
        if !outcome.passed {
            return Ok(ExitCode::from(3));
        }
    }

    Ok(ExitCode::SUCCESS)
}

fn write_record(record: &ScenarioRecord, path: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let payload = match ext.as_str() {
        "yaml" | "yml" => record.to_yaml()?,
        _ => record.to_json()?,
    };
    fs::write(path, payload)?;
    Ok(())
}

#[derive(serde::Serialize)]
struct DryRunReportJson {
    name: String,
    seed: u64,
    would_inject_any: bool,
    fault_reports: Vec<DryRunReportJsonEntry>,
}

#[derive(serde::Serialize)]
struct DryRunReportJsonEntry {
    fault_type: String,
    node_id: String,
    would_inject: bool,
    reason: String,
}

impl From<&malcolm::scenario::ScenarioDryRunReport> for DryRunReportJson {
    fn from(report: &malcolm::scenario::ScenarioDryRunReport) -> Self {
        Self {
            name: report.name.clone(),
            seed: report.seed,
            would_inject_any: report.would_inject_any,
            fault_reports: report
                .fault_reports
                .iter()
                .map(|r: &DryRunReport| DryRunReportJsonEntry {
                    fault_type: r.fault_type.clone(),
                    node_id: r.node_id.clone(),
                    would_inject: r.would_inject,
                    reason: r.reason.clone(),
                })
                .collect(),
        }
    }
}

#[derive(serde::Serialize)]
struct ReportJson {
    name: String,
    seed: u64,
    regime: String,
    events: Vec<malcolm::scenario::ScenarioEvent>,
    total_duration_ms: u64,
}

impl From<&ScenarioReport> for ReportJson {
    fn from(report: &ScenarioReport) -> Self {
        Self {
            name: report.name.clone(),
            seed: report.seed,
            regime: format!("{:?}", report.regime).to_lowercase(),
            events: report.events.clone(),
            total_duration_ms: report.total_duration_ms,
        }
    }
}

/// Wrap a JSON payload with a `budget` block at the top level. If the input
/// is not valid JSON, return it unchanged.
fn wrap_report_with_budget(payload: &str, outcome: &BudgetOutcome) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return payload.to_owned();
    };
    if let Some(obj) = value.as_object_mut() {
        let budget = serde_json::json!({
            "passed": outcome.passed,
            "violations": outcome.violations,
        });
        obj.insert("budget".to_owned(), budget);
    }
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| payload.to_owned())
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(4)
        }
    }
}
