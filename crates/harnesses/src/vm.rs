//! Run the packaged guest on fresh scratch storage and retain its evidence.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;

use crate::evidence::{
    self, Backend, Build, DISK_BYTES, DaemonReport, Fio, FlushMarker, GuestCompletion, IO_BYTES,
    read_json,
};
use crate::{
    host,
    process::{self, ManagedChild},
};

// Linux 6.17.13 can delay io_uring worker exit for five seconds. See the
// daemon-lifetime review; this is a shutdown limit, not an IO timing metric.
const DAEMON_SHUTDOWN: Duration = Duration::from_secs(10);

mod live;

#[derive(clap::Args)]
pub struct Args {
    /// New results directory. Existing results are never overwritten.
    #[arg(long)]
    output: PathBuf,
    /// Existing filesystem directory for fresh scratch storage; defaults to output.
    #[arg(long)]
    disk_dir: Option<PathBuf>,
    /// Kill staging after guest FLUSH, then verify from a fresh guest.
    #[arg(long)]
    recovery: bool,
    /// Restart a serial, write-through staging daemon while the same guest runs.
    #[arg(long, conflicts_with = "recovery")]
    live_recovery: bool,
    /// Deterministic boundary at the 32nd write in the live-recovery check.
    #[arg(long, default_value = "after-storage", value_parser = ["before-submit", "after-storage", "after-status", "after-used"])]
    crash_at: String,
    /// Timeout in seconds for each guest boot.
    #[arg(long, default_value_t = 90, value_parser = clap::value_parser!(u64).range(1..=100))]
    timeout: u64,
    #[arg(long, hide = true)]
    vm: PathBuf,
    #[arg(long, hide = true)]
    build_info: PathBuf,
    #[arg(long, hide = true)]
    lock: PathBuf,
}

#[derive(Clone, Copy)]
enum Phase {
    Smoke,
    Write,
    Read,
}

#[derive(Default, Serialize)]
struct PhaseEvidence {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    launcher: Vec<PathBuf>,
    guest_exit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    verified_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    written_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_exit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon_shutdown_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    flush_marker: Option<FlushMarker>,
    #[serde(skip_serializing_if = "Option::is_none")]
    live_recovery: Option<Value>,
}

#[derive(Serialize)]
struct Summary {
    schema_version: u32,
    artifact: String,
    passed: bool,
    paper_gate: Option<String>,
    started_at_utc: String,
    host_machine: String,
    host_kernel: String,
    host_cpu_affinity: Vec<usize>,
    disk_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    invocation_worktree: Option<process::Capture>,
    #[serde(skip_serializing_if = "Option::is_none")]
    raw_image: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    storage_image: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    host_filesystem: Option<process::Capture>,
    #[serde(flatten)]
    guest: PhaseEvidence,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    phases: BTreeMap<String, PhaseEvidence>,
    wall_seconds_including_boot: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

fn valid_qemu_path(path: &Path) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::other("QEMU paths must be UTF-8"))?;
    if value.contains([',', '\n', '\r']) {
        return Err(io::Error::other(
            "QEMU paths cannot contain commas or line breaks",
        ));
    }
    Ok(())
}

fn prepare_output(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    valid_qemu_path(&path)?;
    if path.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output must be a new directory",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("output needs a parent directory"))?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    valid_qemu_path(&parent)?;
    let path = parent.join(
        path.file_name()
            .ok_or_else(|| io::Error::other("invalid output path"))?,
    );
    fs::create_dir(&path)?;
    Ok(path)
}

fn scratch_image(backend: Backend, directory: &Path) -> io::Result<PathBuf> {
    if backend == Backend::Staging {
        return Ok(tempfile::Builder::new()
            .prefix("cas-staging-")
            .tempdir_in(directory)?
            .keep()
            .join("image.log"));
    }
    let (file, path) = tempfile::Builder::new()
        .prefix("cas-smoke-")
        .suffix(".raw")
        .tempfile_in(directory)?
        .keep()
        .map_err(io::Error::other)?;
    // SAFETY: the descriptor stays open; the offset and length are representable.
    let result = unsafe { libc::posix_fallocate(file.as_raw_fd(), 0, DISK_BYTES as libc::off_t) };
    if result != 0 {
        return Err(io::Error::from_raw_os_error(result));
    }
    file.sync_all()?;
    Ok(path)
}

fn environment(image: &Path, results: &Path, temporary: &Path) -> BTreeMap<OsString, OsString> {
    let mut env: BTreeMap<_, _> = std::env::vars_os()
        .filter(|(key, _)| {
            let key = key.to_string_lossy();
            !key.starts_with("QEMU_")
                && !key.starts_with("NIX_GUEST_")
                && !matches!(
                    key.as_ref(),
                    "NIX_DISK_IMAGE" | "SHARED_DIR" | "CAS_VHOST_SOCKET"
                )
        })
        .collect();
    env.insert("CAS_RAW_IMAGE".into(), image.into());
    env.insert("CAS_RESULTS_DIR".into(), results.into());
    env.insert("TMPDIR".into(), temporary.into());
    env.insert("USE_TMPDIR".into(), "1".into());
    env
}

fn logged_command(
    program: &Path,
    output: &Path,
    log: &str,
    env: &BTreeMap<OsString, OsString>,
) -> io::Result<Command> {
    let file = File::options()
        .write(true)
        .create_new(true)
        .open(output.join(log))?;
    let mut command = Command::new(program);
    command
        .current_dir(output)
        .env_clear()
        .envs(env)
        .stdout(file.try_clone()?)
        .stderr(file);
    Ok(command)
}

fn execute_guest(
    args: &Args,
    build: &Build,
    output: &Path,
    image: &Path,
    phase: Phase,
    evidence: &mut PhaseEvidence,
) -> io::Result<()> {
    let results = output.join("guest");
    fs::create_dir(&results)?;
    let temporary = output.join("tmp");
    fs::create_dir(&temporary)?;
    let read_only = matches!(phase, Phase::Read);
    if !matches!(phase, Phase::Smoke) {
        fs::write(
            results.join("recovery-phase"),
            if read_only { "read\n" } else { "write\n" },
        )?;
    }
    let mut env = environment(image, &results, &temporary);
    // Socket paths must fit AF_UNIX even when the output directory is long.
    let socket_directory = if build.backend != Backend::Raw {
        Some(
            tempfile::Builder::new()
                .prefix("cas-vhost-")
                .tempdir_in("/tmp")?,
        )
    } else {
        None
    };
    let mut daemon = if let Some(directory) = &socket_directory {
        let socket = directory.path().join("block.sock");
        env.insert("CAS_VHOST_SOCKET".into(), socket.clone().into());
        let program = build
            .daemon
            .as_ref()
            .ok_or_else(|| io::Error::other("build is missing its daemon path"))?;
        let mut command = logged_command(program, output, "daemon.log", &env)?;
        command
            .arg("--socket")
            .arg(&socket)
            .arg("--image")
            .arg(image)
            .arg("--report")
            .arg(output.join("daemon.json"));
        if build.backend == Backend::Staging {
            command.args(["--backend", "staging"]);
            if !read_only {
                command.arg("--create-bytes").arg(DISK_BYTES.to_string());
            }
        }
        let mut child = ManagedChild::spawn(&mut command)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket
            .metadata()
            .is_ok_and(|meta| meta.file_type().is_socket())
        {
            process::check_interrupt()?;
            if child.poll()?.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other(
                    "daemon failed to open its socket; see daemon.log",
                ));
            }
            thread::sleep(process::POLL);
        }
        Some(child)
    } else {
        None
    };
    evidence.launcher = vec![args.vm.clone()];
    let mut guest =
        ManagedChild::spawn(&mut logged_command(&args.vm, output, "console.log", &env)?)?;
    if matches!(phase, Phase::Write) {
        let daemon = daemon
            .as_mut()
            .ok_or_else(|| io::Error::other("recovery needs a daemon"))?;
        let deadline = Instant::now() + Duration::from_secs(args.timeout);
        let marker = results.join("write-flushed.json");
        while !marker.try_exists()? {
            process::check_interrupt()?;
            if guest.poll()?.is_some() || daemon.poll()?.is_some() {
                return Err(io::Error::other(
                    "guest or daemon exited before the recovery FLUSH marker",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "guest did not confirm recovery FLUSH before the deadline",
                ));
            }
            thread::sleep(process::POLL);
        }
        let marker: FlushMarker = read_json(&marker)?;
        marker.verify()?;
        read_json::<Fio>(&results.join("recovery.json"))?.verify("recovery-write", IO_BYTES, 0)?;
        // The guest waits after fio's final sync and blockdev --flushbufs. Kill
        // the backend first, so QEMU teardown cannot supply a shutdown flush.
        daemon.signal(libc::SIGKILL)?;
        let code = process::exit_code(daemon.wait(DAEMON_SHUTDOWN)?);
        evidence.daemon_exit = Some(code);
        if code != -libc::SIGKILL {
            return Err(io::Error::other("recovery did not kill the live daemon"));
        }
        evidence.flush_marker = Some(marker);
        evidence.written_bytes = Some(IO_BYTES);
        evidence.guest_exit = Some(process::exit_code(guest.stop()?));
        return Ok(());
    }
    let status = guest.wait(Duration::from_secs(args.timeout))?;
    evidence.guest_exit = Some(process::exit_code(status));
    if !status.success() {
        return Err(io::Error::other(format!(
            "QEMU exited with {status}; see console.log"
        )));
    }
    read_json::<GuestCompletion>(&results.join("completion.json"))?.verify()?;
    if read_only {
        read_json::<Fio>(&results.join("recovery.json"))?.verify("recovery-read", 0, IO_BYTES)?;
        evidence.verified_bytes = Some(IO_BYTES);
    } else {
        read_json::<Fio>(&results.join("fio.json"))?.verify("raw-smoke", IO_BYTES, IO_BYTES)?;
        evidence.verified_bytes = Some(IO_BYTES);
        if daemon.is_some() {
            read_json::<Fio>(&results.join("queue.json"))?.verify(
                "queue-smoke",
                IO_BYTES,
                IO_BYTES,
            )?;
            evidence.verified_bytes = Some(2 * IO_BYTES);
        }
    }
    if let Some(daemon) = &mut daemon {
        let start = Instant::now();
        let status = daemon.wait(DAEMON_SHUTDOWN);
        evidence.daemon_shutdown_seconds = Some(start.elapsed().as_secs_f64());
        let status = status?;
        evidence.daemon_exit = Some(process::exit_code(status));
        if !status.success() {
            return Err(io::Error::other("daemon failed; see daemon.log"));
        }
        let value: Value = read_json(&output.join("daemon.json"))?;
        evidence.daemon = Some(value.clone());
        serde_json::from_value::<DaemonReport>(value)?.verify(build.backend, read_only)?;
    }
    Ok(())
}

fn execute(args: &mut Args, summary: &mut Summary) -> io::Result<()> {
    File::options().read(true).write(true).open("/dev/kvm")?;
    let value: Value = read_json(&args.build_info)?;
    summary.build = Some(value.clone());
    let build: Build = serde_json::from_value(value)?;
    if (args.recovery || args.live_recovery) && build.backend != Backend::Staging {
        return Err(io::Error::other("recovery requires the staging runner"));
    }
    summary.artifact = format!(
        "development_{}_vm_{}",
        build.backend.name(),
        if args.live_recovery {
            "live_recovery"
        } else if args.recovery {
            "recovery"
        } else {
            "smoke"
        }
    );
    if build.system != format!("{}-linux", summary.host_machine) {
        return Err(io::Error::other(
            "guest architecture must match the KVM host",
        ));
    }
    args.vm = args.vm.canonicalize()?;
    fs::copy(&args.lock, args.output.join("flake.lock"))?;
    fs::copy(&args.build_info, args.output.join("build.json"))?;
    summary.invocation_worktree = Some(process::capture(
        &["git", "status", "--porcelain"],
        &std::env::current_dir()?,
    ));
    let disk_dir = args
        .disk_dir
        .as_ref()
        .unwrap_or(&args.output)
        .canonicalize()?;
    valid_qemu_path(&disk_dir)?;
    if !disk_dir.is_dir() {
        return Err(io::Error::other("disk directory must exist"));
    }
    let image = scratch_image(build.backend, &disk_dir)?;
    if build.backend != Backend::Staging {
        summary.raw_image = Some(image.clone());
    }
    summary.storage_image = Some(image.clone());
    summary.host_filesystem = Some(process::capture(
        &[
            "findmnt",
            "--json",
            "--target",
            disk_dir.to_str().expect("validated UTF-8"),
            "--output",
            "TARGET,SOURCE,FSTYPE,OPTIONS",
        ],
        &args.output,
    ));
    if args.live_recovery {
        live::execute(args, &build, &image, &mut summary.guest)?;
    } else if args.recovery {
        for (name, phase) in [("write", Phase::Write), ("read", Phase::Read)] {
            let output = args.output.join(name);
            fs::create_dir(&output)?;
            let evidence = summary.phases.entry(name.into()).or_default();
            execute_guest(args, &build, &output, &image, phase, evidence)?;
        }
        summary.guest.verified_bytes = summary.phases["read"].verified_bytes;
    } else {
        execute_guest(
            args,
            &build,
            &args.output,
            &image,
            Phase::Smoke,
            &mut summary.guest,
        )?;
    }
    Ok(())
}

pub fn run(mut args: Args) -> io::Result<()> {
    args.output = prepare_output(&args.output)?;
    let identity = host::identity()?;
    let mut summary = Summary {
        schema_version: 1,
        artifact: "development_raw_vm_smoke".into(),
        passed: false,
        paper_gate: None,
        started_at_utc: host::utc_now()?,
        host_machine: identity.machine,
        host_kernel: identity.kernel,
        host_cpu_affinity: host::cpu_affinity()?,
        disk_bytes: DISK_BYTES,
        build: None,
        invocation_worktree: None,
        raw_image: None,
        storage_image: None,
        host_filesystem: None,
        guest: PhaseEvidence {
            verified_bytes: Some(0),
            ..Default::default()
        },
        phases: BTreeMap::new(),
        wall_seconds_including_boot: 0.0,
        error: None,
    };
    let start = Instant::now();
    let result = execute(&mut args, &mut summary);
    summary.passed = result.is_ok();
    summary.error = result.as_ref().err().map(ToString::to_string);
    summary.wall_seconds_including_boot = start.elapsed().as_secs_f64();
    evidence::write_json(&args.output.join("summary.json"), &summary)?;
    println!(
        "{}",
        serde_json::json!({"passed":summary.passed,"results":args.output,"paper_gate":null})
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn existing_results_and_symlinks_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let output = prepare_output(&dir.path().join("results with spaces")).unwrap();
        let marker = output.join("summary.json");
        fs::write(&marker, "previous result").unwrap();
        assert!(prepare_output(&output).is_err());
        assert!(prepare_output(&marker).is_err());
        let alias = dir.path().join("alias");
        symlink(&output, &alias).unwrap();
        assert!(prepare_output(&alias).is_err());
        let dangling = dir.path().join("dangling");
        symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(prepare_output(&dangling).is_err());
        assert_eq!(fs::read_to_string(marker).unwrap(), "previous result");
    }

    #[test]
    fn qemu_separators_are_rejected_including_in_symlink_targets() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["a,b", "a\nb", "a\rb"] {
            let output = dir.path().join(name);
            assert!(prepare_output(&output).is_err());
            assert!(!output.exists());
        }
        let parent = dir.path().join("comma,parent");
        fs::create_dir(&parent).unwrap();
        let alias = dir.path().join("alias");
        symlink(&parent, &alias).unwrap();
        assert!(prepare_output(&alias.join("results")).is_err());
        assert!(!parent.join("results").exists());
    }

    fn fake_guest(script: &str) -> (io::Result<()>, PhaseEvidence) {
        let dir = tempfile::tempdir().unwrap();
        // Source changing fixture data through an existing executable. Creating
        // executable scripts beside parallel forks can race with inherited write
        // descriptors and produce ETXTBSY before close-on-exec takes effect.
        let vm = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/launcher.sh");
        fs::write(dir.path().join("fixture.sh"), script).unwrap();
        let args = Args {
            output: dir.path().into(),
            disk_dir: None,
            recovery: false,
            live_recovery: false,
            crash_at: "after-storage".into(),
            timeout: 1,
            vm,
            build_info: PathBuf::new(),
            lock: PathBuf::new(),
        };
        let build = Build {
            system: "test".into(),
            backend: Backend::Raw,
            daemon: None,
        };
        let mut evidence = PhaseEvidence::default();
        let result = execute_guest(
            &args,
            &build,
            dir.path(),
            &dir.path().join("scratch.raw"),
            Phase::Smoke,
            &mut evidence,
        );
        (result, evidence)
    }

    #[test]
    fn successful_launcher_still_requires_complete_guest_evidence() {
        let script = r#"
cat > "$CAS_RESULTS_DIR/completion.json" <<'EOF'
{"schema_version":1,"service_result":"success","exit_code":"exited","exit_status":"0"}
EOF
cat > "$CAS_RESULTS_DIR/fio.json" <<'EOF'
{"jobs":[{"jobname":"raw-smoke","error":0,"write":{"io_bytes":67108864},"read":{"io_bytes":67108864}}]}
EOF
"#;
        let (result, evidence) = fake_guest(script);
        result.unwrap();
        assert_eq!(evidence.guest_exit, Some(0));
        assert_eq!(evidence.verified_bytes, Some(IO_BYTES));
        assert!(fake_guest("exit 0").0.is_err());
        assert!(fake_guest(&script.replace("67108864", "4096")).0.is_err());
        assert!(
            fake_guest(&script.replace("\"error\":0", "\"error\":84"))
                .0
                .is_err()
        );
        assert!(fake_guest(&format!("{script}\nexit 7")).0.is_err());
    }

    #[test]
    fn stalled_guest_hits_its_deadline() {
        assert_eq!(
            fake_guest("exec sleep 30").0.unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
    }
}
