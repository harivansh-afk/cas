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
    /// New storage directory under an already-mounted XFS filesystem.
    #[arg(long)]
    storage: PathBuf,
    /// Block device backing that mount; identity is checked before writing files.
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
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
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
    evidence::require(
        parent.metadata()?.dev() == metadata.rdev(),
        "storage filesystem does not match --device",
    )?;
    let sysfs = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(metadata.rdev()),
        libc::minor(metadata.rdev())
    ));
    evidence::require(
        args.allow_loop_device || !sysfs.join("loop").exists(),
        "loop devices require --allow-loop-device and are development evidence",
    )?;
    let fs_type = std::process::Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "-T"])
        .arg(&parent)
        .output()?;
    evidence::require(
        fs_type.status.success() && fs_type.stdout.trim_ascii() == b"xfs",
        "native storage needs an existing XFS mount",
    )?;
    fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/kvm")?;
    Ok(device)
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
    evidence::write_json(
        &args.output.join("identity.json"),
        &serde_json::json!({
            "source_revision":build.source_revision, "source_path":build.source_path,
            "started_at":host::utc_now()?, "cpu_affinity":host::cpu_affinity()?,
            "host_kernel":host::identity()?.kernel, "device":device,
            "workload_blake3":blake3::hash(&fs::read(&args.script)?).to_hex().to_string(),
            "paper_gates":[], "native_media":!args.allow_loop_device,
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
    fn malformed_geometry_and_qemu_paths_are_refused_before_device_access() {
        let mut args = args(&["--image-bytes", "4097"]);
        assert!(validate(&args).unwrap_err().to_string().contains("aligned"));
        args.output = "bad,path".into();
        assert!(validate(&args).unwrap_err().to_string().contains("commas"));
    }
}
