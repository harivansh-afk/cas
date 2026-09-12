//! A fresh XFS VM with source-bound native IO and reflink evidence.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::{evidence, host, process, source};

mod checks;

#[derive(clap::Args)]
pub struct Args {
    /// Exact checkout used by the Nix build.
    #[arg(long, default_value = ".")]
    checkout: PathBuf,
    /// New directory; every retry keeps its own disk and evidence.
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
    vm: PathBuf,
    harness: PathBuf,
    tests: PathBuf,
    daemon_tests: PathBuf,
    qemu: PathBuf,
    qemu_executable: PathBuf,
    guest_kernel: String,
    service_deadline_seconds: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    schema_version: u32,
    passed: bool,
    started_at_utc: String,
    ended_at_utc: String,
    error: Option<String>,
    artifacts: source::Manifest,
}

fn execute(args: &Args) -> io::Result<()> {
    let output = &args.output;
    let checkout = args.checkout.canonicalize()?;
    let build: Build = evidence::read_json(&args.build_info)?;
    fs::copy(&args.build_info, output.join("build.json"))?;
    if std::env::current_exe()?.canonicalize()? != build.harness.canonicalize()? {
        return Err(io::Error::other("fixture must run its packaged harness"));
    }
    let inputs = source::checkout_inputs(&checkout, &output.join("inputs-before"))?;
    source::copy(&checkout, &output.join("source"), &inputs)?;
    evidence::write_json(&output.join("source.json"), &inputs)?;
    source::compare(&inputs, &source::scan(&build.source_path)?, "fixture build")?;
    let revision = source::text_command(
        &["git", "rev-parse", "HEAD"],
        &checkout,
        &output.join("revision"),
    )?;
    if build.source_revision.trim_end_matches("-dirty") != revision.trim() {
        return Err(io::Error::other("fixture build revision differs"));
    }
    source::text_command(
        &["git", "diff", "HEAD", "--binary"],
        &checkout,
        &output.join("dirty-patch"),
    )?;
    host::preflight(
        "XFS development fixture",
        &output.join("host.json"),
        &checkout,
    )?;
    let mut executables = BTreeMap::new();
    for path in [
        &build.vm,
        &build.harness,
        &build.tests,
        &build.daemon_tests,
        &build.qemu,
        &build.qemu_executable,
    ] {
        executables.insert(path.clone(), source::entry(path)?);
    }
    evidence::write_json(&output.join("executables.json"), &executables)?;
    let mut closure = Command::new("nix");
    closure.args(["path-info", "--recursive", "--json"]).args(
        executables
            .keys()
            .map(|path| path.parent().unwrap().parent().unwrap()),
    );
    let result = process::run_logged(
        &mut closure,
        &output.join("closure"),
        Duration::from_secs(100),
    )?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other("fixture closure capture failed"));
    }
    let service_deadline = build.service_deadline_seconds;
    let guest_deadline = service_deadline
        .checked_add(15)
        .ok_or_else(|| io::Error::other("fixture deadline overflow"))?;
    let guest = output.join("guest");
    let temporary = output.join("tmp");
    fs::create_dir(&guest)?;
    fs::create_dir(&temporary)?;
    let disk = output.join("xfs.img");
    File::options()
        .write(true)
        .create_new(true)
        .open(&disk)?
        .set_len(2 * 1024 * 1024 * 1024)?;
    evidence::write_json(
        &output.join("conditions.json"),
        &serde_json::json!({
            "profile":"development", "paper_gates":[], "checkpoint_complete":false,
            "guest_ram_bytes":2 * 1024_u64 * 1024 * 1024, "vcpus":4,
            "disk_bytes":2 * 1024_u64 * 1024 * 1024, "host_allocation":"sparse",
            "filesystem":"XFS", "payload_io":"O_DIRECT", "network":"none",
            "acceleration":"KVM", "service_deadline_seconds":service_deadline,
            "guest_deadline_seconds":guest_deadline, "guest_cache_state":"fresh boot",
            "host_cache_state":"uncontrolled; no performance acceptance",
        }),
    )?;
    let log = File::create(output.join("console.log"))?;
    let mut command = Command::new(&build.vm);
    command
        .env_clear()
        .current_dir(output)
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(
            ["HOME", "USER", "LOGNAME"]
                .into_iter()
                .filter_map(|key| std::env::var_os(key).map(|value| (key, value))),
        )
        .env("TMPDIR", &temporary)
        .env("CAS_XFS_IMAGE", &disk)
        .env("CAS_RESULTS_DIR", &guest)
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = process::ManagedChild::spawn(&mut command)?;
    let outcome = crate::qemu::record(&mut child, output)
        .and_then(|()| child.wait(Duration::from_secs(guest_deadline)));
    let exit = outcome
        .as_ref()
        .ok()
        .map(|status| process::exit_code(*status));
    evidence::write_json(
        &output.join("guest-exit.json"),
        &serde_json::json!({
            "exit_code":exit, "error":outcome.as_ref().err().map(ToString::to_string),
        }),
    )?;
    outcome?;
    checks::verify(output, &build)?;
    source::compare(
        &inputs,
        &source::checkout_inputs(&checkout, &output.join("inputs-after"))?,
        "source changed during fixture",
    )
}

pub fn run(mut args: Args) -> io::Result<()> {
    args.output = crate::qemu::prepare_output(&args.output)?;
    let mut report = Report {
        schema_version: 4,
        passed: false,
        started_at_utc: host::utc_now()?,
        ended_at_utc: String::new(),
        error: None,
        artifacts: BTreeMap::new(),
    };
    let result = execute(&args);
    report.passed = result.is_ok();
    report.error = result.as_ref().err().map(ToString::to_string);
    report.ended_at_utc = host::utc_now()?;
    report.artifacts = source::scan(&args.output)?;
    evidence::write_json(&args.output.join("fixture.json"), &report)?;
    result?;
    verify(&args.output)
}

pub fn verify(output: &Path) -> io::Result<()> {
    let report: Report = evidence::read_json(&output.join("fixture.json"))?;
    if report.schema_version != 4 || !report.passed || report.error.is_some() {
        return Err(io::Error::other("XFS fixture failed"));
    }
    let mut actual = source::scan(output)?;
    actual.remove(Path::new("fixture.json"));
    source::compare(&report.artifacts, &actual, "retained fixture evidence")?;
    let inputs = evidence::read_json(&output.join("source.json"))?;
    source::compare(
        &inputs,
        &source::scan(&output.join("source"))?,
        "archived fixture source",
    )?;
    let build: Build = evidence::read_json(&output.join("build.json"))?;
    checks::verify(output, &build)
}
