//! Host-native storage and direct KVM guests. Never formats a device or replaces evidence.
mod runtime;

use crate::{evidence, host, qemu};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
};

const GIB: u64 = 1024 * 1024 * 1024;
const STORE: &str = "01010101010101010101010101010101";
/// The guest limit the campaign driver reads from `crates/harnesses/native-limits.json`
/// (inside the crate, so the packaged build sees it); a unit test keeps the two equal.
pub const MAX_GUESTS: u8 = 16;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Raw,
    Daemon,
    Cas,
}
impl Backend {
    fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Daemon => "daemon",
            Self::Cas => "cas",
        }
    }
}

#[derive(clap::Args, Serialize)]
pub struct Args {
    /// New evidence directory, on a different filesystem from storage.
    #[arg(long)]
    output: PathBuf,
    /// New storage directory under an already-mounted XFS filesystem (CAS and
    /// daemon backends) or an XFS or ZFS filesystem (raw backend).
    #[arg(long)]
    storage: PathBuf,
    /// Block device backing that mount: the mount's own device for XFS, a vdev
    /// of the dataset's pool for ZFS; identity is checked before writing files.
    #[arg(long)]
    device: PathBuf,
    /// Explicitly label a loop-device run as development, never native-media evidence.
    #[arg(long)]
    allow_loop_device: bool,
    #[arg(long)]
    build_info: PathBuf,
    /// Host shell script. CAS_SSH_CONFIG and CAS_OUTPUT identify the prepared guests/results.
    #[arg(long)]
    script: PathBuf,
    /// Repeat to pass an argument to the retained workload script.
    #[arg(long = "script-arg")]
    script_args: Vec<String>,
    #[arg(long, value_enum)]
    backend: Backend,
    /// Concurrent guests, each with its own data disk and fio job.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=MAX_GUESTS as i64))]
    guests: u8,
    #[arg(long, default_value_t = 4 * GIB)]
    image_bytes: u64,
    #[arg(long, default_value_t = 64 * 1024 * 1024)]
    segment_bytes: u64,
    #[arg(long, default_value_t = GIB)]
    staging_bytes: u64,
    #[arg(long, default_value_t = 16 * 1024 * 1024)]
    cache_bytes: u64,
    #[arg(long, default_value_t = 900, value_parser = clap::value_parser!(u64).range(1..=3600))]
    timeout_seconds: u64,
}

#[derive(Deserialize)]
struct Build {
    source_revision: String,
    source_path: PathBuf,
    lock: PathBuf,
    host: PathBuf,
    daemon: PathBuf,
    guests: std::collections::BTreeMap<String, PathBuf>,
    acceleration: String,
}

fn image(index: u8) -> String {
    format!("{:032x}", u128::from(index) + 2)
}

/// Filesystem types a backend may store on. CAS needs XFS (reflink, hole
/// punching, aligned O_DIRECT); the raw image control may also sit on a ZFS
/// dataset so the ZFS arms share this runner.
fn allowed_filesystems(backend: Backend) -> &'static [&'static str] {
    match backend {
        Backend::Raw => &["xfs", "zfs"],
        Backend::Daemon | Backend::Cas => &["xfs"],
    }
}

/// Whether `path` names a partition of `device` by the kernel's rule: a digit
/// suffix, with a `p` separator only when the device name itself ends in a
/// digit (`/dev/sda1`, `/dev/nvme0n1p2`), or udev's `-partN`. `/dev/sdap` is
/// another disk, not a partition of `/dev/sda`.
fn is_partition_of(path: &str, device: &str) -> bool {
    let Some(rest) = path.strip_prefix(device) else {
        return false;
    };
    let digits = if let Some(digits) = rest.strip_prefix("-part") {
        Some(digits)
    } else if device.ends_with(|c: char| c.is_ascii_digit()) {
        rest.strip_prefix('p')
    } else {
        Some(rest)
    };
    digits.is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `zpool list -v -H -P <pool>` lists `device` (or a partition on it) as a vdev.
fn pool_has_device(listing: &str, device: &Path) -> bool {
    let device = device.to_string_lossy();
    listing.lines().skip(1).any(|line| {
        line.split('\t')
            .map(str::trim)
            .filter(|field| field.starts_with('/'))
            .any(|path| {
                let path = fs::canonicalize(path)
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| path.to_string());
                path == device || is_partition_of(&path, &device)
            })
    })
}

fn zfs_pool(parent: &Path) -> io::Result<String> {
    let source = std::process::Command::new("findmnt")
        .args(["-n", "-o", "SOURCE", "-T"])
        .arg(parent)
        .output()?;
    let dataset = String::from_utf8_lossy(&source.stdout).trim().to_string();
    evidence::require(
        source.status.success() && !dataset.is_empty(),
        "storage dataset unknown",
    )?;
    Ok(dataset.split('/').next().unwrap_or(&dataset).to_string())
}

fn validate(args: &Args) -> io::Result<PathBuf> {
    for path in [&args.output, &args.storage] {
        qemu::valid_path(path)?;
        evidence::require(
            path.symlink_metadata().is_err(),
            "storage and output must be new paths",
        )?;
    }
    evidence::require(
        args.image_bytes >= 64 * 1024 * 1024 && args.image_bytes.is_multiple_of(4096),
        "image size must be aligned and at least 64 MiB",
    )?;
    let parent = args
        .storage
        .parent()
        .ok_or_else(|| io::Error::other("storage needs an existing parent"))?
        .canonicalize()?;
    let device = args.device.canonicalize()?;
    let metadata = device.metadata()?;
    evidence::require(
        metadata.file_type().is_block_device(),
        "--device must identify a block device",
    )?;
    let fs_type = std::process::Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-T"])
        .arg(&parent)
        .output()?;
    let fs_type = String::from_utf8_lossy(&fs_type.stdout).trim().to_string();
    evidence::require(
        allowed_filesystems(args.backend).contains(&fs_type.as_str()),
        &format!(
            "{} storage needs an existing {} mount, found {fs_type:?}",
            args.backend.name(),
            allowed_filesystems(args.backend).join(" or ")
        ),
    )?;
    if fs_type == "zfs" {
        let pool = zfs_pool(&parent)?;
        let listing = std::process::Command::new("zpool")
            .args(["list", "-v", "-H", "-P"])
            .arg(&pool)
            .output()?;
        evidence::require(
            listing.status.success()
                && pool_has_device(&String::from_utf8_lossy(&listing.stdout), &device),
            "--device is not a vdev of the storage dataset's pool",
        )?;
    } else {
        evidence::require(
            parent.metadata()?.dev() == metadata.rdev(),
            "storage filesystem does not match --device",
        )?;
    }
    let sysfs = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ));
    evidence::require(
        args.allow_loop_device || !sysfs.join("loop").exists(),
        "loop devices require --allow-loop-device and are development evidence",
    )?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")?;
    Ok(device)
}

fn storage_fstype(storage: &Path) -> io::Result<String> {
    let output = std::process::Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-T"])
        .arg(storage)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run(mut args: Args) -> io::Result<()> {
    let device = validate(&args)?;
    let build: Build = evidence::read_json(&args.build_info)?;
    evidence::require(build.acceleration == "kvm", "native build must require KVM")?;
    let vm = build
        .guests
        .get(args.backend.name())
        .ok_or_else(|| io::Error::other("backend missing from pinned build"))?
        .clone();
    args.script = args.script.canonicalize()?;
    args.output = qemu::prepare_output(&args.output)?;
    evidence::require(
        args.output.metadata()?.dev() != args.storage.parent().unwrap().metadata()?.dev(),
        "reports and experiment storage must be on different filesystems",
    )?;
    args.storage = qemu::prepare_output(&args.storage)?;
    fs::copy(&args.build_info, args.output.join("build.json"))?;
    fs::copy(&build.lock, args.output.join("flake.lock"))?;
    fs::copy(&args.script, args.output.join("workload.sh"))?;
    evidence::write_json(&args.output.join("request.json"), &args)?;
    // SAFETY: sysconf takes a constant selector and no pointers or retained storage.
    let clock_ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    evidence::require(clock_ticks > 0, "CPU tick frequency unavailable")?;
    evidence::write_json(
        &args.output.join("identity.json"),
        &serde_json::json!({
            "source_revision":build.source_revision, "source_path":build.source_path,
            "started_at":host::utc_now()?, "cpu_affinity":host::cpu_affinity()?,
            "clock_ticks_per_second":clock_ticks,
            "host_kernel":host::identity()?.kernel, "device":device,
            "workload_blake3":blake3::hash(&fs::read(&args.script)?).to_hex().to_string(),
            "paper_gates":[], "native_media":!args.allow_loop_device,
            "storage_fstype":storage_fstype(&args.storage)?, "guests":args.guests,
            "geometry":"storage on host; direct KVM guests; no outer VM"
        }),
    )?;
    let started = host::utc_now()?;
    let result = runtime::run(&args, &build, &vm, &device);
    evidence::write_json(
        &args.output.join("outcome.json"),
        &serde_json::json!({
            "success":result.is_ok(), "error":result.as_ref().err().map(ToString::to_string),
            "started":started, "finished":host::utc_now()?, "storage_retained":true,
            "paper_gates":[]
        }),
    )?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        args: Args,
    }
    fn args(extra: &[&str]) -> Args {
        Cli::try_parse_from(
            [
                "native",
                "--output",
                "new-output",
                "--storage",
                "new-storage",
                "--device",
                "/dev/not-a-device",
                "--build-info",
                "build.json",
                "--script",
                "work.sh",
                "--backend",
                "cas",
            ]
            .into_iter()
            .chain(extra.iter().copied()),
        )
        .unwrap()
        .args
    }
    #[test]
    fn existing_evidence_is_refused_before_device_access() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = args(&[]);
        args.output = dir.path().to_path_buf();
        assert!(
            validate(&args)
                .unwrap_err()
                .to_string()
                .contains("must be new")
        );
    }
    #[test]
    fn guest_counts_are_bounded() {
        assert_eq!(args(&["--guests", "16"]).guests, 16);
        let cli = Cli::try_parse_from([
            "native",
            "--output",
            "o",
            "--storage",
            "s",
            "--device",
            "d",
            "--build-info",
            "b",
            "--script",
            "w",
            "--backend",
            "raw",
            "--guests",
            "17",
        ]);
        assert!(cli.is_err());
    }
    #[test]
    fn only_the_raw_backend_accepts_zfs() {
        assert_eq!(allowed_filesystems(Backend::Raw), &["xfs", "zfs"]);
        assert_eq!(allowed_filesystems(Backend::Cas), &["xfs"]);
        assert_eq!(allowed_filesystems(Backend::Daemon), &["xfs"]);
    }
    #[test]
    fn pool_membership_matches_whole_disks_and_their_partitions() {
        let listing = "tank\t1.6T\t1M\t1.6T\t-\t-\t0%\t0%\t1.00x\tONLINE\t-\n\t/dev/nvme1n1p1\t1.6T\t1M\t1.6T\t-\t-\t0%\t0.00%\t-\tONLINE\n";
        assert!(pool_has_device(listing, Path::new("/dev/nvme1n1")));
        assert!(pool_has_device(listing, Path::new("/dev/nvme1n1p1")));
        assert!(!pool_has_device(listing, Path::new("/dev/nvme0n1")));
        assert!(!pool_has_device(
            "tank\tONLINE\n",
            Path::new("/dev/nvme1n1")
        ));
    }
    #[test]
    fn malformed_geometry_and_qemu_paths_are_refused_before_device_access() {
        let mut args = args(&["--image-bytes", "4097"]);
        assert!(validate(&args).unwrap_err().to_string().contains("aligned"));
        args.output = "bad,path".into();
        assert!(validate(&args).unwrap_err().to_string().contains("commas"));
    }

    #[test]
    fn partition_naming_follows_the_kernel_rule() {
        assert!(is_partition_of("/dev/sda1", "/dev/sda"));
        assert!(is_partition_of("/dev/sda12", "/dev/sda"));
        assert!(!is_partition_of("/dev/sdap", "/dev/sda"));
        assert!(!is_partition_of("/dev/sdap1", "/dev/sda"));
        assert!(is_partition_of("/dev/nvme0n1p2", "/dev/nvme0n1"));
        assert!(!is_partition_of("/dev/nvme0n12", "/dev/nvme0n1"));
        assert!(!is_partition_of("/dev/nvme0n1", "/dev/nvme0n1"));
        assert!(is_partition_of(
            "/dev/disk/by-id/nvme-X-part3",
            "/dev/disk/by-id/nvme-X"
        ));
        assert!(!is_partition_of(
            "/dev/disk/by-id/nvme-X-part",
            "/dev/disk/by-id/nvme-X"
        ));
        let listing = "pool\t-\n\t/dev/sdap\t-\n\t/dev/sda1\t-\n";
        assert!(pool_has_device(listing, Path::new("/dev/sda")));
        assert!(!pool_has_device(
            "pool\t-\n\t/dev/sdap\t-\n",
            Path::new("/dev/sda")
        ));
    }

    #[test]
    fn guest_limit_matches_the_shared_limits_file() {
        let limits: serde_json::Value =
            serde_json::from_str(include_str!("../native-limits.json")).unwrap();
        assert_eq!(limits["max_guests"], u64::from(MAX_GUESTS));
    }
}
