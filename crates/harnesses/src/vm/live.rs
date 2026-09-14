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

pub(super) fn validate_options(args: &Args, backend: Backend) -> io::Result<()> {
    let reference_point = matches!(
        args.crash_at,
        CrashPoint::BeforeSubmit
            | CrashPoint::AfterStorage
            | CrashPoint::AfterStatus
            | CrashPoint::AfterUsed
    );
    if args.live_recovery && !reference_point && backend != Backend::LocalAsync {
        return Err(io::Error::other("this crash point requires local-async"));
    }
    if args.replay_crash_at.is_some()
        && (backend != Backend::LocalAsync || args.crash_at != CrashPoint::BeforeSubmit)
    {
        return Err(io::Error::other(
            "interrupted replay requires local-async and --crash-at before-submit",
        ));
    }
    Ok(())
}

fn wait_pause(
    marker: &Path,
    expected_point: &str,
    writes: u64,
    guest: &mut ManagedChild,
    daemon: &mut ManagedChild,
    deadline: Instant,
) -> io::Result<Pause> {
    while !marker.try_exists()? {
        process::check_interrupt()?;
        if guest.poll()?.is_some() || daemon.poll()?.is_some() || Instant::now() >= deadline {
            return Err(io::Error::other(
                "live recovery did not reach its requested boundary",
            ));
        }
        thread::sleep(process::POLL);
    }
    let pause: Pause = read_json(marker)?;
    if pause.schema_version != 1 || pause.point != expected_point || pause.writes != writes {
        return Err(io::Error::other("invalid live recovery pause marker"));
    }
    Ok(pause)
}

fn kill(daemon: &mut ManagedChild, guest: &mut ManagedChild, socket: &Path) -> io::Result<i32> {
    if guest.poll()?.is_some() {
        return Err(io::Error::other("guest exited before backend kill"));
    }
    daemon.signal(libc::SIGKILL)?;
    let killed = process::exit_code(daemon.wait(DAEMON_SHUTDOWN)?);
    if killed != -libc::SIGKILL {
        return Err(io::Error::other("live recovery did not kill its daemon"));
    }
    // The private socket's owning process has been reaped.
    fs::remove_file(socket)?;
    Ok(killed)
}

pub(super) fn execute(
    args: &Args,
    build: &VmBuild,
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
    let deadline = Instant::now() + Duration::from_secs(args.timeout);
    let spawn =
        |name: &str, create: bool, pause: Option<(&str, u64, &Path)>| -> io::Result<ManagedChild> {
            let mut command = logged_command(program, output, &format!("{name}.log"), &env)?;
            command
                .arg("--socket")
                .arg(&socket)
                .arg("--image")
                .arg(image)
                .args(["--backend", build.backend.name(), "--restartable"])
                .arg("--report")
                .arg(output.join(format!("{name}.json")));
            if create {
                command.arg("--create-bytes").arg(DISK_BYTES.to_string());
            }
            if let Some((point, after, marker)) = pause {
                command
                    .arg("--pause-at")
                    .arg(point)
                    .arg("--pause-after")
                    .arg(after.to_string())
                    .arg("--pause-marker")
                    .arg(marker);
            }
            let mut child = ManagedChild::spawn(&mut command)?;
            let startup = deadline.min(Instant::now() + Duration::from_secs(10));
            while !socket.metadata().is_ok_and(|m| m.file_type().is_socket()) {
                process::check_interrupt()?;
                if child.poll()?.is_some() || Instant::now() >= startup {
                    return Err(io::Error::other("live daemon did not open its socket"));
                }
                thread::sleep(process::POLL);
            }
            Ok(child)
        };
    let mut daemon = spawn(
        "daemon-before",
        true,
        Some((args.crash_at.name(), 32, &marker)),
    )?;
    evidence.launcher = vec![args.vm.clone()];
    let mut guest =
        ManagedChild::spawn(&mut logged_command(&args.vm, output, "console.log", &env)?)?;
    record_qemu(args, &mut guest, output)?;
    let pause = wait_pause(
        &marker,
        args.crash_at.name(),
        32,
        &mut guest,
        &mut daemon,
        deadline,
    )?;
    if build.backend == Backend::LocalAsync {
        let (Some(p), Some(e)) = (pause.published, pause.durable) else {
            return Err(io::Error::other("missing pre-crash P/E evidence"));
        };
        let published_cut = matches!(
            args.crash_at,
            CrashPoint::AfterStorage
                | CrashPoint::AfterStatus
                | CrashPoint::AfterUsed
                | CrashPoint::BeforeSync
                | CrashPoint::AfterSync
        );
        if e > p || (published_cut && (p < 32 || p == e)) {
            return Err(io::Error::other(
                "concurrent crash did not exercise a published unsynced prefix",
            ));
        }
    }
    let killed = kill(&mut daemon, &mut guest, &socket)?;
    let mut interrupted = Vec::new();
    let mut previous_p = pause.published.unwrap_or(0);
    if let Some(point) = args.replay_crash_at {
        for attempt in 1..=args.replay_restarts {
            let marker = output.join(format!("pause-replay-{attempt}.json"));
            let after = if point == ReplayPoint::AfterReplayAppend {
                1
            } else {
                32
            };
            daemon = spawn(
                &format!("daemon-replay-{attempt}"),
                false,
                Some((point.name(), after, &marker)),
            )?;
            let replay_pause = wait_pause(
                &marker,
                point.name(),
                after,
                &mut guest,
                &mut daemon,
                deadline,
            )?;
            let (Some(p), Some(e)) = (replay_pause.published, replay_pause.durable) else {
                return Err(io::Error::other("missing interrupted-replay P/E evidence"));
            };
            if p < previous_p
                || e > p
                || (point == ReplayPoint::AfterReplayAppend && p == previous_p)
                || (point == ReplayPoint::AfterRecoveryFence && e != p)
            {
                return Err(io::Error::other(
                    "interrupted replay violated its prefix boundary",
                ));
            }
            let killed = kill(&mut daemon, &mut guest, &socket)?;
            interrupted.push(serde_json::json!({"attempt":attempt,"point":point,"killed_daemon_exit":killed,"published":p,"durable":e}));
            previous_p = p;
        }
    }
    daemon = spawn("daemon-after", false, None)?;
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
    serde_json::from_value::<DaemonReport>(value)?.verify_live(build.backend, args.crash_at)?;
    evidence.verified_bytes = Some(LIVE_BYTES);
    evidence.written_bytes = Some(LIVE_BYTES);
    evidence.live_recovery = Some(serde_json::json!({
        "crash_at": args.crash_at, "write_number": 32, "killed_daemon_exit": killed,
        "guest_boot_id": before.trim(), "guest_reboots": 0,
        "published_before_kill": pause.published, "durable_before_kill": pause.durable,
        "interrupted_replays": interrupted,
        "mode": if build.backend == Backend::LocalAsync { "concurrent_retained_inflight" } else { "serial_write_through" },
    }));
    Ok(())
}
