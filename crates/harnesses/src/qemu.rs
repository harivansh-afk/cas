//! Shared VM path checks and live invocation capture.
use crate::{
    evidence,
    process::{self, ManagedChild},
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub fn valid_path(path: &Path) -> io::Result<()> {
    let value = path
        .to_str()
        .ok_or_else(|| io::Error::other("QEMU paths must be UTF-8"))?;
    if value.contains([',', '\n', '\r']) {
        return Err(io::Error::other(
            "QEMU paths cannot contain commas or line breaks",
        ));
    }
    Ok(())
}

pub fn prepare_output(path: &Path) -> io::Result<PathBuf> {
    let path = std::path::absolute(path)?;
    valid_path(&path)?;
    if path.symlink_metadata().is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "output must be a new directory",
        ));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("output needs a parent directory"))?;
    fs::create_dir_all(parent)?;
    let parent = parent.canonicalize()?;
    valid_path(&parent)?;
    let path = parent.join(
        path.file_name()
            .ok_or_else(|| io::Error::other("invalid output path"))?,
    );
    fs::create_dir(&path)?;
    Ok(path)
}

/// Retain the actual QEMU executable and expanded arguments from its live process.
pub fn record(guest: &mut ManagedChild, output: &Path) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let mut pids = vec![guest.pid()];
        let mut cursor = 0;
        while cursor < pids.len() && cursor < 64 {
            let pid = pids[cursor];
            cursor += 1;
            let root = PathBuf::from(format!("/proc/{pid}"));
            if let Ok(executable) = fs::read_link(root.join("exe"))
                && executable
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains("qemu-system-"))
            {
                let bytes = fs::read(root.join("cmdline"))?;
                let argv: Vec<_> = bytes
                    .split(|byte| *byte == 0)
                    .filter(|value| !value.is_empty())
                    .map(|value| String::from_utf8_lossy(value).into_owned())
                    .collect();
                // A Nix launcher can exec QEMU between these procfs reads.
                // Retry that transition instead of retaining incomplete argv.
                if argv.len() < 2 || fs::read_link(root.join("exe"))? != executable {
                    continue;
                }
                return evidence::write_json(
                    &output.join("qemu.json"),
                    &serde_json::json!({
                        "pid":pid, "executable":executable, "argv":argv,
                        "process_status":fs::read_to_string(root.join("status"))?,
                    }),
                );
            }
            if let Ok(children) = fs::read_to_string(root.join(format!("task/{pid}/children"))) {
                pids.extend(
                    children
                        .split_whitespace()
                        .filter_map(|pid| pid.parse::<u32>().ok()),
                );
            }
        }
        process::check_interrupt()?;
        if guest.poll()?.is_some() || Instant::now() >= deadline {
            return Err(io::Error::other(
                "could not record the running QEMU invocation",
            ));
        }
        thread::sleep(process::POLL);
    }
}
