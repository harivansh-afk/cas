use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

const LIMIT: u64 = 16 * 1024 * 1024;
pub(super) struct Samples {
    output: PathBuf,
    file: File,
    bytes: u64,
}
impl Samples {
    pub fn new(output: &Path) -> io::Result<Self> {
        Ok(Self {
            output: output.to_owned(),
            file: File::options()
                .write(true)
                .create_new(true)
                .open(output.join("memory.jsonl"))?,
            bytes: 0,
        })
    }
    pub fn tick(&mut self, daemons: &[ManagedChild], guests: &[ManagedChild]) -> io::Result<()> {
        let mut processes = Vec::new();
        for (role, children) in [("storage", daemons), ("qemu", guests)] {
            for child in children {
                let path = PathBuf::from(format!("/proc/{}", child.pid()));
                let rollup = match fs::read_to_string(path.join("smaps_rollup")) {
                    Ok(v) => v,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e),
                };
                let kb = |label: &str| -> io::Result<u64> {
                    rollup
                        .lines()
                        .find_map(|l| l.strip_prefix(label))
                        .and_then(|v| v.split_whitespace().next())
                        .and_then(|v| v.parse::<u64>().ok())
                        .map(|v| v * 1024)
                        .ok_or_else(|| io::Error::other("invalid smaps_rollup"))
                };
                processes.push(serde_json::json!({"role":role,"pid":child.pid(),"rss_bytes":kb("Rss:")?,"pss_bytes":kb("Pss:")?,"smaps_rollup":rollup,"stat":fs::read_to_string(path.join("stat"))?,"io":fs::read_to_string(path.join("io"))?}));
            }
        }
        let value = serde_json::json!({"utc":crate::host::utc_now()?,"processes":processes,"scope":"inner storage and QEMU processes; excludes outer VM and host page cache"});
        let mut bytes = serde_json::to_vec(&value)?;
        bytes.push(b'\n');
        if self.bytes + bytes.len() as u64 > LIMIT {
            fs::rename(
                self.output.join("memory.jsonl"),
                self.output.join("memory.previous.jsonl"),
            )?;
            self.file = File::options()
                .write(true)
                .create_new(true)
                .open(self.output.join("memory.jsonl"))?;
            self.bytes = 0;
        }
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        self.bytes += bytes.len() as u64;
        replace_atomically(&self.output.join("memory.json"), &value)?;
        if let Some(value) = last_record(&self.output.join("daemon/telemetry.jsonl"))? {
            replace_atomically(&self.output.join("storage.json"), &value)?;
        }
        Ok(())
    }
}
fn last_record(path: &Path) -> io::Result<Option<serde_json::Value>> {
    let mut file = match File::open(path) {
        Ok(v) => v,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let end = file.metadata()?.len();
    file.seek(SeekFrom::Start(end.saturating_sub(2 * 1024 * 1024)))?;
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024).read_to_end(&mut bytes)?;
    // The writer may be in the middle of its next sample; only consume complete lines.
    let Some(last) = bytes.iter().rposition(|b| *b == b'\n') else {
        return Ok(None);
    };
    let start = bytes[..last]
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |p| p + 1);
    Ok(Some(serde_json::from_slice(&bytes[start..last])?))
}

pub(super) fn outer() -> io::Result<serde_json::Value> {
    let group = fs::read_to_string("/proc/self/cgroup")?;
    let relative = group
        .lines()
        .find_map(|line| line.strip_prefix("0::/"))
        .ok_or_else(|| io::Error::other("cgroup v2 membership unavailable"))?;
    let path = Path::new("/sys/fs/cgroup").join(relative);
    Ok(serde_json::json!({
        "utc":crate::host::utc_now()?,
        "memory_current_bytes":fs::read_to_string(path.join("memory.current"))?.trim().parse::<u64>().map_err(io::Error::other)?,
        "memory_peak_bytes":fs::read_to_string(path.join("memory.peak"))?.trim().parse::<u64>().map_err(io::Error::other)?,
        "memory_events":fs::read_to_string(path.join("memory.events"))?,
        "host_meminfo":fs::read_to_string("/proc/meminfo")?,
        "scope":"whole lab user service including outer QEMU; overlaps inner guest accounting"
    }))
}
