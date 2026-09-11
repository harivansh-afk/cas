//! One guest stays alive while a stopped backend is killed and replaced.
use super::*;
use crate::evidence::LIVE_BYTES;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pause {
    schema_version: u32,
    point: String,
    writes: u64,
    published: Option<u64>,
    durable: Option<u64>,
}

pub(super) fn execute(
    args: &Args,
    build: &Build,
    image: &Path,
    evidence: &mut PhaseEvidence,
) -> io::Result<()> {
    let output = &args.output;
    let results = output.join("guest");
    fs::create_dir(&results)?;
    let temporary = output.join("tmp");
    fs::create_dir(&temporary)?;
    fs::write(results.join("live-recovery"), build.backend.name())?;
    let directory = tempfile::Builder::new()
        .prefix("cas-live-")
        .tempdir_in("/tmp")?;
    let socket = directory.path().join("block.sock");
    let marker = output.join("pause.json");
    let program = build
        .daemon
        .as_ref()
        .ok_or_else(|| io::Error::other("missing daemon"))?;
    let mut env = environment(image, &results, &temporary);
    env.insert("CAS_VHOST_SOCKET".into(), socket.clone().into());
    env.insert("CAS_RECONNECT_MS".into(), "100".into());
    let spawn = |first: bool| -> io::Result<ManagedChild> {
        let mut command = logged_command(
            program,
            output,
            if first {
                "daemon-before.log"
            } else {
                "daemon-after.log"
            },
            &env,
        )?;
        command
            .arg("--socket")
            .arg(&socket)
            .arg("--image")
            .arg(image)
            .args(["--backend", build.backend.name(), "--restartable"])
            .arg("--report")
            .arg(output.join(if first {
                "daemon-before.json"
            } else {
                "daemon-after.json"
            }));
        if first {
            command
                .arg("--create-bytes")
                .arg(DISK_BYTES.to_string())
                .arg("--pause-at")
                .arg(&args.crash_at)
                .args(["--pause-after", "32"])
                .arg("--pause-marker")
                .arg(&marker);
        }
        let mut child = ManagedChild::spawn(&mut command)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.metadata().is_ok_and(|m| m.file_type().is_socket()) {
            process::check_interrupt()?;
            if child.poll()?.is_some() || Instant::now() >= deadline {
                return Err(io::Error::other("live daemon did not open its socket"));
            }
            thread::sleep(process::POLL);
        }
        Ok(child)
    };
    let mut daemon = spawn(true)?;
    evidence.launcher = vec![args.vm.clone()];
    let mut guest =
        ManagedChild::spawn(&mut logged_command(&args.vm, output, "console.log", &env)?)?;
    record_qemu(args, &mut guest, output)?;
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    while !marker.try_exists()? {
        process::check_interrupt()?;
        if guest.poll()?.is_some() || daemon.poll()?.is_some() || Instant::now() >= deadline {
            return Err(io::Error::other(
                "live recovery did not reach the requested write boundary",
            ));
        }
        thread::sleep(process::POLL);
    }
    let pause: Pause = read_json(&marker)?;
    if pause.schema_version != 1 || pause.point != args.crash_at || pause.writes != 32 {
        return Err(io::Error::other("invalid live recovery pause marker"));
    }
    if build.backend == Backend::LocalAsync {
        let (Some(p), Some(e)) = (pause.published, pause.durable) else {
            return Err(io::Error::other("missing pre-crash P/E evidence"));
        };
        if e > p || (args.crash_at != "before-submit" && (p < 32 || p == e)) {
            return Err(io::Error::other(
                "concurrent crash did not exercise a published unsynced prefix",
            ));
        }
    }
    if guest.poll()?.is_some() {
        return Err(io::Error::other("guest exited before backend kill"));
    }
    daemon.signal(libc::SIGKILL)?;
    let killed = process::exit_code(daemon.wait(DAEMON_SHUTDOWN)?);
    if killed != -libc::SIGKILL {
        return Err(io::Error::other("live recovery did not kill its daemon"));
    }
    // This socket lives in our private directory and its owner has been reaped.
    fs::remove_file(&socket)?;
    daemon = spawn(false)?;
    if guest.poll()?.is_some() {
        return Err(io::Error::other("guest exited during backend replacement"));
    }
    let status = guest.wait(deadline.saturating_duration_since(Instant::now()))?;
    evidence.guest_exit = Some(process::exit_code(status));
    if !status.success() {
        return Err(io::Error::other("live guest failed; see console.log"));
    }
    read_json::<GuestCompletion>(&results.join("completion.json"))?.verify()?;
    read_json::<Fio>(&results.join("live.json"))?.verify_jobs(
        "live-recovery",
        LIVE_BYTES,
        LIVE_BYTES,
        if build.backend == Backend::LocalAsync {
            4
        } else {
            1
        },
    )?;
    let before = fs::read_to_string(results.join("boot-before.txt"))?;
    let after = fs::read_to_string(results.join("boot-after.txt"))?;
    if before.trim().is_empty() || before != after {
        return Err(io::Error::other(
            "guest boot identity changed during live recovery",
        ));
    }
    let status = daemon.wait(DAEMON_SHUTDOWN)?;
    evidence.daemon_exit = Some(process::exit_code(status));
    if !status.success() {
        return Err(io::Error::other(
            "restarted daemon failed; see daemon-after.log",
        ));
    }
    let value: Value = read_json(&output.join("daemon-after.json"))?;
    evidence.daemon = Some(value.clone());
    serde_json::from_value::<DaemonReport>(value)?.verify_live(build.backend, &args.crash_at)?;
    evidence.verified_bytes = Some(LIVE_BYTES);
    evidence.written_bytes = Some(LIVE_BYTES);
    evidence.live_recovery = Some(serde_json::json!({
        "crash_at": args.crash_at, "write_number": 32, "killed_daemon_exit": killed,
        "guest_boot_id": before.trim(), "guest_reboots": 0,
        "published_before_kill": pause.published, "durable_before_kill": pause.durable,
        "mode": if build.backend == Backend::LocalAsync { "concurrent_retained_inflight" } else { "serial_write_through" },
    }));
    Ok(())
}
