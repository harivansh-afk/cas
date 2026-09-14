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
            buffer: Buffer(reserved_vec(RECORD_BYTES, metadata)?),
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
        let record = Record {
            schema_version: 1,
            phase: "live",
            elapsed_ns: self.started.elapsed().as_nanos(),
            host: runtime.host()?.map(|host| host.report()),
            images: Images(controls),
            sampling_ns: Sampling(Instant::now()),
        };
        self.journal
            .record(|buffer| serde_json::to_writer(buffer, &record).map_err(io::Error::from))?;
        self.next = Instant::now() + INTERVAL;
        Ok(())
    }
}

/// One line of the journal. Image snapshots are taken while the line is
/// serialized, so `sampling_ns` covers them and their encoding.
#[derive(serde::Serialize)]
struct Record<'a> {
    schema_version: u32,
    phase: &'static str,
    elapsed_ns: u128,
    host: Option<serde_json::Value>,
    images: Images<'a>,
    sampling_ns: Sampling,
}

struct Images<'a>(&'a [([u8; 16], Control)]);
impl serde::Serialize for Images<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::{Error, SerializeSeq};
        let mut images = serializer.serialize_seq(Some(self.0.len()))?;
        for (image, control) in self.0 {
            images.serialize_element(&Image {
                image: ImageId(*image),
                report: control.snapshot().map_err(S::Error::custom)?,
            })?;
        }
        images.end()
    }
}

#[derive(serde::Serialize)]
struct Image {
    image: ImageId,
    report: serde_json::Value,
}

struct ImageId([u8; 16]);
impl serde::Serialize for ImageId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&format_args!("{:032x}", u128::from_be_bytes(self.0)))
    }
}

/// Elapsed time measured when the field is written, after the fields before it.
struct Sampling(Instant);
impl serde::Serialize for Sampling {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u128(self.0.elapsed().as_nanos())
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
                reserved_vec(0, &resources.metadata).unwrap(),
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
    fn sample_writes_one_json_line_with_the_journal_schema() {
        let root = tempfile::tempdir().unwrap();
        let report = tempfile::tempdir().unwrap();
        let resources = Arc::new(Resources::default());
        let host = crate::local::host::tests::create(root.path(), 1, Arc::clone(&resources));
        let mut runtime = Runtime::Cold(host);
        let image = report.path().join("image.raw");
        File::create(&image).unwrap().set_len(4096).unwrap();
        let service = Service::new(
            Backend::new(&image).unwrap(),
            File::create(report.path().join("image.json")).unwrap(),
        )
        .unwrap();
        let controls = [([0xab; 16], service.control().unwrap())];
        let path = report.path().join("live.jsonl");
        let mut telemetry = Telemetry::new(&path, &resources.metadata, false).unwrap();
        telemetry.sample(&mut runtime, &controls).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let (line, rest) = text.split_once('\n').unwrap();
        assert!(rest.is_empty());
        let record: serde_json::Value = serde_json::from_str(line).unwrap();
        let keys: Vec<&str> = record
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            keys,
            [
                "elapsed_ns",
                "host",
                "images",
                "phase",
                "sampling_ns",
                "schema_version"
            ]
        );
        assert_eq!(record["schema_version"], 1);
        assert_eq!(record["phase"], "live");
        assert!(record["host"]["failure"].is_null());
        assert_eq!(record["images"][0]["image"], "ab".repeat(16));
        assert_eq!(record["images"][0]["report"]["backend"], "raw_io_uring");
        assert!(record["sampling_ns"].is_u64());
        drop((controls, service));
        let Runtime::Cold(host) = runtime else {
            unreachable!()
        };
        crate::local::host::tests::shutdown(host);
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
