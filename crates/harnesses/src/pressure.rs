//! Fixed development workload inventory and the guest/controller file protocol.
use crate::{evidence, process};
use serde::{Deserialize, Serialize};
use std::{fs, io, path::Path, time::Duration};

mod guest;
pub use guest::{Args, run};
pub const SET_BYTES: u64 = 8 * 1024 * 1024;
pub const WRITE_BYTES: u64 = 16 * 1024 * 1024;
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Stage {
    Shared,
    Disjoint,
    ScanHot,
    Displace,
    SharedMiss,
    Write4kQ1,
    Write4kQ32,
    Write1mQ1,
    Write1mQ32,
    Calibration,
    Burst,
}
impl Stage {
    pub const ALL: [Self; 11] = [
        Self::Shared,
        Self::Disjoint,
        Self::ScanHot,
        Self::Displace,
        Self::SharedMiss,
        Self::Write4kQ1,
        Self::Write4kQ32,
        Self::Write1mQ1,
        Self::Write1mQ32,
        Self::Calibration,
        Self::Burst,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Disjoint => "disjoint",
            Self::ScanHot => "scan-hot",
            Self::Displace => "displace",
            Self::SharedMiss => "shared-miss",
            Self::Write4kQ1 => "write-4k-q1",
            Self::Write4kQ32 => "write-4k-q32",
            Self::Write1mQ1 => "write-1m-q1",
            Self::Write1mQ32 => "write-1m-q32",
            Self::Calibration => "calibration",
            Self::Burst => "burst",
        }
    }
    pub fn fio(self) -> Option<FioControl> {
        let (block_bytes, depth) = match self {
            Self::Write4kQ1 => (4096, 1),
            Self::Write4kQ32 => (4096, 32),
            Self::Write1mQ1 => (1024 * 1024, 1),
            Self::Write1mQ32 => (1024 * 1024, 32),
            _ => return None,
        };
        Some(FioControl {
            block_bytes,
            depth,
            size_bytes: if block_bytes == 4096 {
                SET_BYTES
            } else {
                64 * 1024 * 1024
            },
        })
    }
}
pub struct FioControl {
    pub block_bytes: u64,
    pub depth: u32,
    pub size_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub stage: Stage,
    pub rate_bytes_per_second: Option<u64>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Completed {
    pub stage: Stage,
    pub image: u8,
    pub bytes: u64,
    pub elapsed_ns: u64,
    pub read_latency: Option<Latency>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Latency {
    pub count: u64,
    pub total_ns: u64,
    pub maximum_ns: u64,
    /// Bucket i contains latencies in [2^i, 2^(i+1)) microseconds; bucket 0 also includes <1 us.
    pub log2_microseconds: [u64; 32],
}
impl Latency {
    fn record(&mut self, elapsed: Duration) {
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let bucket = (ns / 1000).max(1).ilog2().min(31) as usize;
        self.count += 1;
        self.total_ns += ns;
        self.maximum_ns = self.maximum_ns.max(ns);
        self.log2_microseconds[bucket] += 1;
    }
}

/// Publish complete JSON at once: an observed filename never denotes a partial write.
pub fn publish(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let temporary = path.with_extension("pending");
    evidence::write_json(&temporary, value)?;
    if path.exists() {
        return Err(io::Error::other("pressure evidence already exists"));
    }
    fs::rename(temporary, path)
}
pub fn wait(path: &Path) -> io::Result<()> {
    let deadline = std::time::Instant::now() + COMMAND_TIMEOUT;
    while !path.exists() {
        process::check_interrupt()?;
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("pressure barrier: {}", path.display()),
            ));
        }
        std::thread::sleep(process::POLL);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_fio_control_can_fill_two_requested_queue_windows() {
        for control in Stage::ALL.into_iter().filter_map(Stage::fio) {
            assert!(control.size_bytes >= 2 * control.block_bytes * u64::from(control.depth));
            assert!(control.size_bytes.is_multiple_of(control.block_bytes));
        }
    }
}
