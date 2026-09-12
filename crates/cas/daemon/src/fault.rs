//! One-shot pauses at write boundaries, controlled by the crash-test harness.
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::ValueEnum;
use serde::Serialize;
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Point {
    AfterPrepared,
    AfterActive,
    BeforeSubmit,
    AfterAppendCqe,
    BeforeSync,
    AfterSync,
    AfterReplayAppend,
    BeforeRecoveryFence,
    AfterRecoveryFence,
    AfterStorage,
    AfterStatus,
    AfterUsed,
}

#[derive(Clone, Copy, Serialize)]
pub struct Snapshot {
    pub published: u64,
    pub durable: u64,
}

pub struct Pause {
    point: Point,
    after: NonZeroU64,
    marker: PathBuf,
}

impl Pause {
    pub fn new(point: Point, after: NonZeroU64, marker: PathBuf) -> io::Result<Self> {
        match marker.symlink_metadata() {
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "pause marker already exists",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            point,
            after,
            marker,
        })
    }

    fn publish(&self, snapshot: Option<Snapshot>) -> io::Result<()> {
        #[derive(Serialize)]
        struct Marker {
            schema_version: u32,
            point: Point,
            writes: NonZeroU64,
            #[serde(flatten)]
            snapshot: Option<Snapshot>,
        }

        publish_marker(
            &self.marker,
            &Marker {
                schema_version: 1,
                point: self.point,
                writes: self.after,
                snapshot,
            },
        )
    }
}

pub(crate) fn publish_marker(marker: &std::path::Path, value: &impl Serialize) -> io::Result<()> {
    let parent = marker
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let mut file = NamedTempFile::new_in(parent)?;
    serde_json::to_writer(file.as_file_mut(), value)?;
    file.persist_noclobber(marker)
        .map_err(|error| error.error)?;
    Ok(())
}

/// One pause shared by the frontend, IO reactor and recovery worker.
#[derive(Clone)]
pub struct Injection(Arc<Mutex<Option<Pause>>>);

impl Injection {
    fn take_pause(&self, point: Point, count: Option<u64>) -> Option<Pause> {
        self.0
            .lock()
            .expect("fault injection poisoned")
            .take_if(|pause| {
                pause.point == point
                    && count.is_some_and(|count| match point {
                        Point::AfterAppendCqe
                        | Point::BeforeSync
                        | Point::AfterSync
                        | Point::BeforeRecoveryFence
                        | Point::AfterRecoveryFence => count >= pause.after.get(),
                        _ => count == pause.after.get(),
                    })
            })
    }

    pub fn hit(
        &self,
        point: Point,
        count: Option<u64>,
        snapshot: Option<Snapshot>,
    ) -> io::Result<()> {
        let Some(pause) = self.take_pause(point, count) else {
            return Ok(());
        };
        pause.publish(snapshot)?;
        stop_process()
    }
}

#[derive(Default)]
pub struct Fault {
    injection: Option<Injection>,
    writes: u64,
    snapshot: Option<Snapshot>,
}

impl Fault {
    pub fn new(pause: Option<Pause>) -> Self {
        Self {
            injection: pause.map(|pause| Injection(Arc::new(Mutex::new(Some(pause))))),
            writes: 0,
            snapshot: None,
        }
    }

    pub fn injection(&self) -> Option<Injection> {
        self.injection.clone()
    }

    pub fn set_snapshot(&mut self, snapshot: Option<Snapshot>) {
        self.snapshot = snapshot;
    }

    pub fn upcoming_write(&self) -> u64 {
        self.writes.saturating_add(1)
    }

    pub fn next_write(&mut self) -> u64 {
        self.writes = self.upcoming_write();
        self.writes
    }

    #[cfg(test)]
    fn take_pause(&mut self, point: Point, write: Option<u64>) -> Option<Pause> {
        self.injection.as_ref()?.take_pause(point, write)
    }

    pub fn hit(&mut self, point: Point, write: Option<u64>) -> io::Result<()> {
        self.injection.as_ref().map_or(Ok(()), |injection| {
            injection.hit(point, write, self.snapshot)
        })
    }
}

pub(crate) fn stop_process() -> io::Result<()> {
    // SAFETY: SIGSTOP stops this process. The harness owns its process group
    // and can kill or resume it; no Rust data is accessed by a signal handler.
    if unsafe { libc::raise(libc::SIGSTOP) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_batch_pause_matches_a_covered_prefix_only_once() {
        let directory = tempfile::tempdir().unwrap();
        let fault = Fault::new(Some(
            Pause::new(
                Point::BeforeSync,
                NonZeroU64::new(32).unwrap(),
                directory.path().join("pause.json"),
            )
            .unwrap(),
        ));
        let frontend = fault.injection().unwrap();
        let worker = frontend.clone();
        assert!(frontend.take_pause(Point::BeforeSync, Some(31)).is_none());
        assert!(worker.take_pause(Point::AfterSync, Some(64)).is_none());
        assert!(worker.take_pause(Point::BeforeSync, Some(64)).is_some());
        assert!(frontend.take_pause(Point::BeforeSync, Some(128)).is_none());
    }

    #[test]
    fn pause_matches_one_write_and_one_boundary_once() {
        let directory = tempfile::tempdir().unwrap();
        let mut fault = Fault::new(Some(
            Pause::new(
                Point::AfterStorage,
                NonZeroU64::new(2).unwrap(),
                directory.path().join("pause.json"),
            )
            .unwrap(),
        ));
        assert!(
            fault
                .take_pause(Point::AfterStorage, Some(fault.writes))
                .is_none()
        );
        fault.next_write();
        assert!(
            fault
                .take_pause(Point::AfterStorage, Some(fault.writes))
                .is_none()
        );
        fault.next_write();
        assert!(
            fault
                .take_pause(Point::BeforeSubmit, Some(fault.writes))
                .is_none()
        );
        fault.next_write(); // A newer admission cannot hide write 2's callback.
        assert!(fault.take_pause(Point::AfterStorage, None).is_none());
        assert!(fault.take_pause(Point::AfterStorage, Some(2)).is_some());
        assert!(
            fault
                .take_pause(Point::AfterStorage, Some(fault.writes))
                .is_none()
        );
        fault.next_write();
        assert!(
            fault
                .take_pause(Point::AfterStorage, Some(fault.writes))
                .is_none()
        );
    }

    #[test]
    fn marker_is_typed_and_existing_evidence_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pause.json");
        let pause = Pause::new(
            Point::AfterStatus,
            NonZeroU64::new(32).unwrap(),
            path.clone(),
        )
        .unwrap();
        pause.publish(None).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"schema_version": 1, "point": "after-status", "writes": 32})
        );
        assert_eq!(
            pause.publish(None).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
