use super::*;
use std::{
    net::TcpListener,
    os::unix::fs::{MetadataExt, PermissionsExt},
    thread,
    time::Instant,
};

fn lock() -> io::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(root()?.join("operation.lock"))?;
    file.try_lock()
        .map_err(|_| io::Error::other("another casctl lifecycle operation is running"))?;
    Ok(file)
}
fn user_command(program: &str) -> Process {
    let mut command = Process::new(program);
    // SAFETY: getuid has no preconditions and does not retain memory.
    let runtime = format!("/run/user/{}", unsafe { libc::getuid() });
    command.env("XDG_RUNTIME_DIR", &runtime).env(
        "DBUS_SESSION_BUS_ADDRESS",
        format!("unix:path={runtime}/bus"),
    );
    command.arg("--user");
    command
}
fn unit_busy(state: &str) -> io::Result<bool> {
    match state {
        "active" | "activating" | "deactivating" | "reloading" | "refreshing" => Ok(true),
        "inactive" | "failed" => Ok(false),
        _ => Err(io::Error::other(format!(
            "unknown user-service state: {state}"
        ))),
    }
}
fn running(config: &Config) -> io::Result<bool> {
    let output = user_command("systemctl")
        .args(["show", "--property=ActiveState", "--value", &config.unit])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("cannot query the systemd user service"));
    }
    unit_busy(String::from_utf8_lossy(&output.stdout).trim())
}
fn build() -> io::Result<LabBuild> {
    let path = std::env::var_os("CAS_LAB_BUILD").ok_or_else(|| {
        io::Error::other("use the packaged CLI: nix build .#casctl; ./result/bin/casctl new NAME")
    })?;
    evidence::read_json(Path::new(&path))
}
pub(super) fn available(path: &Path) -> io::Result<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(io::Error::other)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is terminated; stat points to the correct writable ABI type.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statvfs initialized stat.
    let stat = unsafe { stat.assume_init() };
    stat.f_bavail
        .checked_mul(stat.f_frsize)
        .ok_or_else(|| io::Error::other("space overflow"))
}
fn preflight(required: u64) -> io::Result<()> {
    File::options()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .map_err(|e| io::Error::other(format!("KVM unavailable: {e}")))?;
    let free = available(&root()?)?;
    if free < required + 25 * GIB {
        return Err(io::Error::other(format!(
            "need room for the lab plus 25 GiB free; available {:.1} GiB",
            free as f64 / GIB as f64
        )));
    }
    let memory = fs::read_to_string("/proc/meminfo")?;
    let kb: u64 = memory
        .lines()
        .find_map(|line| line.strip_prefix("MemAvailable:"))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| io::Error::other("missing MemAvailable"))?;
    if kb * 1024 < 6 * GIB {
        return Err(io::Error::other(
            "need at least 6 GiB available RAM for the lab",
        ));
    }
    checked(
        user_command("systemctl")
            .args(["show-environment"])
            .stdout(Stdio::null()),
    )
}
pub(super) fn doctor() -> io::Result<()> {
    preflight(4 * GIB)?;
    let build = build()?;
    if !build.vm.is_file() {
        return Err(io::Error::other(
            "pinned VM build is unavailable; rebuild .#casctl",
        ));
    }
    println!(
        "KVM and user services available\nDisk free: {:.1} GiB; each lab disk capped at 4 GiB\nOuter host: KVM / XFS / 4 GiB RAM\nGuests: TCG / 512 MiB RAM each / 512 MiB data disk\nBuild: {} ({})\nState: {}",
        available(&root()?)? as f64 / GIB as f64,
        build.source_revision,
        build.system,
        root()?.display()
    );
    Ok(())
}
pub(super) fn new(name: Option<String>, count: u8, backend: Backend, json: bool) -> io::Result<()> {
    let _lock = lock()?;
    let name = name.unwrap_or(format!("vm-{}", timestamp()?));
    let directory = lab(&name)?;
    preflight(4 * GIB)?;
    let build = build()?;
    fs::create_dir(&directory)
        .map_err(|e| io::Error::other(format!("cannot create {name}: {e}")))?;
    let unit = format!(
        "casctl-{}.service",
        &blake3::hash(directory.as_os_str().as_encoded_bytes()).to_hex()[..20]
    );
    let config = Config {
        schema_version: 1,
        name,
        count,
        backend,
        disk_bytes: 4 * GIB,
        build,
        unit,
    };
    evidence::write_json(&directory.join("config.json"), &config)?;
    let vm_output = config
        .build
        .vm
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| io::Error::other("invalid pinned VM path"))?;
    checked(
        Process::new("nix-store")
            .arg("--add-root")
            .arg(directory.join("build"))
            .args(["--indirect", "--realise"])
            .arg(vm_output)
            .stdout(Stdio::null()),
    )?;
    File::options()
        .write(true)
        .create_new(true)
        .open(directory.join("disk.raw"))?
        .set_len(config.disk_bytes)?;
    checked(
        Process::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-C", "casctl-local"])
            .arg("-f")
            .arg(directory.join("key")),
    )?;
    fs::set_permissions(directory.join("key"), fs::Permissions::from_mode(0o600))?;
    boot(&directory, &config)?;
    if json {
        println!("{}", serde_json::to_string(&entry(&directory, &config)?)?);
    } else {
        println!(
            "{} is ready. casctl shell {}\nData: /mnt/cas in the guest\nResults: {}",
            config.name,
            config.name,
            directory.display()
        );
    }
    Ok(())
}
pub(super) fn start(name: &str) -> io::Result<()> {
    let _lock = lock()?;
    let (directory, config) = load(name)?;
    preflight(config.disk_bytes)?;
    boot(&directory, &config)?;
    println!("{name} is ready. casctl shell {name}");
    Ok(())
}
fn boot(directory: &Path, config: &Config) -> io::Result<()> {
    if running(config)? {
        return Err(io::Error::other("lab is already running"));
    }
    if !directory.join("disk.raw").is_file() {
        return Err(io::Error::other("lab disk is missing"));
    }
    let run = directory.join("runs").join(timestamp()?);
    fs::create_dir_all(run.join("tmp"))?;
    fs::copy(directory.join("key.pub"), run.join("authorized_keys"))?;
    evidence::write_json(&run.join("config.json"), config)?;
    // Reserve a loopback-only candidate. A race is reported by QEMU, never worked around by stealing a port.
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    publish(
        &directory.join("active.json"),
        &Active {
            run: run.clone(),
            port,
        },
    )?;
    drop(listener);
    let mut service = user_command("systemd-run");
    service
        .args([
            "--quiet",
            "--collect",
            "--unit",
            &config.unit,
            "--property=MemoryMax=6G",
            "--property=MemorySwapMax=0",
            "--property=KillMode=control-group",
            "--property=TimeoutStopSec=15",
        ])
        .arg(format!(
            "--setenv=PATH={}",
            std::env::var("PATH").map_err(io::Error::other)?
        ))
        .arg(std::env::current_exe()?)
        .arg("lab-supervise")
        .arg(directory)
        .arg(&run);
    checked(&mut service)?;
    eprintln!(
        "Booting {}: {} guest(s), {} backend…",
        config.name,
        config.count,
        config.backend.name()
    );
    let started = Instant::now();
    loop {
        if run.join("ready.json").exists() && run.join("ssh_config").exists() {
            checked(ssh_target(directory, "vm1", &["true".into()])?.stdout(Stdio::null()))?;
            return Ok(());
        }
        if run.join("outcome.json").exists() || !running(config)? || started.elapsed() >= STARTUP {
            fs::write(run.join("stop"), b"startup canceled\n")?;
            // Ensure a failed creation cannot leave an untracked lab consuming resources.
            let _ = checked(user_command("systemctl").args(["stop", &config.unit]));
            return Err(io::Error::other(format!(
                "{} did not become ready; casctl logs {} (evidence: {})",
                config.name,
                config.name,
                run.display()
            )));
        }
        thread::sleep(Duration::from_millis(250));
    }
}
pub(super) fn configure_ssh(
    directory: &Path,
    run: &Path,
    config: &Config,
    port: u16,
) -> io::Result<()> {
    let mut keys = String::new();
    keys.push_str(&format!(
        "cas-host {}",
        fs::read_to_string(run.join("host-key.pub"))?
    ));
    for i in 1..=config.count {
        keys.push_str(&format!(
            "cas-vm{i} {}",
            fs::read_to_string(run.join(format!("guest-{i}/host-key.pub")))?
        ));
    }
    fs::write(run.join("known_hosts"), keys)?;
    let mut text = format!(
        "Host *\n User root\n IdentityFile \"{}\"\n IdentitiesOnly yes\n UserKnownHostsFile \"{}\"\n StrictHostKeyChecking yes\n BatchMode yes\n ConnectTimeout 10\n LogLevel ERROR\nHost host\n HostName 127.0.0.1\n Port {port}\n HostKeyAlias cas-host\n",
        directory.join("key").display(),
        run.join("known_hosts").display()
    );
    for i in 1..=config.count {
        text += &format!(
            "Host vm{i}\n HostName 127.0.0.1\n Port {}\n HostKeyAlias cas-vm{i}\n ProxyJump host\n",
            23479 + u16::from(i)
        );
    }
    fs::write(run.join("ssh_config"), text)
}
fn ssh_target(directory: &Path, target: &str, command: &[String]) -> io::Result<Process> {
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    let mut ssh = Process::new("ssh");
    ssh.arg("-F").arg(active.run.join("ssh_config"));
    if command.is_empty() {
        ssh.arg("-t");
    }
    ssh.arg(target);
    if !command.is_empty() {
        ssh.arg(
            command
                .iter()
                .map(|s| quote(s))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    Ok(ssh)
}
pub(super) fn ssh(name: &str, command: &[String]) -> io::Result<Process> {
    let (name, peer) = name.split_once('/').unwrap_or((name, "1"));
    let (directory, config) = load(name)?;
    let peer: u8 = peer
        .parse()
        .map_err(|_| io::Error::other("guest must be NAME/NUMBER"))?;
    if peer == 0 || peer > config.count {
        return Err(io::Error::other("guest number is outside this lab"));
    }
    if !running(&config)? {
        return Err(io::Error::other("lab is stopped; use casctl start NAME"));
    }
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    if !active.run.join("ssh_config").is_file() {
        return Err(io::Error::other(
            "lab is still starting; inspect casctl ls or casctl logs NAME",
        ));
    }
    ssh_target(&directory, &format!("vm{peer}"), command)
}
pub(super) fn stop(name: &str, force: bool) -> io::Result<()> {
    let _lock = lock()?;
    let (directory, config) = load(name)?;
    if !running(&config)? {
        println!("{name} is already stopped");
        return Ok(());
    }
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    if force {
        checked(user_command("systemctl").args(["stop", &config.unit]))?;
    } else {
        fs::write(active.run.join("stop"), b"graceful shutdown\n")?;
        let started = Instant::now();
        while running(&config)? {
            if started.elapsed() > STARTUP {
                return Err(io::Error::other(
                    "shutdown timed out; inspect logs or use stop --force",
                ));
            }
            thread::sleep(Duration::from_millis(250));
        }
        let outcome: serde_json::Value = evidence::read_json(&active.run.join("outcome.json"))?;
        if outcome["success"] != true {
            return Err(io::Error::other(
                "lab stopped with errors; evidence retained",
            ));
        }
    }
    println!("Stopped {name}; disk and results retained.");
    Ok(())
}
fn entry(directory: &Path, config: &Config) -> io::Result<serde_json::Value> {
    let active: Option<Active> = if directory.join("active.json").exists() {
        Some(evidence::read_json(&directory.join("active.json"))?)
    } else {
        None
    };
    let live = running(config)?;
    let ready = active
        .as_ref()
        .is_some_and(|a| a.run.join("ready.json").exists());
    Ok(
        serde_json::json!({"name":config.name,"guests":config.count,"backend":config.backend.name(),"state":if live { if ready {"running"} else {"starting"} } else {"stopped"},"disk_allocated_bytes":fs::metadata(directory.join("disk.raw"))?.blocks()*512,"disk_cap_bytes":config.disk_bytes,"results":active.map(|a|a.run),"outer_acceleration":"kvm","guest_acceleration":"tcg"}),
    )
}
pub(super) fn list(json: bool) -> io::Result<()> {
    let mut rows = Vec::new();
    for entry in fs::read_dir(root()?.join("labs"))? {
        let path = entry?.path();
        if path.join("config.json").exists() {
            rows.push(self::entry(
                &path,
                &evidence::read_json(&path.join("config.json"))?,
            )?);
        }
    }
    rows.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_string());
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    } else {
        println!("NAME                 STATE      BACKEND GUESTS DISK");
        for row in rows {
            println!(
                "{:<20} {:<10} {:<7} {:<6} {:.1} MiB",
                row["name"].as_str().unwrap_or_default(),
                row["state"].as_str().unwrap_or_default(),
                row["backend"].as_str().unwrap_or_default(),
                row["guests"],
                row["disk_allocated_bytes"].as_u64().unwrap_or(0) as f64 / 1048576.0
            );
        }
    }
    Ok(())
}
pub(super) fn status(name: &str, json: bool) -> io::Result<()> {
    let (directory, config) = load(name)?;
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    let mut value = entry(&directory, &config)?;
    for (key, file) in [
        ("processes", "memory.json"),
        ("storage", "storage.json"),
        ("lab_memory", "spark-memory.json"),
    ] {
        let path = active.run.join(file);
        if path.exists() {
            value[key] = evidence::read_json(&path)?;
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        println!(
            "{}: {} ({} guest(s), {} backend)",
            config.name,
            value["state"].as_str().unwrap_or_default(),
            config.count,
            config.backend.name()
        );
        println!(
            "Disk allocation: {:.1} MiB / 4 GiB",
            value["disk_allocated_bytes"].as_u64().unwrap_or(0) as f64 / 1048576.0
        );
        if let Some(processes) = value["processes"]["processes"].as_array() {
            let pss: u64 = processes
                .iter()
                .filter_map(|p| p["pss_bytes"].as_u64())
                .sum();
            println!(
                "Inner processes: {:.1} MiB PSS (outer VM excluded)",
                pss as f64 / 1048576.0
            );
        }
        let host = &value["storage"]["host"];
        if !host.is_null() {
            println!(
                "Storage pressure: {}  GC required: {}",
                host["space"]["pressured"], host["collection_required"]
            );
            println!("Staging: {}", host["staging"]);
            println!("Cache: {}", host["cache"]);
        }
        println!("Results: {}", active.run.display());
    }
    Ok(())
}
pub(super) fn logs(name: &str) -> io::Result<()> {
    let (directory, config) = load(name)?;
    let active: Active = evidence::read_json(&directory.join("active.json"))?;
    for file in [
        "outcome.json",
        "host-outcome.json",
        "service.log",
        "console.log",
        "host.log",
    ] {
        let path = active.run.join(file);
        if path.exists() {
            println!("{}\n{}", path.display(), tail(&path, 40)?);
        }
    }
    for i in 1..=config.count {
        let path = active.run.join(format!("guest-{i}/console.log"));
        if path.exists() {
            println!("{}\n{}", path.display(), tail(&path, 20)?);
        }
    }
    Ok(())
}
fn tail(path: &Path, lines: usize) -> io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(
        file.metadata()?.len().saturating_sub(65536),
    ))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<_> = text.lines().rev().take(lines).collect();
    Ok(lines.into_iter().rev().collect::<Vec<_>>().join("\n"))
}
pub(super) fn remove(name: &str) -> io::Result<()> {
    let _lock = lock()?;
    let (directory, config) = load(name)?;
    if running(&config)? {
        return Err(io::Error::other("stop the lab before removing its disk"));
    }
    let mut deleted = Vec::new();
    let mut bulk = vec![directory.join("disk.raw")];
    let runs = directory.join("runs");
    if runs.exists() {
        for entry in fs::read_dir(runs)? {
            let run = entry?.path();
            bulk.push(run.join("tmp"));
            for i in 1..=config.count {
                bulk.push(run.join(format!("guest-{i}/tmp")));
            }
        }
    }
    bulk.retain(|p| p.exists());
    for path in &bulk {
        inventory(path, &mut deleted)?;
    }
    publish(
        &directory.join("cleanup-plan.json"),
        &serde_json::json!({"files":deleted,"recorded_at":crate::host::utc_now()?}),
    )?;
    for path in bulk {
        if path.symlink_metadata()?.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    for secret in ["key", "key.pub", "build"] {
        let path = directory.join(secret);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    evidence::write_json(
        &directory.join("cleanup.json"),
        &serde_json::json!({"deleted":deleted,"bulk_available":false,"removed_at":crate::host::utc_now()?}),
    )?;
    let archives = root()?.join("archives");
    fs::create_dir_all(&archives)?;
    let archive = archives.join(format!("{name}-{}", timestamp()?));
    fs::rename(&directory, &archive)?;
    println!("Removed {name}'s disk. Results: {}", archive.display());
    Ok(())
}
fn inventory(path: &Path, entries: &mut Vec<serde_json::Value>) -> io::Result<()> {
    let meta = path.symlink_metadata()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path)? {
            inventory(&entry?.path(), entries)?;
        }
    } else {
        entries.push(
            serde_json::json!({"path":path,"bytes":meta.len(),"allocated_bytes":meta.blocks()*512}),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn shutdown_is_busy_until_systemd_releases_the_process_group() {
        for state in ["active", "activating", "deactivating", "reloading"] {
            assert!(super::unit_busy(state).unwrap());
        }
        assert!(!super::unit_busy("inactive").unwrap());
        assert!(!super::unit_busy("failed").unwrap());
        assert!(super::unit_busy("unexpected").is_err());
    }
}
