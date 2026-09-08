//! One-shot pauses at write boundaries, controlled by the crash-test harness.
use std::io;
use std::num::NonZeroU64;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::Serialize;
use tempfile::NamedTempFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Point {
    BeforeSubmit,
    AfterStorage,
    AfterStatus,
    AfterUsed,
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

    fn publish(&self) -> io::Result<()> {
        #[derive(Serialize)]
        struct Marker {
            schema_version: u32,
            point: Point,
            writes: NonZeroU64,
        }

        let parent = self
            .marker
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut file = NamedTempFile::new_in(parent)?;
        serde_json::to_writer(
            file.as_file_mut(),
            &Marker {
                schema_version: 1,
                point: self.point,
                writes: self.after,
            },
        )?;
        file.persist_noclobber(&self.marker)
            .map_err(|error| error.error)?;
        Ok(())
    }
}

#[derive(Default)]
pub struct Fault {
    pause: Option<Pause>,
    writes: u64,
}

impl Fault {
    pub fn new(pause: Option<Pause>) -> Self {
        Self { pause, writes: 0 }
    }

    pub fn next_write(&mut self) {
        self.writes = self.writes.saturating_add(1);
    }

    fn take_pause(&mut self, point: Point) -> Option<Pause> {
        self.pause
            .take_if(|pause| pause.point == point && pause.after.get() == self.writes)
    }

    pub fn hit(&mut self, point: Point) -> io::Result<()> {
        let Some(pause) = self.take_pause(point) else {
            return Ok(());
        };
        pause.publish()?;
        stop_process()
    }
}

fn stop_process() -> io::Result<()> {
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
        assert!(fault.take_pause(Point::AfterStorage).is_none());
        fault.next_write();
        assert!(fault.take_pause(Point::AfterStorage).is_none());
        fault.next_write();
        assert!(fault.take_pause(Point::BeforeSubmit).is_none());
        assert!(fault.take_pause(Point::AfterStorage).is_some());
        assert!(fault.take_pause(Point::AfterStorage).is_none());
        fault.next_write();
        assert!(fault.take_pause(Point::AfterStorage).is_none());
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
        pause.publish().unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            serde_json::json!({"schema_version": 1, "point": "after-status", "writes": 32})
        );
        assert_eq!(
            pause.publish().unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }
}
