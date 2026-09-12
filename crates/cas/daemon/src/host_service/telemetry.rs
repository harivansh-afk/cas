//! Bounded, opt-in snapshots; output failure follows normal supervisor teardown.
use super::*;
use std::io::Write;

const INTERVAL: Duration = Duration::from_millis(500);
const RECORD_BYTES: usize = 1024 * 1024;
const FILE_BYTES: usize = 64 * RECORD_BYTES;
const SAMPLES: usize = 4096;

struct Buffer(BudgetVec<u8, BudgetAllocator>);

impl Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > RECORD_BYTES - self.0.len() {
            return Err(io::Error::other("telemetry record exceeds 1 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct Journal<W> {
    writer: W,
    buffer: Buffer,
    samples: usize,
    bytes: usize,
}

impl<W: Write> Journal<W> {
    fn new(writer: W, metadata: &Arc<Budget>) -> io::Result<Self> {
        Ok(Self {
            writer,
            buffer: Buffer(table(RECORD_BYTES, metadata)?),
            samples: 0,
            bytes: 0,
        })
    }

    fn record(&mut self, serialize: impl FnOnce(&mut Buffer) -> io::Result<()>) -> io::Result<()> {
        if self.samples == SAMPLES {
            return Err(io::Error::other("telemetry sample limit reached"));
        }
        self.buffer.0.clear();
        serialize(&mut self.buffer)?;
        self.buffer.write_all(b"\n")?;
        if self.buffer.0.len() > FILE_BYTES - self.bytes {
            return Err(io::Error::other("telemetry file limit reached"));
        }
        self.writer.write_all(&self.buffer.0)?;
        self.writer.flush()?;
        self.bytes += self.buffer.0.len();
        self.samples += 1;
        Ok(())
    }
}

pub(super) struct Telemetry {
    journal: Journal<File>,
    started: Instant,
    next: Instant,
}

impl Telemetry {
    pub fn new(path: &Path, metadata: &Arc<Budget>) -> io::Result<Self> {
        let now = Instant::now();
        Ok(Self {
            journal: Journal::new(create_report(path)?, metadata)?,
            started: now,
            next: now,
        })
    }

    pub fn sample(
        &mut self,
        runtime: &mut Runtime,
        controls: &[([u8; 16], Control)],
    ) -> io::Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        let elapsed = self.started.elapsed().as_nanos();
        self.journal.record(|buffer| {
            write!(
                buffer,
                "{{\"schema_version\":1,\"phase\":\"live\",\"elapsed_ns\":{elapsed},\"host\":"
            )?;
            serde_json::to_writer(&mut *buffer, &runtime.host()?.map(|host| host.report()))?;
            buffer.write_all(b",\"images\":[")?;
            for (index, (image, control)) in controls.iter().enumerate() {
                if index != 0 {
                    buffer.write_all(b",")?;
                }
                write!(
                    buffer,
                    "{{\"image\":\"{:032x}\",\"report\":",
                    u128::from_be_bytes(*image)
                )?;
                serde_json::to_writer(&mut *buffer, &control.snapshot()?)?;
                buffer.write_all(b"}")?;
            }
            buffer.write_all(b"]}")
        })?;
        self.next = Instant::now() + INTERVAL;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_reject_a_record_before_extending_the_output() {
        let metadata = crate::local::metadata_budget();
        let mut journal = Journal::new(Vec::new(), &metadata).unwrap();
        journal.record(|buffer| buffer.write_all(b"{} ")).unwrap();
        assert_eq!(journal.writer, b"{} \n");
        journal.bytes = FILE_BYTES;
        assert!(journal.record(|buffer| buffer.write_all(b"{}")).is_err());
        assert_eq!(journal.writer, b"{} \n");
        journal.bytes = 4;
        journal.samples = SAMPLES;
        assert!(
            journal
                .record(|_| panic!("serialize past sample cap"))
                .is_err()
        );
        assert_eq!(journal.writer, b"{} \n");
        drop(journal);
        assert_eq!(metadata.usage().current.bytes, 0);
    }

    #[test]
    fn record_overflow_and_output_failure_release_charged_scratch() {
        let metadata = crate::local::metadata_budget();
        let mut journal = Journal::new(io::sink(), &metadata).unwrap();
        let payload = vec![0; RECORD_BYTES];
        assert!(journal.record(|buffer| buffer.write_all(&payload)).is_err());
        assert_eq!(journal.samples, 0);
        drop(journal);
        let full = File::options().write(true).open("/dev/full").unwrap();
        let mut journal = Journal::new(full, &metadata).unwrap();
        assert!(journal.record(|buffer| buffer.write_all(b"{}")).is_err());
        assert_eq!(journal.samples, 0);
        drop(journal);
        assert_eq!(metadata.usage().current.bytes, 0);
    }
}
