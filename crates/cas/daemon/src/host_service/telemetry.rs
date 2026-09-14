//! Bounded snapshots: strict experiments fail closed; operational output can stop.
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
    rotate: Option<PathBuf>,
}

impl Telemetry {
    pub fn new(path: &Path, metadata: &Arc<Budget>, rotate: bool) -> io::Result<Self> {
        let now = Instant::now();
        Ok(Self {
            journal: Journal::new(create_report(path)?, metadata)?,
            started: now,
            next: now,
            rotate: rotate.then(|| path.to_owned()),
        })
    }

    pub fn operational(&self) -> bool {
        self.rotate.is_some()
    }

    fn rotate_if_needed(&mut self) -> io::Result<()> {
        if let Some(path) = &self.rotate
            && (self.journal.samples == SAMPLES || self.journal.bytes > FILE_BYTES - RECORD_BYTES)
        {
            self.journal.writer.flush()?;
            std::fs::rename(path, path.with_extension("previous.jsonl"))?;
            self.journal.writer = create_report(path)?;
            self.journal.samples = 0;
            self.journal.bytes = 0;
        }
        Ok(())
    }

    pub fn sample(
        &mut self,
        runtime: &mut Runtime,
        controls: &[([u8; 16], Control)],
    ) -> io::Result<()> {
        if Instant::now() < self.next {
            return Ok(());
        }
        self.rotate_if_needed()?;
        let sampling = Instant::now();
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
            write!(
                buffer,
                "] ,\"sampling_ns\":{}}}",
                sampling.elapsed().as_nanos()
            )
        })?;
        self.next = Instant::now() + INTERVAL;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_failure_stops_only_the_operational_observer() {
        for operational in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let report = tempfile::tempdir().unwrap();
            let resources = Arc::new(Resources::default());
            let host = crate::local::host::tests::create(root.path(), 1, Arc::clone(&resources));
            let mut runtime = Runtime::Cold(host);
            let mut telemetry = Telemetry::new(
                &report.path().join("live.jsonl"),
                &resources.metadata,
                operational,
            )
            .unwrap();
            telemetry.journal.writer = File::options().write(true).open("/dev/full").unwrap();
            let result = run_services(
                table(0, &resources.metadata).unwrap(),
                &[],
                &mut runtime,
                &resources.metadata,
                Some(telemetry),
            );
            assert_eq!(result.storage.is_ok(), operational);
            assert!(result.telemetry_error.is_some());
            assert_eq!(
                runtime.host().unwrap().unwrap().failure().is_none(),
                operational
            );
            let Runtime::Cold(host) = runtime else {
                unreachable!()
            };
            crate::local::host::tests::shutdown(host);
            assert_eq!(resources.metadata.usage().current.bytes, 0);
        }
    }

    #[test]
    fn long_session_rotates_and_strict_experiment_keeps_its_cap() {
        let directory = tempfile::tempdir().unwrap();
        let budget = crate::local::metadata_budget();
        for rotate in [false, true] {
            let path = directory.path().join(format!("{rotate}.jsonl"));
            let mut telemetry = Telemetry::new(&path, &budget, rotate).unwrap();
            telemetry.journal.record(|b| b.write_all(b"{}")).unwrap();
            telemetry.journal.samples = SAMPLES;
            telemetry.rotate_if_needed().unwrap();
            assert_eq!(
                telemetry
                    .journal
                    .record(|b| b.write_all(b"{\"next\":true}"))
                    .is_ok(),
                rotate
            );
            if rotate {
                assert_eq!(
                    std::fs::read(path.with_extension("previous.jsonl")).unwrap(),
                    b"{}\n"
                );
                assert_eq!(telemetry.journal.samples, 1);
            }
        }
        assert_eq!(budget.usage().current.bytes, 0);
    }

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
