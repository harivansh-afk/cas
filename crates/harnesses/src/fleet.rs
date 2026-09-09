//! Controlled publisher image -> prepared base -> independently updated clones.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use clap::{Parser, ValueEnum};
use serde_json::json;

use crate::process::ManagedChild;

const IMAGE_SHA256: &str = "7df0201546f75b8bcc1044594c806c35749421ad3c9bc1be2a3ab806cfae39cc";
const ROLES: [&str; 3] = ["web", "cache", "database"];
const EPOCHS: [&str; 2] = ["20260805T000000Z", "20260905T000000Z"];

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Phase {
    All,
    Prepare,
    August,
    September,
}

#[derive(Parser)]
#[command(override_usage = "cas-census-fleet --image INPUT --output OUTPUT [--phase PHASE]")]
pub struct Args {
    #[arg(long)]
    output: PathBuf,
    /// Official noble-server-cloudimg-arm64.img dated 20260705; checksum enforced.
    #[arg(long)]
    image: PathBuf,
    #[arg(long, value_enum, default_value = "all")]
    phase: Phase,
    #[arg(long, hide = true)]
    firmware: PathBuf,
    #[arg(long, hide = true)]
    firmware_vars: PathBuf,
    #[arg(long, hide = true)]
    workload: PathBuf,
}

fn execute(dir: &Path, name: &str, command: &mut Command) -> io::Result<()> {
    writeln!(
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("commands.log"))?,
        "{command:?}"
    )?;
    let log = File::create(dir.join(format!("{name}.log")))?;
    command
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    let status = ManagedChild::spawn(command)?.wait(Duration::from_secs(900))?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "{name} failed ({status}); see {}",
            dir.display()
        )));
    }
    Ok(())
}

fn copy_writable(from: &Path, to: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::copy(from, to)?;
    fs::set_permissions(to, fs::Permissions::from_mode(0o600))
}

fn boot(args: &Args, dir: &Path, role: &str, snapshot: &str) -> io::Result<()> {
    let guest = dir.join("guest");
    fs::create_dir(&guest)?;
    let workload = fs::read_to_string(&args.workload)?
        .replace("@ROLE@", role)
        .replace("@SNAPSHOT@", snapshot);
    let config = json!({
        "hostname": role,
        "manage_etc_hosts": true,
        "package_update": false,
        "package_upgrade": false,
        "bootcmd": [["systemctl", "mask", "--now", "apt-daily.timer", "apt-daily-upgrade.timer",
            "apt-daily.service", "apt-daily-upgrade.service", "snapd.service", "snapd.socket"]],
        "write_files": [{"path": "/var/tmp/cas-update.sh", "permissions": "0700", "content": workload}],
        "runcmd": [["bash", "/var/tmp/cas-update.sh"]],
    });
    fs::write(
        dir.join("user-data"),
        format!("#cloud-config\n{config:#}\n"),
    )?;
    fs::write(
        dir.join("meta-data"),
        format!("instance-id: cas-{role}-{snapshot}\nlocal-hostname: {role}\n"),
    )?;
    execute(
        dir,
        "seed",
        Command::new("cloud-localds")
            .arg(dir.join("seed.img"))
            .arg(dir.join("user-data"))
            .arg(dir.join("meta-data")),
    )?;
    copy_writable(&args.firmware_vars, &dir.join("vars.fd"))?;
    execute(
        dir,
        "qemu",
        Command::new("qemu-system-aarch64")
            .args([
                "-machine",
                "virt,accel=kvm,gic-version=3",
                "-cpu",
                "host",
                "-smp",
                "2",
                "-m",
                "2048",
                "-display",
                "none",
                "-monitor",
                "none",
                "-no-reboot",
            ])
            .args([
                "-serial",
                &format!("file:{}", dir.join("console.log").display()),
            ])
            .args([
                "-drive",
                &format!(
                    "if=pflash,format=raw,readonly=on,file={}",
                    args.firmware.display()
                ),
            ])
            .args([
                "-drive",
                &format!(
                    "if=pflash,format=raw,file={}",
                    dir.join("vars.fd").display()
                ),
            ])
            .args([
                "-drive",
                &format!(
                    "if=virtio,format=qcow2,file={}",
                    dir.join("disk.qcow2").display()
                ),
            ])
            .args([
                "-drive",
                &format!(
                    "if=virtio,format=raw,readonly=on,file={}",
                    dir.join("seed.img").display()
                ),
            ])
            .args([
                "-virtfs",
                &format!(
                    "local,path={},mount_tag=cas-evidence,security_model=none",
                    guest.display()
                ),
            ])
            .args(["-nic", "user,model=virtio-net-pci"]),
    )?;
    if fs::read_to_string(guest.join("exit-code"))?.trim() != "0" {
        return Err(io::Error::other(format!(
            "guest workload failed: {}",
            guest.display()
        )));
    }
    execute(
        dir,
        "map",
        Command::new("qemu-img")
            .args(["map", "--output=json"])
            .arg(dir.join("disk.qcow2")),
    )?;
    execute(
        dir,
        "convert",
        Command::new("qemu-img")
            .args(["convert", "-f", "qcow2", "-O", "raw"])
            .arg(dir.join("disk.qcow2"))
            .arg(dir.join("image.raw")),
    )?;
    execute(
        dir,
        "normalize",
        Command::new("cas-normalize-root")
            .arg(dir.join("image.raw"))
            .arg(dir.join("root")),
    )?;
    let mut permissions = fs::metadata(dir.join("disk.qcow2"))?.permissions();
    permissions.set_readonly(true);
    fs::set_permissions(dir.join("disk.qcow2"), permissions)
}

fn census(output: &Path, name: &str, images: &[PathBuf]) -> io::Result<()> {
    execute(
        output,
        name,
        Command::new("casctl").arg("census").args(images),
    )?;
    fs::rename(
        output.join(format!("{name}.log")),
        output.join(format!("{name}.json")),
    )
}

pub fn run(mut args: Args) -> io::Result<()> {
    if std::env::consts::ARCH != "aarch64" {
        return Err(io::Error::other(
            "this dated ARM64 fleet requires native aarch64 KVM",
        ));
    }
    args.image = fs::canonicalize(&args.image)?;
    args.firmware = fs::canonicalize(&args.firmware)?;
    args.firmware_vars = fs::canonicalize(&args.firmware_vars)?;
    args.workload = fs::canonicalize(&args.workload)?;
    if args.phase == Phase::All || args.phase == Phase::Prepare {
        if let Some(parent) = args.output.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::create_dir(&args.output)?;
    }
    args.output = fs::canonicalize(&args.output)?;
    for path in [&args.output, &args.firmware, &args.firmware_vars] {
        if path.to_str().is_none_or(|text| text.contains([',', '\n'])) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "QEMU paths must be UTF-8 without commas or newlines",
            ));
        }
    }
    let base = args.output.join("base");
    let base_root = base.join("root.root.raw");
    if args.phase == Phase::All || args.phase == Phase::Prepare {
        execute(
            &args.output,
            "input-sha256",
            Command::new("sha256sum").arg(&args.image),
        )?;
        let checksum = fs::read_to_string(args.output.join("input-sha256.log"))?;
        if checksum.split_whitespace().next() != Some(IMAGE_SHA256) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "publisher image checksum mismatch",
            ));
        }
        fs::copy(&args.workload, args.output.join("workload.sh"))?;
        fs::write(
            args.output.join("fleet.json"),
            format!(
                "{:#}\n",
                json!({
                    "schema_version": 1, "image_date": "20260705", "image_sha256": IMAGE_SHA256,
                    "roles": ROLES, "epochs": EPOCHS, "cpus_per_guest": 2, "memory_mib": 2048,
                    "image": args.image, "firmware": args.firmware, "workload": args.workload,
                    "note": "Stock QEMU storage; offline content census, not a CAS performance experiment"
                })
            ),
        )?;
        fs::create_dir(&base)?;
        copy_writable(&args.image, &base.join("disk.qcow2"))?;
        execute(
            &base,
            "resize",
            Command::new("qemu-img")
                .arg("resize")
                .arg(base.join("disk.qcow2"))
                .arg("8G"),
        )?;
        boot(&args, &base, "base", "base")?;
        census(&args.output, "t0", &vec![base_root.clone(); 4])?;
    } else if fs::read(args.output.join("workload.sh"))? != fs::read(&args.workload)? {
        return Err(io::Error::other("workload changed between fleet phases"));
    }
    for (index, epoch) in EPOCHS.iter().enumerate() {
        if args.phase != Phase::All && args.phase != [Phase::August, Phase::September][index] {
            continue;
        }
        let epoch_dir = args.output.join(format!("t{}", index + 1));
        fs::create_dir(&epoch_dir)?;
        let mut images = vec![base_root.clone()];
        for role in ROLES {
            let dir = epoch_dir.join(role);
            fs::create_dir(&dir)?;
            if index == 0 {
                execute(
                    &dir,
                    "clone",
                    Command::new("qemu-img")
                        .args(["create", "-f", "qcow2", "-F", "qcow2", "-b"])
                        .arg(base.join("disk.qcow2"))
                        .arg(dir.join("disk.qcow2")),
                )?;
                execute(
                    &dir,
                    "clone-compare",
                    Command::new("qemu-img")
                        .args(["compare", "-f", "qcow2", "-F", "qcow2"])
                        .arg(base.join("disk.qcow2"))
                        .arg(dir.join("disk.qcow2")),
                )?;
            } else {
                copy_writable(
                    &args.output.join(format!("t{index}/{role}/disk.qcow2")),
                    &dir.join("disk.qcow2"),
                )?;
            }
            boot(&args, &dir, role, epoch)?;
            images.push(dir.join("root.root.raw"));
        }
        census(&args.output, &format!("t{}", index + 1), &images)?;
    }
    Ok(())
}
