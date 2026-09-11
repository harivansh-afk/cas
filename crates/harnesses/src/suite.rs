//! Bind checkpoint results to source contents, Nix outputs and retained evidence.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{evidence, host, process, source};

const REQUIRED: [&str; 14] = [
    "rust-format",
    "clippy",
    "rust-tests",
    "diff-check",
    "nix-format",
    "nix-check",
    "raw-qemu",
    "raw-daemon",
    "staging",
    "fresh-recovery",
    "live-before-submit",
    "live-after-storage",
    "live-after-status",
    "live-after-used",
];

#[derive(clap::Args)]
pub struct Args {
    /// Run all reference checks and, for C2, the packed synchronous backend.
    #[arg(long, default_value = "C2", value_parser = ["C1", "C2"])]
    checkpoint: String,
    /// Checkout whose exact contents must match the Nix build.
    #[arg(long, default_value = ".")]
    checkout: PathBuf,
    /// New directory; retain each retry in a separate directory.
    #[arg(long)]
    output: PathBuf,
    #[arg(long, hide = true)]
    build_info: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Build {
    source_revision: String,
    source_path: PathBuf,
    harness: PathBuf,
    package: PathBuf,
    tools: Vec<PathBuf>,
    wrappers: BTreeMap<String, PathBuf>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    passed: bool,
    command: process::CommandResult,
    assertions: Vec<String>,
    error: Option<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u32,
    checkpoint: String,
    passed: bool,
    started_at_utc: String,
    ended_at_utc: String,
    scenarios: BTreeMap<String, Scenario>,
    artifacts: source::Manifest,
    error: Option<String>,
}

fn text_command(argv: &[&str], cwd: &Path, directory: &Path) -> io::Result<String> {
    let mut command = Command::new(argv[0]);
    command.args(&argv[1..]).current_dir(cwd);
    let result = process::run_logged(&mut command, directory, Duration::from_secs(100))?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other(format!(
            "{} failed; see {}",
            argv[0],
            directory.display()
        )));
    }
    fs::read_to_string(directory.join("stdout.log"))
}

fn checkout_inputs(checkout: &Path, directory: &Path) -> io::Result<source::Manifest> {
    let files = text_command(
        &[
            "git",
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
        checkout,
        directory,
    )?;
    source::from_paths(
        checkout,
        files
            .split('\0')
            .filter(|path| !path.is_empty())
            .map(PathBuf::from),
    )
}

fn require_wrapper<'a>(build: &'a Build, name: &str) -> io::Result<&'a Path> {
    build
        .wrappers
        .get(name)
        .map(PathBuf::as_path)
        .ok_or_else(|| io::Error::other(format!("missing {name} wrapper")))
}

fn validate_vm(directory: &Path, source: &Path) -> io::Result<Vec<String>> {
    let summary: Value = evidence::read_json(&directory.join("run/summary.json"))?;
    if summary["passed"] != true || summary["paper_gate"] != Value::Null {
        return Err(io::Error::other(
            "VM scenario did not pass as a development check",
        ));
    }
    if summary["build"]["source_path"].as_str() != source.to_str() {
        return Err(io::Error::other("VM scenario used the wrong build source"));
    }
    let build: Value = evidence::read_json(&directory.join("run/build.json"))?;
    if build != summary["build"] {
        return Err(io::Error::other(
            "VM summary and retained build identity disagree",
        ));
    }
    let lock = fs::read(directory.join("run/flake.lock"))?;
    if lock != fs::read(source.join("flake.lock"))? {
        return Err(io::Error::other("VM lock differs from tested source"));
    }
    Ok(vec![
        "guest and backend assertions passed".into(),
        "build source and lock matched".into(),
    ])
}

fn scenario(
    id: &str,
    command: &mut Command,
    output: &Path,
    build_source: Option<&Path>,
    report: &mut Report,
) -> io::Result<()> {
    process::check_interrupt()?;
    eprintln!("checkpoint {}: {id}", report.checkpoint);
    let directory = output.join("scenarios").join(id);
    let command_result = process::run_logged(command, &directory, Duration::from_secs(115))?;
    let validation = if command_result.exit_code != Some(0) || command_result.error.is_some() {
        Err(io::Error::other(
            "scenario command failed; see retained logs",
        ))
    } else if let Some(source) = build_source {
        validate_vm(&directory, source)
    } else {
        Ok(vec!["command exited successfully".into()])
    };
    let (passed, assertions, error) = match validation {
        Ok(assertions) => (true, assertions, None),
        Err(error) => (false, Vec::new(), Some(error.to_string())),
    };
    let result = Scenario {
        passed,
        command: command_result,
        assertions,
        error,
    };
    evidence::write_json(&directory.join("result.json"), &result)?;
    report.scenarios.insert(id.into(), result);
    Ok(())
}

fn execute(args: &Args, report: &mut Report) -> io::Result<()> {
    let checkout = args.checkout.canonicalize()?;
    let output = &args.output;
    let build: Build = evidence::read_json(&args.build_info)?;
    fs::copy(&args.build_info, output.join("build.json"))?;
    if std::env::current_exe()?.canonicalize()? != build.harness.canonicalize()? {
        return Err(io::Error::other(
            "suite is not running the harness from its Nix build",
        ));
    }
    let inputs = checkout_inputs(&checkout, &output.join("inputs-before"))?;
    source::copy(&checkout, &output.join("source"), &inputs)?;
    evidence::write_json(&output.join("source.json"), &inputs)?;
    source::compare(
        &inputs,
        &source::scan(&build.source_path)?,
        "Nix build source",
    )?;
    let revision = text_command(
        &["git", "rev-parse", "HEAD"],
        &checkout,
        &output.join("revision"),
    )?;
    if build.source_revision.trim_end_matches("-dirty") != revision.trim() {
        return Err(io::Error::other(
            "build revision differs from the expected checkout",
        ));
    }
    text_command(
        &["git", "diff", "HEAD", "--binary"],
        &checkout,
        &output.join("dirty-patch"),
    )?;
    text_command(
        &["git", "status", "--porcelain=v1", "--untracked-files=all"],
        &checkout,
        &output.join("worktree"),
    )?;
    text_command(
        &["cargo", "metadata", "--locked", "--format-version=1"],
        &checkout,
        &output.join("cargo-graph"),
    )?;
    host::preflight(
        &format!("{} development suite", args.checkpoint),
        &output.join("host.json"),
        &checkout,
    )?;
    evidence::write_json(
        &output.join("conditions.json"),
        &json!({
            "profile": "development", "host_cpu_affinity": host::cpu_affinity()?,
            "host_cache_state": "uncontrolled; no paper measurement",
            "guest_state": "fresh boot and new scratch image per scenario; live recovery retains one guest",
            "guest_io": "direct fio; workload contents and seeds retained under source/crates/harnesses/fio",
            "host_payload_io": "O_DIRECT", "durability": "local; serial live reference flushes every write",
            "host_buffer_alignment_bytes":4096, "logical_block_bytes":4096, "virtio_sector_bytes":512,
            "queue_count":1, "queue_entries":128,
            "scenario_deadline_seconds":115, "guest_boot_deadline_seconds":90,
            "cargo_features":"workspace defaults; resolved features in cargo-graph/stdout.log",
            "paper_gates":[],
        }),
    )?;
    let mut closure = Command::new("nix");
    closure
        .args(["path-info", "--recursive", "--json"])
        .arg(&build.package)
        .args(build.wrappers.values())
        .args(&build.tools)
        .current_dir(&checkout);
    let result = process::run_logged(
        &mut closure,
        &output.join("closure"),
        Duration::from_secs(100),
    )?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other("failed to capture Nix closure"));
    }
    let mut executables = BTreeMap::new();
    executables.insert(build.harness.clone(), source::entry(&build.harness)?);
    for (name, wrapper) in &build.wrappers {
        let info_path = wrapper.join("share/cas/build.json");
        let info: Value = evidence::read_json(&info_path)?;
        if info["source_path"].as_str() != build.source_path.to_str()
            || info["harness"].as_str() != build.harness.to_str()
        {
            return Err(io::Error::other(format!("wrong build in {name} wrapper")));
        }
        executables.insert(
            wrapper.join("bin/cas-vm-smoke"),
            source::entry(&wrapper.join("bin/cas-vm-smoke"))?,
        );
        for key in ["harness", "daemon", "qemu"] {
            if let Some(path) = info[key].as_str() {
                let path = PathBuf::from(path);
                executables.insert(path.clone(), source::entry(&path)?);
            }
        }
        fs::copy(info_path, output.join(format!("build-{name}.json")))?;
    }
    evidence::write_json(&output.join("executables.json"), &executables)?;
    fs::create_dir(output.join("scenarios"))?;
    for (id, argv) in [
        (
            "rust-format",
            vec!["cargo", "fmt", "--all", "--", "--check"],
        ),
        (
            "clippy",
            vec![
                "cargo",
                "clippy",
                "--workspace",
                "--all-targets",
                "--locked",
                "--",
                "-D",
                "warnings",
            ],
        ),
        (
            "rust-tests",
            vec!["cargo", "test", "--workspace", "--locked"],
        ),
        ("diff-check", vec!["git", "diff", "HEAD", "--check"]),
        ("nix-format", vec!["nix", "fmt", "--", "--ci"]),
        ("nix-check", vec!["nix", "flake", "check"]),
    ] {
        let mut command = Command::new(argv[0]);
        command.args(&argv[1..]).current_dir(&checkout);
        scenario(id, &mut command, output, None, report)?;
    }
    let mut vm_scenarios = vec![
        ("raw-qemu", "raw", vec![]),
        ("raw-daemon", "daemon", vec![]),
        ("staging", "staging", vec![]),
        ("fresh-recovery", "staging", vec!["--recovery"]),
        (
            "live-before-submit",
            "staging",
            vec!["--live-recovery", "--crash-at", "before-submit"],
        ),
        (
            "live-after-storage",
            "staging",
            vec!["--live-recovery", "--crash-at", "after-storage"],
        ),
        (
            "live-after-status",
            "staging",
            vec!["--live-recovery", "--crash-at", "after-status"],
        ),
        (
            "live-after-used",
            "staging",
            vec!["--live-recovery", "--crash-at", "after-used"],
        ),
    ];
    if args.checkpoint == "C2" {
        vm_scenarios.extend([
            ("local-sync", "local", vec![]),
            ("local-fresh-recovery", "local", vec!["--recovery"]),
        ]);
    }
    for (id, wrapper, extra) in vm_scenarios {
        let mut command = Command::new(require_wrapper(&build, wrapper)?.join("bin/cas-vm-smoke"));
        command
            .args(extra)
            .arg("--output")
            .arg(output.join("scenarios").join(id).join("run"))
            .arg("--expect-source")
            .arg(&build.source_path)
            .current_dir(&checkout);
        scenario(id, &mut command, output, Some(&build.source_path), report)?;
    }
    source::compare(
        &inputs,
        &checkout_inputs(&checkout, &output.join("inputs-after"))?,
        "source changed during suite",
    )?;
    validate_results(report)
}

fn validate_results(report: &Report) -> io::Result<()> {
    let mut required = REQUIRED.to_vec();
    if report.checkpoint == "C2" {
        required.extend(["local-sync", "local-fresh-recovery"]);
    }
    if report.schema_version != 1
        || !matches!(report.checkpoint.as_str(), "C1" | "C2")
        || report.scenarios.len() != required.len()
    {
        return Err(io::Error::other(
            "incomplete or unsupported checkpoint suite",
        ));
    }
    for id in required {
        let result = report
            .scenarios
            .get(id)
            .ok_or_else(|| io::Error::other(format!("missing scenario {id}")))?;
        if !result.passed
            || result.command.exit_code != Some(0)
            || result.command.error.is_some()
            || result.error.is_some()
            || result.assertions.is_empty()
        {
            return Err(io::Error::other(format!("required scenario {id} failed")));
        }
    }
    Ok(())
}

pub fn run(mut args: Args) -> io::Result<()> {
    args.output = std::path::absolute(&args.output)?;
    fs::create_dir_all(
        args.output
            .parent()
            .ok_or_else(|| io::Error::other("output requires a parent"))?,
    )?;
    fs::create_dir(&args.output)?;
    let mut report = Report {
        schema_version: 1,
        checkpoint: args.checkpoint.clone(),
        passed: false,
        started_at_utc: host::utc_now()?,
        ended_at_utc: String::new(),
        scenarios: BTreeMap::new(),
        artifacts: BTreeMap::new(),
        error: None,
    };
    let result = execute(&args, &mut report);
    report.passed = result.is_ok();
    report.error = result.as_ref().err().map(ToString::to_string);
    report.ended_at_utc = host::utc_now()?;
    report.artifacts = source::scan(&args.output)?;
    evidence::write_json(&args.output.join("suite.json"), &report)?;
    result?;
    verify(&args.output)
}

pub fn verify(output: &Path) -> io::Result<()> {
    let report: Report = evidence::read_json(&output.join("suite.json"))?;
    validate_results(&report)?;
    if !report.passed || report.error.is_some() {
        return Err(io::Error::other("suite failed"));
    }
    let mut actual = source::scan(output)?;
    actual.remove(Path::new("suite.json"));
    source::compare(&report.artifacts, &actual, "retained evidence")?;
    for (id, expected) in &report.scenarios {
        for file in ["stdout.log", "stderr.log", "command.json", "result.json"] {
            let key = PathBuf::from("scenarios").join(id).join(file);
            if !actual.contains_key(&key) {
                return Err(io::Error::other(format!(
                    "required evidence missing: {}",
                    key.display()
                )));
            }
        }
        let directory = output.join("scenarios").join(id);
        let command: process::CommandResult = evidence::read_json(&directory.join("command.json"))?;
        let scenario: Scenario = evidence::read_json(&directory.join("result.json"))?;
        if command != expected.command || scenario != *expected {
            return Err(io::Error::other(format!(
                "{id} disagrees with its retained result"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c2_requires_both_local_guest_scenarios() {
        let mut report = passing_report();
        report.checkpoint = "C2".into();
        assert!(validate_results(&report).is_err());
        for id in ["local-sync", "local-fresh-recovery"] {
            let scenario =
                serde_json::from_value(serde_json::to_value(&report.scenarios["staging"]).unwrap())
                    .unwrap();
            report.scenarios.insert(id.into(), scenario);
        }
        validate_results(&report).unwrap();
        report.scenarios.get_mut("local-sync").unwrap().passed = false;
        assert!(validate_results(&report).is_err());
    }

    fn passing_report() -> Report {
        Report {
            schema_version: 1,
            checkpoint: "C1".into(),
            passed: true,
            started_at_utc: String::new(),
            ended_at_utc: String::new(),
            artifacts: BTreeMap::new(),
            error: None,
            scenarios: REQUIRED
                .into_iter()
                .map(|id| {
                    (
                        id.into(),
                        Scenario {
                            passed: true,
                            assertions: vec!["fixture assertion".into()],
                            error: None,
                            command: process::CommandResult {
                                argv: vec!["fixture".into()],
                                started_at_utc: String::new(),
                                ended_at_utc: String::new(),
                                deadline_seconds: 1,
                                elapsed_seconds: 0.0,
                                exit_code: Some(0),
                                error: None,
                            },
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn missing_skipped_failed_or_unsupported_scenarios_cannot_pass() {
        let mut report = passing_report();
        validate_results(&report).unwrap();
        report
            .scenarios
            .get_mut("staging")
            .unwrap()
            .command
            .exit_code = Some(7);
        assert!(validate_results(&report).is_err());
        report = passing_report();
        report.scenarios.remove("raw-qemu");
        assert!(validate_results(&report).is_err());
        report = passing_report();
        report.schema_version = 2;
        assert!(validate_results(&report).is_err());
    }

    #[test]
    fn retained_evidence_cannot_disappear_after_success() {
        let directory = tempfile::tempdir().unwrap();
        let mut report = passing_report();
        for id in REQUIRED {
            let scenario = directory.path().join("scenarios").join(id);
            fs::create_dir_all(&scenario).unwrap();
            for file in ["stdout.log", "stderr.log"] {
                fs::write(scenario.join(file), "fixture").unwrap();
            }
            evidence::write_json(
                &scenario.join("command.json"),
                &report.scenarios[id].command,
            )
            .unwrap();
            evidence::write_json(&scenario.join("result.json"), &report.scenarios[id]).unwrap();
        }
        report.artifacts = source::scan(directory.path()).unwrap();
        evidence::write_json(&directory.path().join("suite.json"), &report).unwrap();
        verify(directory.path()).unwrap();
        fs::remove_file(directory.path().join("scenarios/staging/stdout.log")).unwrap();
        assert!(verify(directory.path()).is_err());
    }
}
