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

/// A procfs read that may vanish while a launcher execs; only NotFound is transient.
fn transient<T>(result: io::Result<T>) -> io::Result<Option<T>> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
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
                // A Nix launcher can exec QEMU between these procfs reads, and a
                // short-lived helper can exit between them. Retry that transition
                // on the next pass (still bounded by the deadline below) instead
                // of retaining incomplete argv or failing on a vanished entry.
                let Some(bytes) = transient(fs::read(root.join("cmdline")))? else {
                    continue;
                };
                let argv: Vec<_> = bytes
                    .split(|byte| *byte == 0)
                    .filter(|value| !value.is_empty())
                    .map(|value| String::from_utf8_lossy(value).into_owned())
                    .collect();
                let Some(current) = transient(fs::read_link(root.join("exe")))? else {
                    continue;
                };
                if argv.len() < 2 || current != executable {
                    continue;
                }
                let Some(status) = transient(fs::read_to_string(root.join("status")))? else {
                    continue;
                };
                return evidence::write_json(
                    &output.join("qemu.json"),
                    &serde_json::json!({
                        "pid":pid, "executable":executable, "argv":argv,
                        "process_status":status,
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
