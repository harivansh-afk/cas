//! Replace only the daemon, retaining both original frontend processes.
use super::*;

#[derive(Debug, PartialEq, Serialize, Deserialize)]
struct Identity {
    pid: u32,
    start_ticks: u64,
}

impl Identity {
    fn observe(directory: &Path) -> io::Result<Self> {
        let qemu: serde_json::Value = evidence::read_json(&directory.join("qemu.json"))?;
        let pid = qemu["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| io::Error::other("QEMU PID is absent"))?;
        let start_ticks = process::proc_stat_field(pid, 22)?
            .parse()
            .map_err(|_| io::Error::other("QEMU process start time is absent"))?;
        Ok(Self { pid, start_ticks })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Restart {
    schema_version: u32,
    old_host_pid: u32,
    new_host_pid: u32,
    signal: i32,
    exit_code: i32,
    before: Vec<Identity>,
    after: Vec<Identity>,
}

pub(super) struct Boundary {
    pub deadline: Instant,
    pub cut: Option<Cut>,
}

pub(super) fn restart(
    args: &Args,
    build: &SharedBuild,
    output: &Path,
    sockets: &[PathBuf; 2],
    host: &mut ManagedChild,
    guests: &mut [(ManagedChild, PathBuf, bool)],
    boundary: Boundary,
) -> io::Result<PathBuf> {
    let mut armed = false;
    loop {
        process::check_interrupt()?;
        if host.poll()?.is_some() {
            return Err(io::Error::other("host exited before the crash boundary"));
        }
        let mut ready = true;
        for (guest, directory, _) in guests.iter_mut() {
            if guest.poll()?.is_some() {
                return Err(io::Error::other("guest exited before the crash boundary"));
            }
            ready &= directory.join("ready").is_file();
        }
        let reached = if let Some(cut) = boundary.cut {
            if ready && !armed {
                fs::write(
                    output.join("daemon/compaction-arm"),
                    "both applications fsynced",
                )?;
                for (_, directory, _) in guests.iter() {
                    fs::write(directory.join("continue"), "begin continuation")?;
                }
                armed = true;
            }
            let marker = output.join("daemon/compaction-pause.json");
            if marker.is_file() {
                verify_cut(&marker, cut)?;
                stopped(host.pid())?
            } else {
                false
            }
        } else {
            ready
        };
        if reached {
            break;
        }
        if Instant::now() >= boundary.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "application readiness expired",
            ));
        }
        thread::sleep(process::POLL);
    }
    let before = guests
        .iter()
        .map(|(_, path, _)| Identity::observe(path))
        .collect::<io::Result<Vec<_>>>()?;
    let old_host_pid = host.pid();
    host.signal(libc::SIGKILL)?;
    let exit_code = process::exit_code(host.wait(Duration::from_secs(5))?);
    evidence::write_json(
        &output.join("killed-host.json"),
        &serde_json::json!({
            "pid":old_host_pid, "signal":libc::SIGKILL, "exit_code":exit_code,
            "at_utc":crate::host::utc_now()?, "guest_identities":before, "stopped_at_cut":boundary.cut.is_some(),
        }),
    )?;
    if exit_code != -libc::SIGKILL {
        return Err(io::Error::other(
            "host did not exit by the requested SIGKILL",
        ));
    }
    // The process group has exited. These are the two private socket names
    // created for this phase; preserve every storage and report file.
    for socket in sockets {
        fs::remove_file(socket)?;
    }
    let replacement = output.join("replacement");
    fs::create_dir(&replacement)?;
    *host = start_host(args, build, &replacement, sockets, "retained", None)?;
    let after = guests
        .iter()
        .map(|(_, path, _)| Identity::observe(path))
        .collect::<io::Result<Vec<_>>>()?;
    let report = Restart {
        schema_version: 1,
        old_host_pid,
        new_host_pid: host.pid(),
        signal: libc::SIGKILL,
        exit_code,
        before,
        after,
    };
    evidence::write_json(&output.join("restart.json"), &report)?;
    if report.before != report.after {
        return Err(io::Error::other(
            "frontend process identity changed at restart",
        ));
    }
    for (_, directory, _) in guests {
        if !armed {
            fs::write(
                directory.join("continue"),
                "begin continuation after replacement",
            )?;
        }
        fs::write(directory.join("resume"), "retained sockets ready")?;
    }
    Ok(replacement)
}

pub(super) fn verify_restart(output: &Path, cut: Option<Cut>) -> io::Result<()> {
    if let Some(cut) = cut {
        verify_cut(&output.join("daemon/compaction-pause.json"), cut)?;
    }
    let report: Restart = evidence::read_json(&output.join("restart.json"))?;
    if report.schema_version != 1
        || report.signal != libc::SIGKILL
        || report.exit_code != -libc::SIGKILL
        || report.old_host_pid == report.new_host_pid
        || report.before.len() != 2
        || report.before != report.after
    {
        return Err(io::Error::other(
            "retained process replacement evidence differs",
        ));
    }
    let killed: serde_json::Value = evidence::read_json(&output.join("killed-host.json"))?;
    if killed["pid"] != report.old_host_pid
        || killed["exit_code"] != -libc::SIGKILL
        || killed["signal"] != libc::SIGKILL
        || killed["stopped_at_cut"] != cut.is_some()
    {
        return Err(io::Error::other(
            "kill evidence differs from retained replacement",
        ));
    }
    let command: serde_json::Value =
        evidence::read_json(&output.join("replacement/daemon-command.json"))?;
    if !command["argv"].as_array().is_some_and(|argv| {
        argv.windows(2)
            .any(|pair| pair[0] == "--mode" && pair[1] == "retained")
    }) {
        return Err(io::Error::other(
            "replacement did not request retained recovery",
        ));
    }
    for index in 0..2 {
        let directory = output.join(format!("guest-{index}"));
        let qemu: serde_json::Value = evidence::read_json(&directory.join("qemu.json"))?;
        if qemu["pid"] != report.before[index].pid
            || report.before[index].pid == report.before[1 - index].pid
            || report.before[index].start_ticks == 0
        {
            return Err(io::Error::other(
                "restart identities differ from original QEMU processes",
            ));
        }
        if !directory.join("ready").is_file()
            || !directory.join("continue").is_file()
            || !directory.join("updated").is_file()
            || !directory.join("resume").is_file()
        {
            return Err(io::Error::other(
                "guest readiness or release evidence is absent",
            ));
        }
    }
    Ok(())
}

fn stopped(pid: u32) -> io::Result<bool> {
    Ok(process::proc_stat_field(pid, 3)? == "T")
}

fn verify_cut(path: &Path, cut: Cut) -> io::Result<()> {
    let marker: serde_json::Value = evidence::read_json(path)?;
    let (p, e, d, through) = (
        evidence::u64_at(&marker, "/published")?,
        evidence::u64_at(&marker, "/durable")?,
        evidence::u64_at(&marker, "/manifest_durable")?,
        evidence::u64_at(&marker, "/selected_through")?,
    );
    if marker["schema_version"] != 1
        || marker["point"] != cut.name()
        || marker["armed"] != true
        || marker["image"] != IMAGES[0]
        || through == 0
        || through > e
        || e > p
    {
        return Err(io::Error::other(
            "compaction cut identity or prefix differs",
        ));
    }
    let before = matches!(cut, Cut::BeforeChunks | Cut::AfterChunks);
    if (before && (d >= through || evidence::u64_at(&marker, "/chunks")? == 0))
        || (!before && d != through)
    {
        return Err(io::Error::other(
            "manifest publication differs at selected cut",
        ));
    }
    match cut {
        Cut::AfterPunch
            if marker["reclamation"]["operation"] != "punch"
                || marker["reclamation"]["bytes"].as_u64().unwrap_or(0) == 0 =>
        {
            return Err(io::Error::other("cut lacks a completed payload punch"));
        }
        Cut::AfterUnlink if marker["reclamation"]["operation"] != "unlink" => {
            return Err(io::Error::other("cut lacks a completed segment unlink"));
        }
        _ => (),
    }
    Ok(())
}
