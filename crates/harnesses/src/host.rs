//! Read-only host inventory and whole-disk identity checks.
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs::{self, File};
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::{evidence, process};

pub struct Identity {
    pub machine: String,
    pub kernel: String,
}

pub fn identity() -> io::Result<Identity> {
    // SAFETY: uname initializes the struct and its fields are terminated C strings.
    unsafe {
        let mut name: libc::utsname = std::mem::zeroed();
        if libc::uname(&mut name) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Identity {
            machine: CStr::from_ptr(name.machine.as_ptr())
                .to_string_lossy()
                .into_owned(),
            kernel: CStr::from_ptr(name.release.as_ptr())
                .to_string_lossy()
                .into_owned(),
        })
    }
}

pub fn utc_now() -> io::Result<String> {
    let seconds: libc::time_t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_secs()
        .try_into()
        .map_err(io::Error::other)?;
    // SAFETY: gmtime_r writes to valid storage; strftime receives a terminated
    // format and the buffer's actual size. Neither retains any of these pointers.
    unsafe {
        let mut time: libc::tm = std::mem::zeroed();
        if libc::gmtime_r(&seconds, &mut time).is_null() {
            return Err(io::Error::last_os_error());
        }
        let mut buffer = [0u8; 32];
        let len = libc::strftime(
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            c"%Y-%m-%dT%H:%M:%SZ".as_ptr(),
            &time,
        );
        if len == 0 {
            return Err(io::Error::other("UTC timestamp exceeded its buffer"));
        }
        Ok(String::from_utf8_lossy(&buffer[..len]).into_owned())
    }
}

pub fn cpu_affinity() -> io::Result<Vec<usize>> {
    // SAFETY: the kernel writes a correctly sized cpu_set_t. CPU_ISSET only
    // reads initialized storage at indexes within CPU_SETSIZE.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of_val(&set), &mut set) != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((0..libc::CPU_SETSIZE as usize)
            .filter(|&cpu| libc::CPU_ISSET(cpu, &set))
            .collect())
    }
}

pub fn preflight(label: &str, output: &Path, checkout: &Path) -> io::Result<()> {
    // Reserve the evidence path before probing; an old inventory is never replaced.
    let file = File::options().write(true).create_new(true).open(output)?;
    let checkout = checkout.canonicalize()?;
    let target = checkout
        .to_str()
        .ok_or_else(|| io::Error::other("checkout path must be UTF-8"))?;
    let commands: [(&str, &[&str]); 14] = [
        ("revision", &["git", "rev-parse", "HEAD"]),
        ("worktree", &["git", "status", "--porcelain"]),
        ("kernel", &["uname", "-a"]),
        ("cpu", &["lscpu", "--json"]),
        (
            "block_devices",
            &[
                "lsblk",
                "--json",
                "--bytes",
                "--output",
                "NAME,TYPE,SIZE,MODEL,ROTA,MOUNTPOINTS,FSTYPE,LOG-SEC,PHY-SEC",
            ],
        ),
        (
            "checkout_filesystem",
            &["findmnt", "--json", "--target", target],
        ),
        (
            "network_links",
            &["ip", "-details", "-json", "link", "show"],
        ),
        ("rustc", &["rustc", "--version", "--verbose"]),
        ("cargo", &["cargo", "--version"]),
        ("fio", &["fio", "--version"]),
        ("qemu_x86_64", &["qemu-system-x86_64", "--version"]),
        ("qemu_aarch64", &["qemu-system-aarch64", "--version"]),
        ("qemu_img", &["qemu-img", "--version"]),
        ("zfs", &["zfs", "--version"]),
    ];
    let mut captured = BTreeMap::new();
    for (name, command) in commands {
        process::check_interrupt()?;
        captured.insert(name, process::capture(command, &checkout));
    }
    let mut paths = vec![
        PathBuf::from("/proc/meminfo"),
        PathBuf::from("/sys/kernel/mm/transparent_hugepage/enabled"),
        PathBuf::from("/sys/devices/system/node/online"),
        PathBuf::from("/sys/class/dmi/id/bios_vendor"),
        PathBuf::from("/sys/class/dmi/id/bios_version"),
        PathBuf::from("/sys/class/dmi/id/bios_date"),
    ];
    let mut governors: Vec<_> = fs::read_dir("/sys/devices/system/cpu")?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("cpufreq/scaling_governor"))
        .filter(|path| path.exists())
        .collect();
    governors.sort();
    paths.extend(governors);
    let settings: Vec<Value> = paths
        .into_iter()
        .map(|path| match fs::read_to_string(&path) {
            Ok(value) => json!({"path":path, "status":"ok", "value":value.trim()}),
            Err(error) => json!({"path":path, "status":"unavailable", "error":error.to_string()}),
        })
        .collect();
    evidence::write_json_to(
        file,
        &json!({
            "schema_version":1, "artifact":"environment_inventory", "captured_at_utc":utc_now()?,
            "label":label, "machine":identity()?.machine, "harness_version":env!("CARGO_PKG_VERSION"),
            "commands":captured, "settings":settings, "paper_gate":null,
        }),
    )
}

fn stable_disk_path(path: &Path) -> io::Result<()> {
    if path.parent() != Some(Path::new("/dev/disk/by-id")) {
        return Err(io::Error::other(format!(
            "use a stable /dev/disk/by-id path: {}",
            path.display()
        )));
    }
    Ok(())
}

struct DiskIdentity {
    device: u64,
    block_device: bool,
    sysfs_exists: bool,
    partition: bool,
}

impl DiskIdentity {
    fn validate(self, path: &Path) -> io::Result<u64> {
        if !self.block_device {
            return Err(io::Error::other(format!(
                "not a block device: {}",
                path.display()
            )));
        }
        if !self.sysfs_exists || self.partition {
            return Err(io::Error::other(format!(
                "not a whole disk: {}",
                path.display()
            )));
        }
        Ok(self.device)
    }
}

fn disk_identity(path: &Path) -> io::Result<u64> {
    stable_disk_path(path)?;
    let meta = path.metadata()?; // Follow by-id aliases to the actual device.
    let device = meta.rdev();
    let sysfs = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(device),
        libc::minor(device)
    ));
    DiskIdentity {
        device,
        block_device: meta.file_type().is_block_device(),
        sysfs_exists: sysfs.exists(),
        partition: sysfs.join("partition").exists(),
    }
    .validate(path)
}

fn distinct_disks(os: u64, data: u64) -> io::Result<()> {
    if os == data {
        Err(io::Error::other(
            "OS and experiment paths identify the same block device",
        ))
    } else {
        Ok(())
    }
}

pub fn check_disks(os: &Path, data: &Path) -> io::Result<()> {
    distinct_disks(disk_identity(os)?, disk_identity(data)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_aliases_cannot_select_the_same_device_twice() {
        assert!(distinct_disks(libc::makedev(8, 0), libc::makedev(8, 0)).is_err());
        assert!(distinct_disks(libc::makedev(8, 0), libc::makedev(8, 16)).is_ok());
    }

    #[test]
    fn only_stable_whole_block_devices_are_accepted() {
        let path = Path::new("/dev/disk/by-id/os");
        stable_disk_path(path).unwrap();
        assert!(stable_disk_path(Path::new("/dev/sda")).is_err());
        for (block_device, sysfs_exists, partition) in [
            (false, true, false),
            (true, false, false),
            (true, true, true),
        ] {
            assert!(
                DiskIdentity {
                    device: 1,
                    block_device,
                    sysfs_exists,
                    partition
                }
                .validate(path)
                .is_err()
            );
        }
        assert_eq!(
            DiskIdentity {
                device: 1,
                block_device: true,
                sysfs_exists: true,
                partition: false
            }
            .validate(path)
            .unwrap(),
            1
        );
    }

    #[test]
    fn preflight_refuses_existing_evidence_before_probing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inventory.json");
        fs::write(&path, "previous inventory").unwrap();
        assert!(preflight("test", &path, dir.path()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "previous inventory");
    }
}
