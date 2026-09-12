//! Bind checkpoint results to source contents, Nix outputs and retained evidence.
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{evidence, host, persistence, process, source};

mod scenarios;

#[derive(clap::Args)]
pub struct Args {
    /// Run source-bound reference, ordering, recovery and store checkpoint checks.
    #[arg(long, default_value = "C2", value_parser = ["C1", "C2", "C3", "C4", "C5"])]
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
    #[serde(default)]
    fixtures: BTreeMap<String, PathBuf>,
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

fn fixture_identity(
    info: &Value,
    scenario: &Value,
    build: &Build,
    case: scenarios::Fixture,
) -> io::Result<()> {
    let shared = case.wrapper == "shared";
    let workload = match case.wrapper {
        "pressure" => "pressure",
        "shared" => "shared",
        _ => "core",
    };
    if info["source_path"].as_str() != build.source_path.to_str()
        || info["harness"].as_str() != build.harness.to_str()
        || info["workload"] != workload
        || info["live_recovery"] != shared
        || *scenario != json!({"crash_at": case.cut})
    {
        return Err(io::Error::other(
            "fixture identity or crash scenario differs from checkpoint inventory",
        ));
    }
    Ok(())
}

fn validate_fixture(
    directory: &Path,
    build: &Build,
    case: scenarios::Fixture,
) -> io::Result<Vec<String>> {
    let run = directory.join("run");
    crate::fixture::verify(&run)?;
    let info: Value = evidence::read_json(&run.join("build.json"))?;
    let scenario: Value = evidence::read_json(&run.join("guest/scenario.json"))?;
    fixture_identity(&info, &scenario, build, case)?;
    Ok(vec![
        "nested fixture integrity and semantic checks passed".into(),
        "fixture source, executable, workload and crash cut matched the inventory".into(),
    ])
}

enum Validation<'a> {
    Command,
    Guest(&'a Path),
    Persistence(&'a Path),
    Fixture(&'a Build, scenarios::Fixture),
}

fn validate_model(directory: &Path, executable: &Path) -> io::Result<Vec<String>> {
    let run = directory.join("run");
    persistence::verify(&run)?;
    let report: Value = evidence::read_json(&run.join("model.json"))?;
    let actual: source::Entry = serde_json::from_value(report["binary"].clone())?;
    if actual != source::entry(executable)? {
        return Err(io::Error::other(
            "persistence model used the wrong executable",
        ));
    }
    Ok(vec![
        "all persistence cases and negative controls passed".into(),
        "model executable matched the build".into(),
    ])
}

fn scenario(
    id: &str,
    command: &mut Command,
    output: &Path,
    validation: Validation<'_>,
    report: &mut Report,
) -> io::Result<()> {
    process::check_interrupt()?;
    eprintln!("checkpoint {}: {id}", report.checkpoint);
    let directory = output.join("scenarios").join(id);
    let deadline = match &validation {
        Validation::Fixture(_, case) => case.deadline_seconds(),
        _ => 115,
    };
    let command_result = process::run_logged(command, &directory, Duration::from_secs(deadline))?;
    let validation = if command_result.exit_code != Some(0) || command_result.error.is_some() {
        Err(io::Error::other(
            "scenario command failed; see retained logs",
        ))
    } else {
        match validation {
            Validation::Command => Ok(vec!["command exited successfully".into()]),
            Validation::Guest(source) => validate_vm(&directory, source),
            Validation::Persistence(executable) => validate_model(&directory, executable),
            Validation::Fixture(build, case) => validate_fixture(&directory, build, case),
        }
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
    let inputs = source::checkout_inputs(&checkout, &output.join("inputs-before"))?;
    source::copy(&checkout, &output.join("source"), &inputs)?;
    evidence::write_json(&output.join("source.json"), &inputs)?;
    source::compare(
        &inputs,
        &source::scan(&build.source_path)?,
        "Nix build source",
    )?;
    let revision = source::text_command(
        &["git", "rev-parse", "HEAD"],
        &checkout,
        &output.join("revision"),
    )?;
    if build.source_revision.trim_end_matches("-dirty") != revision.trim() {
        return Err(io::Error::other(
            "build revision differs from the expected checkout",
        ));
    }
    source::text_command(
        &["git", "diff", "HEAD", "--binary"],
        &checkout,
        &output.join("dirty-patch"),
    )?;
    source::text_command(
        &["git", "status", "--porcelain=v1", "--untracked-files=all"],
        &checkout,
        &output.join("worktree"),
    )?;
    source::text_command(
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
            "guest_io": if matches!(args.checkpoint.as_str(), "C4" | "C5") {
                "direct fio references plus buffered ext4 and SQLite in two private filesystem guests; C5 also requires the declared competing-workload matrix"
            } else {
                "direct fio; workload contents and seeds retained under source/crates/harnesses/fio"
            },
            "host_payload_io": "O_DIRECT", "durability": "local; serial live reference flushes every write",
            "host_buffer_alignment_bytes":4096, "logical_block_bytes":4096, "virtio_sector_bytes":512,
            "reference_queues":{"count":1, "entries":128},
            "concurrent_queues":(matches!(args.checkpoint.as_str(), "C3" | "C4" | "C5")).then(|| json!({"count":4, "entries":256})),
            "fixture_inventory":scenarios::fixtures(&args.checkpoint).iter().map(|case| json!({"id":case.id,"crash_at":case.cut,"deadline_seconds":case.deadline_seconds()})).collect::<Vec<_>>(),
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
        .args(build.fixtures.values())
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
    for case in scenarios::fixtures(&args.checkpoint) {
        let wrapper = build
            .fixtures
            .get(case.wrapper)
            .ok_or_else(|| io::Error::other("missing fixture wrapper"))?;
        let executable = wrapper.join("bin").join(case.executable);
        if executables.contains_key(&executable) {
            continue;
        }
        let info: Value = evidence::read_json(&wrapper.join("share/cas/build.json"))?;
        if info["source_path"].as_str() != build.source_path.to_str()
            || info["harness"].as_str() != build.harness.to_str()
        {
            return Err(io::Error::other(
                "fixture wrapper differs from checkpoint build",
            ));
        }
        executables.insert(executable.clone(), source::entry(&executable)?);
        fs::copy(
            wrapper.join("share/cas/build.json"),
            output.join(format!("build-{}.json", case.wrapper)),
        )?;
    }
    evidence::write_json(&output.join("executables.json"), &executables)?;
    fs::create_dir(output.join("scenarios"))?;
    for (id, argv) in scenarios::CHECKS {
        let mut command = Command::new(argv[0]);
        command.args(&argv[1..]).current_dir(&checkout);
        scenario(id, &mut command, output, Validation::Command, report)?;
    }
    for (id, wrapper, extra) in scenarios::vms(&args.checkpoint) {
        let mut command = Command::new(require_wrapper(&build, wrapper)?.join("bin/cas-vm-smoke"));
        command
            .args(extra)
            .arg("--output")
            .arg(output.join("scenarios").join(id).join("run"))
            .arg("--expect-source")
            .arg(&build.source_path)
            .current_dir(&checkout);
        scenario(
            id,
            &mut command,
            output,
            Validation::Guest(&build.source_path),
            report,
        )?;
    }
    if matches!(args.checkpoint.as_str(), "C3" | "C4" | "C5") {
        let mut command = Command::new(&build.harness);
        command
            .arg("persistence")
            .arg("--output")
            .arg(output.join("scenarios/persistence-model/run"))
            .current_dir(&checkout);
        scenario(
            "persistence-model",
            &mut command,
            output,
            Validation::Persistence(&build.harness),
            report,
        )?;
    }
    for case in scenarios::fixtures(&args.checkpoint) {
        let wrapper = build
            .fixtures
            .get(case.wrapper)
            .ok_or_else(|| io::Error::other("missing fixture wrapper"))?;
        let mut command = Command::new(wrapper.join("bin").join(case.executable));
        command
            .arg("--checkout")
            .arg(&checkout)
            .arg("--output")
            .arg(output.join("scenarios").join(case.id).join("run"))
            .current_dir(&checkout);
        if let Some(cut) = case.cut {
            command.args(["--crash-at", cut]);
        }
        scenario(
            case.id,
            &mut command,
            output,
            Validation::Fixture(&build, case),
            report,
        )?;
    }
    source::compare(
        &inputs,
        &source::checkout_inputs(&checkout, &output.join("inputs-after"))?,
        "source changed during suite",
    )?;
    validate_results(report)
}

fn validate_results(report: &Report) -> io::Result<()> {
    let required = scenarios::required(&report.checkpoint);
    if report.schema_version != 1
        || !matches!(report.checkpoint.as_str(), "C1" | "C2" | "C3" | "C4" | "C5")
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
    if matches!(report.checkpoint.as_str(), "C3" | "C4" | "C5") {
        persistence::verify(&output.join("scenarios/persistence-model/run"))?;
    }
    if !scenarios::fixtures(&report.checkpoint).is_empty() {
        let build: Build = evidence::read_json(&output.join("build.json"))?;
        for case in scenarios::fixtures(&report.checkpoint) {
            validate_fixture(&output.join("scenarios").join(case.id), &build, case)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c3_requires_every_concurrent_and_persistence_case() {
        let mut report = passing_report();
        report.checkpoint = "C3".into();
        assert!(validate_results(&report).is_err());
        for id in scenarios::required("C3") {
            let scenario =
                serde_json::from_value(serde_json::to_value(&report.scenarios["staging"]).unwrap())
                    .unwrap();
            report.scenarios.insert(id.into(), scenario);
        }
        assert_eq!(report.scenarios.len(), 32);
        validate_results(&report).unwrap();
        for id in scenarios::required("C3") {
            let scenario = report.scenarios.remove(id).unwrap();
            assert!(validate_results(&report).is_err(), "missing {id}");
            report.scenarios.insert(id.into(), scenario);
        }
    }

    #[test]
    fn c4_and_c5_require_all_cases_and_exact_fixture_identity() {
        for (checkpoint, count) in [("C4", 40), ("C5", 41)] {
            let mut report = passing_report();
            report.checkpoint = checkpoint.into();
            for id in scenarios::required(checkpoint) {
                let scenario = serde_json::from_value(
                    serde_json::to_value(&report.scenarios["staging"]).unwrap(),
                )
                .unwrap();
                report.scenarios.insert(id.into(), scenario);
            }
            assert_eq!(report.scenarios.len(), count);
            validate_results(&report).unwrap();
            for case in scenarios::fixtures(checkpoint) {
                let removed = report.scenarios.remove(case.id).unwrap();
                assert!(validate_results(&report).is_err(), "missing {}", case.id);
                report.scenarios.insert(case.id.into(), removed);
            }
            let build = Build {
                source_revision: "revision".into(),
                source_path: "source".into(),
                harness: "harness".into(),
                package: "package".into(),
                tools: vec![],
                wrappers: BTreeMap::new(),
                fixtures: BTreeMap::new(),
            };
            for case in scenarios::fixtures(checkpoint) {
                let shared = case.wrapper == "shared";
                let info = json!({"source_path":"source", "harness":"harness", "workload":if case.wrapper == "pressure" {"pressure"} else if shared {"shared"} else {"core"}, "live_recovery":shared});
                let scenario = json!({"crash_at":case.cut});
                fixture_identity(&info, &scenario, &build, case).unwrap();
                for (key, wrong) in [
                    ("source_path", json!("other")),
                    ("harness", json!("other")),
                    ("workload", json!("wrong")),
                    ("live_recovery", json!(!shared)),
                ] {
                    let mut changed = info.clone();
                    changed[key] = wrong;
                    assert!(fixture_identity(&changed, &scenario, &build, case).is_err());
                }
                assert!(fixture_identity(&info, &json!({}), &build, case).is_err());
                assert!(
                    fixture_identity(&info, &json!({"crash_at":"wrong"}), &build, case).is_err()
                );
                assert!(
                    fixture_identity(
                        &info,
                        &json!({"crash_at":case.cut,"extra":true}),
                        &build,
                        case
                    )
                    .is_err()
                );
            }
        }
    }

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
            scenarios: scenarios::required("C1")
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
        for id in scenarios::required("C1") {
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
