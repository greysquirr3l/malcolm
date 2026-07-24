//! `malcolm-run` — command-line scenario runner.
//!
//! Loads a named scenario preset, applies user-supplied overrides, runs the
//! scenario, and writes a JSON or YAML report. With `--record` the run is
//! persisted as a [`ScenarioRecord`] for later replay.
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

use malcolm::fault::FaultContext;
use malcolm::presets::{PRESET_NAMES, preset};
use malcolm::replay::{RecordingHarness, ScenarioRecord};
use malcolm::scenario::ChaosScenario;
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
    -h, --help               Print this help text and exit.

EXIT CODES:
    0  success
    1  argument error
    2  unknown preset / profile
    3  i/o error
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
                other => return Err(format!("unrecognised argument: {other}")),
            }
        }
        Ok(args)
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

fn run() -> Result<(), Box<dyn Error>> {
    let args = Args::parse().map_err(|err| {
        eprintln!("error: {err}\n\n{USAGE}");
        err
    })?;

    if args.help {
        print!("{USAGE}");
        return Ok(());
    }

    if args.list_presets {
        for name in PRESET_NAMES {
            println!("{name}");
        }
        return Ok(());
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
            eprintln!("error: statsd recorder construction failed: {error}");
            return Err(error.into());
        }
    };
    #[cfg(feature = "statsd")]
    let hub = malcolm::metrics::MetricsHub::new().with_recorder(statsd_recorder.clone());

    let json_payload = if args.dry_run {
        let report = scenario.dry_run(&ctx);
        serde_json::to_string_pretty(&DryRunReportJson::from(&report))?
    } else {
        #[cfg(feature = "statsd")]
        let report = scenario.run_with_metrics(&mut ctx, &hub);
        #[cfg(not(feature = "statsd"))]
        let report = scenario.run(&mut ctx);
        serde_json::to_string_pretty(&report)?
    };

    if let Some(path) = &args.output {
        fs::write(path, &json_payload)?;
        eprintln!("wrote report to {}", path.display());
    } else {
        let mut stdout = io::stdout().lock();
        stdout.write_all(json_payload.as_bytes())?;
        stdout.write_all(b"\n")?;
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

    Ok(())
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

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}
