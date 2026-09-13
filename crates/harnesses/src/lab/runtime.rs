use super::*;
use std::{thread, time::Instant};

pub(super) fn supervise(directory: &Path, run: &Path) -> io::Result<()> {
    let config: Config = evidence::read_json(&directory.join("config.json"))?;
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    if active.run != run {
        return Err(io::Error::other("active run changed before startup"));
    }
    let started = crate::host::utc_now()?;
    let result = (|| {
        let mut command = Process::new(&config.build.vm);
        command
            .current_dir(run.join("tmp"))
            .env("TMPDIR", run.join("tmp"))
            .env("CAS_XFS_IMAGE", directory.join("disk.raw"))
            .env("CAS_RESULTS_DIR", run)
            .env("CAS_SSH_PORT", active.port.to_string());
        let mut vm = spawn(&mut command, &run.join("console.log"))?;
        crate::qemu::record(&mut vm, run)?;
        loop {
            if let Some(exit) = vm.poll()? {
                if !exit.success() {
                    return Err(io::Error::other(format!("outer VM exited with {exit}")));
                }
                let host: serde_json::Value = evidence::read_json(&run.join("host-outcome.json"))?;
                if host["success"] != true {
                    return Err(io::Error::other("storage/guest shutdown failed"));
                }
                publish(&run.join("spark-memory.json"), &samples::outer()?)?;
                break;
            }
            if run.join("ready.json").exists() && !run.join("ssh_config").exists() {
                client::configure_ssh(directory, run, &config, active.port)?;
            }
            if client::available(directory)? < 25 * GIB {
                fs::write(run.join("stop"), b"host disk headroom exhausted\n")?;
                return Err(io::Error::other(
                    "lab aborted to preserve 25 GiB of host disk headroom",
                ));
            }
            publish(&run.join("spark-memory.json"), &samples::outer()?)?;
            thread::sleep(Duration::from_secs(1));
        }
        Ok(())
    })();
    publish(
        &run.join("outcome.json"),
        &serde_json::json!({"success":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string),"started":started,"finished":crate::host::utc_now()?}),
    )?;
    result
}

#[derive(Deserialize)]
struct HostBuild {
    host: PathBuf,
    daemon: PathBuf,
    guest: PathBuf,
    raw_guest: PathBuf,
    daemon_guest: PathBuf,
}

pub(super) fn host(build: &Path) -> io::Result<()> {
    let build: HostBuild = evidence::read_json(build)?;
    let root = Path::new("/results");
    let config: Config = evidence::read_json(&root.join("config.json"))?;
    let result = serve(root, &config, &build);
    publish(
        &root.join("host-outcome.json"),
        &serde_json::json!({"success":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}),
    )?;
    result
}
fn serve(output: &Path, config: &Config, build: &HostBuild) -> io::Result<()> {
    let storage = Path::new("/fixture/store");
    let sockets = tempfile::Builder::new()
        .prefix("cas-lab-")
        .tempdir_in("/run")?;
    let paths: Vec<_> = (0..config.count)
        .map(|i| sockets.path().join(format!("{i}.sock")))
        .collect();
    let first = !Path::new("/fixture/initialized").exists();
    if first {
        if matches!(config.backend, Backend::Cas) {
            fs::create_dir(storage)?;
            let mut init = Process::new(&build.host);
            init.arg("init").arg("--root").arg(storage).args([
                "--store",
                STORE,
                "--segment-bytes",
                &SEGMENT_BYTES.to_string(),
            ]);
            for i in 0..config.count {
                init.args(["--image", &format!("{}={IMAGE_BYTES}", image(i))]);
            }
            if !spawn(&mut init, &output.join("initialize.log"))?
                .wait(Duration::from_secs(65))?
                .success()
            {
                return Err(io::Error::other("storage initialization failed"));
            }
        } else {
            for i in 0..config.count {
                File::options()
                    .write(true)
                    .create_new(true)
                    .open(format!("/fixture/raw-{i}"))?
                    .set_len(IMAGE_BYTES)?;
            }
        }
        File::create("/fixture/initialized")?.sync_all()?;
        File::open("/fixture")?.sync_all()?;
    }
    let mut daemons = Vec::new();
    match config.backend {
        Backend::Cas => {
            let reports = output.join("daemon");
            fs::create_dir(&reports)?;
            let mut command = Process::new(&build.host);
            command
                .args([
                    "--root",
                    "/fixture/store",
                    "--store",
                    STORE,
                    "--mode",
                    "cold",
                    "--segment-bytes",
                    &SEGMENT_BYTES.to_string(),
                    "--telemetry",
                    "--telemetry-rotate",
                ])
                .arg("--reports")
                .arg(&reports);
            for (i, socket) in paths.iter().enumerate() {
                command.args([
                    "--image",
                    &format!("{}={}", image(i as u8), socket.display()),
                ]);
            }
            daemons.push(spawn(&mut command, &output.join("host.log"))?);
        }
        Backend::Daemon => {
            for (i, socket) in paths.iter().enumerate() {
                let mut command = Process::new(&build.daemon);
                command
                    .arg("--socket")
                    .arg(socket)
                    .args(["--image", &format!("/fixture/raw-{i}")])
                    .arg("--report")
                    .arg(output.join(format!("daemon-{i}.json")));
                daemons.push(spawn(
                    &mut command,
                    &output.join(format!("daemon-{i}.log")),
                )?);
            }
        }
        Backend::Raw => (),
    }
    let started = Instant::now();
    while !matches!(config.backend, Backend::Raw) && !paths.iter().all(|p| p.exists()) {
        for daemon in &mut daemons {
            if daemon.poll()?.is_some() {
                return Err(io::Error::other("storage exited before publishing sockets"));
            }
        }
        if started.elapsed() > Duration::from_secs(60) {
            return Err(io::Error::other("storage startup timed out"));
        }
        thread::sleep(Duration::from_millis(25));
    }
    let mut guests = Vec::new();
    for (i, socket) in paths.iter().enumerate() {
        let directory = output.join(format!("guest-{}", i + 1));
        fs::create_dir(&directory)?;
        fs::create_dir(directory.join("tmp"))?;
        fs::copy(
            output.join("authorized_keys"),
            directory.join("authorized_keys"),
        )?;
        // A missing filesystem marker never authorizes reformatting existing bytes.
        if first {
            fs::write(directory.join("format"), b"new disk\n")?;
        }
        let mut command = Process::new(match config.backend {
            Backend::Raw => &build.raw_guest,
            Backend::Daemon => &build.daemon_guest,
            Backend::Cas => &build.guest,
        });
        command
            .current_dir(directory.join("tmp"))
            .env("TMPDIR", directory.join("tmp"))
            .env("CAS_RESULTS_DIR", &directory)
            .env("CAS_VHOST_SOCKET", socket)
            .env("CAS_RAW_IMAGE", format!("/fixture/raw-{i}"))
            .env("CAS_SSH_PORT", (23480 + i).to_string());
        let mut guest = spawn(&mut command, &directory.join("console.log"))?;
        crate::qemu::record(&mut guest, &directory)?;
        guests.push(guest);
    }
    let mut samples = samples::Samples::new(output)?;
    let mut ready = false;
    let mut shutdown = None;
    loop {
        for daemon in &mut daemons {
            if let Some(status) = daemon.poll()?
                && (shutdown.is_none() || !status.success())
            {
                return Err(io::Error::other(format!("storage exited: {status}")));
            }
        }
        let mut complete = true;
        for guest in &mut guests {
            match guest.poll()? {
                Some(status) if !status.success() => {
                    return Err(io::Error::other(format!("guest exited: {status}")));
                }
                Some(_) if shutdown.is_none() => {
                    return Err(io::Error::other(
                        "guest stopped; stop/start the lab to reconnect its fixed membership",
                    ));
                }
                Some(_) => (),
                None => complete = false,
            }
        }
        if !ready
            && (1..=config.count).all(|i| {
                output.join(format!("guest-{i}/ready")).exists()
                    && output.join(format!("guest-{i}/host-key.pub")).exists()
            })
        {
            publish(
                &output.join("ready.json"),
                &serde_json::json!({"guests":config.count,"backend":config.backend.name(),"ready_at":crate::host::utc_now()?}),
            )?;
            ready = true;
        }
        if output.join("stop").exists() && shutdown.is_none() {
            for i in 1..=config.count {
                fs::write(output.join(format!("guest-{i}/stop")), b"shutdown\n")?;
            }
            shutdown = Some(Instant::now());
        }
        if shutdown.is_some() && complete {
            break;
        }
        if shutdown.is_some_and(|time| time.elapsed() > Duration::from_secs(65))
            || (!ready && started.elapsed() > STARTUP)
        {
            return Err(io::Error::other("guest readiness/shutdown timed out"));
        }
        samples.tick(&daemons, &guests)?;
        thread::sleep(Duration::from_millis(500));
    }
    for daemon in &mut daemons {
        if !daemon.wait(Duration::from_secs(30))?.success() {
            return Err(io::Error::other("storage shutdown failed"));
        }
    }
    Ok(())
}
