use super::*;
use crate::process::{self, ManagedChild};
use std::{
    fs::File,
    io::Write,
    net::TcpListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};

fn spawn(command: &mut Command, output: &Path, name: &str) -> io::Result<ManagedChild> {
    evidence::write_json(
        &output.join(format!("{name}-command.json")),
        &serde_json::json!({
        "program":command.get_program().to_string_lossy(),
        "args":command.get_args().map(|arg| arg.to_string_lossy()).collect::<Vec<_>>()
        }),
    )?;
    process::spawn_logged(command, &output.join(format!("{name}.log")))
}

fn require_alive(children: &mut [ManagedChild]) -> io::Result<()> {
    for child in children {
        if let Some(status) = child.poll()? {
            return Err(io::Error::other(format!(
                "child {} exited early: {status}",
                child.pid()
            )));
        }
    }
    Ok(())
}

fn checked(command: &mut Command, output: &Path, name: &str) -> io::Result<()> {
    let status = spawn(command, output, name)?.wait(Duration::from_secs(60))?;
    evidence::require(status.success(), &format!("{name} failed: {status}"))
}

fn storage(args: &Args, build: &Build, sockets: &[PathBuf]) -> io::Result<Vec<ManagedChild>> {
    let mut children = Vec::new();
    match args.backend {
        Backend::Cas => {
            let root = args.storage.join("store");
            fs::create_dir(&root)?;
            let mut init = Command::new(&build.host);
            init.args(["init", "--root"])
                .arg(&root)
                .args(["--store", STORE])
                .arg("--segment-bytes")
                .arg(args.segment_bytes.to_string())
                .arg("--staging-bytes")
                .arg(args.staging_bytes.to_string());
            for i in 0..args.guests {
                init.args(["--image", &format!("{}={}", image(i), args.image_bytes)]);
            }
            checked(&mut init, &args.output, "initialize")?;
            let reports = args.output.join("daemon");
            fs::create_dir(&reports)?;
            let mut command = Command::new(&build.host);
            command
                .arg("--root")
                .arg(&root)
                .args(["--store", STORE, "--mode", "cold"])
                .arg("--segment-bytes")
                .arg(args.segment_bytes.to_string())
                .arg("--staging-bytes")
                .arg(args.staging_bytes.to_string())
                .arg("--cache-bytes")
                .arg(args.cache_bytes.to_string())
                .arg("--reports")
                .arg(&reports)
                .arg("--telemetry");
            for (i, socket) in sockets.iter().enumerate() {
                command.args([
                    "--image",
                    &format!("{}={}", image(i as u8), socket.display()),
                ]);
            }
            children.push(spawn(&mut command, &args.output, "storage")?);
        }
        Backend::Raw | Backend::Daemon => {
            for (i, socket) in sockets.iter().enumerate() {
                let path = args.storage.join(format!("raw-{i}.img"));
                File::options()
                    .write(true)
                    .create_new(true)
                    .open(&path)?
                    .set_len(args.image_bytes)?;
                if matches!(args.backend, Backend::Daemon) {
                    let mut command = Command::new(&build.daemon);
                    command
                        .arg("--socket")
                        .arg(socket)
                        .arg("--image")
                        .arg(path)
                        .arg("--report")
                        .arg(args.output.join(format!("storage-{i}-report.json")));
                    children.push(spawn(&mut command, &args.output, &format!("storage-{i}"))?);
                }
            }
        }
    }
    let started = Instant::now();
    while !matches!(args.backend, Backend::Raw) && !sockets.iter().all(|p| p.exists()) {
        process::check_interrupt()?;
        require_alive(&mut children)?;
        evidence::require(
            started.elapsed() < Duration::from_secs(30),
            "storage socket startup timed out",
        )?;
        thread::sleep(Duration::from_millis(25));
    }
    Ok(children)
}

struct Sampler {
    file: File,
    processes: Vec<(&'static str, u32)>,
    block_stat: PathBuf,
    cgroup: PathBuf,
    output: PathBuf,
    started: Instant,
}
impl Sampler {
    fn new(args: &Args, device: &Path, processes: Vec<(&'static str, u32)>) -> io::Result<Self> {
        let dev = device.metadata()?.rdev();
        let group = fs::read_to_string("/proc/self/cgroup")?;
        let group = group
            .lines()
            .find_map(|line| line.strip_prefix("0::/"))
            .ok_or_else(|| io::Error::other("cgroup v2 required"))?;
        Ok(Self {
            file: File::options()
                .write(true)
                .create_new(true)
                .open(args.output.join("samples.jsonl"))?,
            processes,
            block_stat: PathBuf::from(format!(
                "/sys/dev/block/{}:{}/stat",
                libc::major(dev),
                libc::minor(dev)
            )),
            cgroup: Path::new("/sys/fs/cgroup").join(group),
            output: args.output.clone(),
            started: Instant::now(),
        })
    }
    fn tick(&mut self) -> io::Result<()> {
        let processes: Vec<_> = self
            .processes
            .iter()
            .map(|(role, pid)| {
                let root = PathBuf::from(format!("/proc/{pid}"));
                serde_json::json!({"role":role, "pid":pid,
                "smaps_rollup":fs::read_to_string(root.join("smaps_rollup")).ok(),
                "stat":fs::read_to_string(root.join("stat")).ok(),
                "io":fs::read_to_string(root.join("io")).ok()})
            })
            .collect();
        let value = serde_json::json!({
            "utc":host::utc_now()?, "phase":fs::read_to_string(self.output.join("phase")).ok(),
            "elapsed_ns":self.started.elapsed().as_nanos(),
            "unix_ns":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(io::Error::other)?.as_nanos(),
            "processes":processes, "device_stat":fs::read_to_string(&self.block_stat)?,
            "meminfo":fs::read_to_string("/proc/meminfo")?,
            "memory_current":fs::read_to_string(self.cgroup.join("memory.current")).ok(),
            "memory_peak":fs::read_to_string(self.cgroup.join("memory.peak")).ok(),
            "memory_max":fs::read_to_string(self.cgroup.join("memory.max")).ok(),
            "memory_events":fs::read_to_string(self.cgroup.join("memory.events")).ok(),
            "scope":"per-process PSS overlaps cgroup memory; do not add them or guest RAM"
        });
        serde_json::to_writer(&mut self.file, &value)?;
        writeln!(self.file)?;
        self.file.flush()
    }
}

pub(super) fn run(args: &Args, build: &Build, vm: &Path, device: &Path) -> io::Result<()> {
    let socket_dir = tempfile::Builder::new().prefix("cas-native-").tempdir()?;
    let sockets: Vec<_> = (0..args.guests)
        .map(|i| socket_dir.path().join(format!("{i}.sock")))
        .collect();
    let key = args.output.join("key");
    checked(
        Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "cas-native"])
            .arg("-f")
            .arg(&key),
        &args.output,
        "keygen",
    )?;
    let mut daemons = storage(args, build, &sockets)?;
    let mut guests = Vec::new();
    let mut ports = Vec::new();
    let mut processes: Vec<_> = daemons.iter().map(|p| ("storage", p.pid())).collect();
    for (i, socket) in sockets.iter().enumerate() {
        let output = args.output.join(format!("guest-{}", i + 1));
        fs::create_dir(&output)?;
        fs::create_dir(output.join("tmp"))?;
        fs::copy(key.with_extension("pub"), output.join("authorized_keys"))?;
        fs::write(output.join("format"), b"new image\n")?;
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener); // QEMU reports a bind race; we never take another process's port.
        let mut command = Command::new(vm);
        command
            .current_dir(output.join("tmp"))
            .env("TMPDIR", output.join("tmp"))
            .env("CAS_RESULTS_DIR", &output)
            .env("CAS_VHOST_SOCKET", socket)
            .env("CAS_RAW_IMAGE", args.storage.join(format!("raw-{i}.img")))
            .env("CAS_SSH_PORT", port.to_string());
        let mut guest = spawn(&mut command, &output, "console")?;
        qemu::record(&mut guest, &output)?;
        let invocation: serde_json::Value = evidence::read_json(&output.join("qemu.json"))?;
        let pid: u32 = evidence::u64_at(&invocation, "/pid")?
            .try_into()
            .map_err(io::Error::other)?;
        // Invocation capture can precede QEMU opening KVM. Observe the live
        // descriptor, not merely a requested command-line acceleration mode.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let kvm = fs::read_dir(format!("/proc/{pid}/fd"))?
                .filter_map(Result::ok)
                .filter_map(|entry| fs::read_link(entry.path()).ok())
                .any(|path| path.to_string_lossy().contains("kvm-vm"));
            if kvm {
                break;
            }
            process::check_interrupt()?;
            evidence::require(guest.poll()?.is_none(), "QEMU exited before KVM setup")?;
            evidence::require(
                Instant::now() < deadline,
                "QEMU has no live KVM VM descriptor",
            )?;
            thread::sleep(Duration::from_millis(50));
        }
        evidence::write_json(
            &output.join("kvm.json"),
            &serde_json::json!({"pid":pid,"kvm_vm_fd":true}),
        )?;
        processes.push(("qemu", pid));
        guests.push(guest);
        ports.push(port);
    }
    let mut sampler = Sampler::new(args, device, processes)?;
    let started = Instant::now();
    while !(1..=args.guests).all(|i| {
        let path = args.output.join(format!("guest-{i}"));
        path.join("ready").exists() && path.join("host-key.pub").exists()
    }) {
        process::check_interrupt()?;
        require_alive(&mut daemons)?;
        require_alive(&mut guests)?;
        evidence::require(
            started.elapsed() < Duration::from_secs(120),
            "guest readiness timed out",
        )?;
        sampler.tick()?;
        thread::sleep(Duration::from_secs(1));
    }
    let mut known = String::new();
    let mut config = format!(
        "Host *\n User root\n IdentityFile {key:?}\n IdentitiesOnly yes\n UserKnownHostsFile {:?}\n StrictHostKeyChecking yes\n BatchMode yes\n ConnectTimeout 10\n LogLevel ERROR\n",
        args.output.join("known_hosts")
    );
    for (i, port) in ports.iter().enumerate() {
        let name = format!("vm{}", i + 1);
        known.push_str(&format!(
            "{name} {}",
            fs::read_to_string(args.output.join(format!("guest-{}/host-key.pub", i + 1)))?
        ));
        config.push_str(&format!(
            "Host {name}\n HostName 127.0.0.1\n Port {port}\n HostKeyAlias {name}\n"
        ));
    }
    fs::write(args.output.join("known_hosts"), known)?;
    let ssh_config = args.output.join("ssh_config");
    fs::write(&ssh_config, config)?;
    for i in 1..=args.guests {
        checked(
            Command::new("ssh")
                .arg("-F")
                .arg(&ssh_config)
                .arg(format!("vm{i}"))
                .arg("true"),
            &args.output,
            &format!("ssh-{i}"),
        )?;
    }
    evidence::write_json(
        &args.output.join("ready.json"),
        &serde_json::json!({"utc":host::utc_now()?,"guests":args.guests}),
    )?;
    let mut command = Command::new("bash");
    command
        .arg(args.output.join("workload.sh"))
        .args(&args.script_args)
        .env("CAS_SSH_CONFIG", &ssh_config)
        .env("CAS_OUTPUT", &args.output)
        .env("CAS_STORAGE", &args.storage)
        .env("CAS_DEVICE_STAT", &sampler.block_stat)
        .env("CAS_GUESTS", args.guests.to_string())
        .env("CAS_BACKEND", args.backend.name());
    let mut workload = spawn(&mut command, &args.output, "workload")?;
    let started = Instant::now();
    loop {
        process::check_interrupt()?;
        require_alive(&mut daemons)?;
        require_alive(&mut guests)?;
        sampler.tick()?;
        if let Some(status) = workload.poll()? {
            evidence::require(status.success(), &format!("workload failed: {status}"))?;
            break;
        }
        evidence::require(
            started.elapsed() < Duration::from_secs(args.timeout_seconds),
            "workload deadline exceeded",
        )?;
        thread::sleep(Duration::from_secs(1));
    }
    for i in 1..=args.guests {
        fs::write(
            args.output.join(format!("guest-{i}/stop")),
            b"clean shutdown\n",
        )?;
    }
    for guest in &mut guests {
        evidence::require(
            guest.wait(Duration::from_secs(90))?.success(),
            "guest shutdown failed",
        )?;
    }
    for daemon in &mut daemons {
        evidence::require(
            daemon.wait(Duration::from_secs(30))?.success(),
            "storage shutdown failed",
        )?;
    }
    sampler.tick()?;
    Ok(())
}
