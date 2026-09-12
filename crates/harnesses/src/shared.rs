//! One host process and two ordinary filesystem guests on the XFS fixture.
use crate::{
    evidence,
    process::{self, ManagedChild},
    qemu,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::{Duration, Instant},
};

const STORE: &str = "01010101010101010101010101010101";
const IMAGES: [&str; 2] = [
    "02020202020202020202020202020202",
    "03030303030303030303030303030303",
];
const IMAGE_BYTES: u64 = 512 * 1024 * 1024;
const SEGMENT_BYTES: u64 = 2 * 1024 * 1024;
const PHASE_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(clap::Args)]
pub struct Args {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(long)]
    build_info: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub host: PathBuf,
    pub guest: PathBuf,
    pub qemu: PathBuf,
    pub kernel: String,
    pub guest_ram_bytes: u64,
}

fn checked(command: &mut Command, output: &Path, timeout: Duration) -> io::Result<()> {
    let result = process::run_logged(command, output, timeout)?;
    if result.exit_code != Some(0) || result.error.is_some() {
        return Err(io::Error::other(format!(
            "command failed: {}",
            output.display()
        )));
    }
    Ok(())
}

fn spawn(command: &mut Command, output: &Path) -> io::Result<ManagedChild> {
    let log = File::options().write(true).create_new(true).open(output)?;
    command.stdout(log.try_clone()?).stderr(log);
    ManagedChild::spawn(command)
}

fn phase(args: &Args, build: &Build, name: &str) -> io::Result<()> {
    let output = args.output.join(name);
    fs::create_dir(&output)?;
    let reports = output.join("daemon");
    fs::create_dir(&reports)?;
    let sockets = tempfile::Builder::new()
        .prefix("cas-shared-")
        .tempdir_in("/run")?;
    let paths = [sockets.path().join("0.sock"), sockets.path().join("1.sock")];
    let mut command = Command::new(&build.host);
    command
        .arg("--root")
        .arg(&args.root)
        .args(["--store", STORE, "--mode", "cold", "--segment-bytes"])
        .arg(SEGMENT_BYTES.to_string())
        .arg("--reports")
        .arg(&reports);
    for (id, socket) in IMAGES.iter().zip(&paths) {
        command
            .arg("--image")
            .arg(format!("{id}={}", socket.display()));
    }
    let mut host = spawn(&mut command, &output.join("daemon.log"))?;
    evidence::write_json(
        &output.join("daemon-command.json"),
        &serde_json::json!({
            "pid":host.pid(), "executable":build.host, "argv":command.get_args().collect::<Vec<_>>()
        }),
    )?;
    let startup = Instant::now() + Duration::from_secs(60);
    while !paths.iter().all(|path| path.exists()) {
        process::check_interrupt()?;
        if host.poll()?.is_some() || Instant::now() >= startup {
            return Err(io::Error::other("shared host did not publish both sockets"));
        }
        thread::sleep(process::POLL);
    }
    let mut guests = Vec::new();
    for (index, socket) in paths.iter().enumerate() {
        let directory = output.join(format!("guest-{index}"));
        fs::create_dir(&directory)?;
        let temporary = directory.join("tmp");
        fs::create_dir(&temporary)?;
        fs::write(directory.join("phase"), name)?;
        fs::write(directory.join("image"), index.to_string())?;
        let mut command = Command::new(&build.guest);
        command
            .env("CAS_RESULTS_DIR", &directory)
            .env("CAS_VHOST_SOCKET", socket)
            .env("TMPDIR", &temporary)
            .env("USE_TMPDIR", "1");
        let mut guest = spawn(&mut command, &directory.join("console.log"))?;
        qemu::record(&mut guest, &directory)?;
        guests.push((guest, directory, false));
    }
    let deadline = Instant::now() + PHASE_TIMEOUT;
    loop {
        process::check_interrupt()?;
        let mut complete = true;
        for (guest, directory, reported) in &mut guests {
            if *reported {
                continue;
            }
            match guest.poll()? {
                Some(status) => {
                    evidence::write_json(
                        &directory.join("exit.json"),
                        &serde_json::json!({"exit_code":process::exit_code(status)}),
                    )?;
                    *reported = true;
                    if !status.success() {
                        return Err(io::Error::other("shared filesystem guest failed"));
                    }
                }
                None => complete = false,
            }
        }
        if complete {
            break;
        }
        if let Some(status) = host.poll()?
            && !status.success()
        {
            return Err(io::Error::other(
                "shared host failed while guests were running",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "shared filesystem phase expired",
            ));
        }
        thread::sleep(process::POLL);
    }
    let status = host.wait(Duration::from_secs(35))?;
    evidence::write_json(
        &output.join("daemon-exit.json"),
        &serde_json::json!({"exit_code":process::exit_code(status)}),
    )?;
    if !status.success() {
        return Err(io::Error::other("shared host shutdown failed"));
    }
    verify_phase(&output, name, build)
}

fn verify_phase(output: &Path, phase: &str, build: &Build) -> io::Result<()> {
    let host: serde_json::Value = evidence::read_json(&output.join("daemon/host.json"))?;
    // The executable's report carries actual shared owner shutdown and budgets.
    if host["services_ok"] != true
        || !host["shutdown_error"].is_null()
        || !host["host"]["failure"].is_null()
        || host["metadata"]["current"]["bytes"] != 0
    {
        return Err(io::Error::other("shared host report failed"));
    }
    let exit: serde_json::Value = evidence::read_json(&output.join("daemon-exit.json"))?;
    if exit["exit_code"] != 0 {
        return Err(io::Error::other("daemon exit evidence failed"));
    }
    for (index, id) in IMAGES.iter().enumerate() {
        let directory = output.join(format!("guest-{index}"));
        evidence::read_json::<evidence::GuestCompletion>(&directory.join("completion.json"))?
            .verify()?;
        let exit: serde_json::Value = evidence::read_json(&directory.join("exit.json"))?;
        let mount: serde_json::Value = evidence::read_json(&directory.join("mount.json"))?;
        if exit["exit_code"] != 0
            || mount["filesystems"][0]["fstype"] != "ext4"
            || !mount["filesystems"][0]["options"]
                .as_str()
                .is_some_and(|options| options.split(',').any(|option| option == "data=ordered"))
            || !fs::read_to_string(directory.join("kernel.log"))?.contains(&build.kernel)
        {
            return Err(io::Error::other(
                "guest exit, filesystem or kernel evidence differs",
            ));
        }
        let actual: serde_json::Value =
            evidence::read_json(&directory.join("workload/filesystem.json"))?;
        if actual["passed"] != true
            || actual["phase"] != phase
            || actual["image"] != index
            || actual["file_bytes"] != 768 * 1024
            || actual["sqlite_rows"] != 1024
            || actual["sqlite_integrity"] != "ok"
            || actual["buffered"] != true
        {
            return Err(io::Error::other(
                "filesystem report differs from required workload",
            ));
        }
        let qemu: serde_json::Value = evidence::read_json(&directory.join("qemu.json"))?;
        let argv = qemu["argv"]
            .as_array()
            .ok_or_else(|| io::Error::other("missing guest QEMU argv"))?;
        if argv.first().and_then(|value| value.as_str()) != build.qemu.to_str()
            || !argv.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|arg| arg.split(',').any(|field| field == "accel=tcg"))
            })
        {
            return Err(io::Error::other(
                "shared guest must use its explicit TCG build",
            ));
        }
        let option = |name: &str| {
            argv.windows(2)
                .find(|pair| pair[0] == name)
                .map(|pair| &pair[1])
        };
        if option("-m")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<u64>().ok())
            != Some(build.guest_ram_bytes / (1024 * 1024))
            || option("-nic").and_then(|v| v.as_str()) != Some("none")
            || !argv.iter().any(|arg| {
                arg.as_str()
                    .is_some_and(|arg| arg.starts_with("vhost-user-blk-pci,"))
            })
        {
            return Err(io::Error::other(
                "guest memory, networking or device configuration differs",
            ));
        }
        let backend: serde_json::Value =
            evidence::read_json(&output.join("daemon").join(format!("{id}.json")))?;
        if phase == "write"
            && (backend["zeroes"].as_u64().unwrap_or(0) == 0
                || backend["flushes"].as_u64().unwrap_or(0) == 0)
        {
            return Err(io::Error::other("guest did not exercise trim and FLUSH"));
        }
        if backend["connection_ok"] != true
            || !backend["fatal_error"].is_null()
            || backend["errors"] != 0
            || backend["pending_at_disconnect"] != 0
        {
            return Err(io::Error::other("image backend report failed"));
        }
    }
    Ok(())
}

pub fn run(args: Args) -> io::Result<()> {
    fs::create_dir(&args.output)?;
    let build: Build = evidence::read_json(&args.build_info)?;
    evidence::write_json(&args.output.join("build.json"), &build)?;
    let started = crate::host::utc_now()?;
    let result = (|| {
        fs::create_dir(&args.root)?;
        let mut command = Command::new(&build.host);
        command
            .arg("init")
            .arg("--root")
            .arg(&args.root)
            .args(["--store", STORE, "--segment-bytes"])
            .arg(SEGMENT_BYTES.to_string());
        for id in IMAGES {
            command.arg("--image").arg(format!("{id}={IMAGE_BYTES}"));
        }
        checked(
            &mut command,
            &args.output.join("initialize"),
            Duration::from_secs(65),
        )?;
        phase(&args, &build, "write")?;
        phase(&args, &build, "verify")?;
        Ok(())
    })();
    evidence::write_json(
        &args.output.join("shared.json"),
        &serde_json::json!({
            "schema_version":1,"passed":result.is_ok(),"started_at_utc":started,"ended_at_utc":crate::host::utc_now()?,
            "error":result.as_ref().err().map(ToString::to_string),"images":2,"image_bytes":IMAGE_BYTES,
            "segment_bytes":SEGMENT_BYTES,"guest_ram_bytes_each":build.guest_ram_bytes,"inner_acceleration":"tcg",
            "paper_gates":[],"checkpoint_complete":false,
        }),
    )?;
    result
}

pub fn verify(output: &Path) -> io::Result<()> {
    let report: serde_json::Value = evidence::read_json(&output.join("shared.json"))?;
    if report["passed"] != true {
        return Err(io::Error::other("shared fixture did not pass"));
    }
    let build: Build = evidence::read_json(&output.join("build.json"))?;
    for phase in ["write", "verify"] {
        verify_phase(&output.join(phase), phase, &build)?;
    }
    for index in 0..2 {
        let get = |phase| {
            evidence::read_json::<serde_json::Value>(
                &output
                    .join(phase)
                    .join(format!("guest-{index}/workload/filesystem.json")),
            )
        };
        if get("write")?["file_blake3"] != get("verify")?["file_blake3"] {
            return Err(io::Error::other("file checksum changed after fresh boot"));
        }
    }
    Ok(())
}
