//! Own storage exclusion before GET/SET_INFLIGHT_FD chooses fresh or live recovery.
use crate::deadline::{Deadline, RECOVERY_TIMEOUT};
use crate::local::{self, Shared};
use cas_core::append::{self, Config, Log, Recovery, Status};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

enum Source {
    Created(Log),
    Existing(Recovery),
}

pub struct Opening {
    source: Option<Source>,
    pub config: Config,
    pub status: Status,
    pub shared: Arc<Shared>,
    pub deadline: Deadline,
}

impl Opening {
    pub fn new(path: &Path, create_bytes: Option<u64>) -> io::Result<Self> {
        Self::with_deadline(path, create_bytes, Deadline::after(RECOVERY_TIMEOUT))
    }

    fn with_deadline(
        path: &Path,
        create_bytes: Option<u64>,
        deadline: Deadline,
    ) -> io::Result<Self> {
        let path = path.to_owned();
        let (source, config, status) = deadline.run(move || match create_bytes {
            Some(bytes) => {
                let log = local::create_log(&path, bytes)?;
                let values = (log.config(), log.status());
                Ok((Source::Created(log), values.0, values.1))
            }
            None => loop {
                deadline.check()?;
                match Log::inspect(&path, append::Limits::default()) {
                    Ok(inspected) => {
                        let values = (inspected.config(), inspected.status());
                        break Ok((Source::Existing(inspected), values.0, values.1));
                    }
                    Err(append::Error::Io(error)) if error.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(deadline.remaining()?.min(Duration::from_millis(10)));
                    }
                    Err(error) => break Err(io::Error::other(error)),
                }
            },
        })?;
        Ok(Self {
            source: Some(source),
            config,
            status,
            shared: Shared::new(status),
            deadline,
        })
    }

    pub fn fresh(&mut self) -> io::Result<Log> {
        let source = self
            .source
            .take()
            .ok_or_else(|| io::Error::other("storage opening already consumed"))?;
        self.deadline.run(move || match source {
            Source::Created(log) => Ok(log),
            Source::Existing(inspected) => inspected.fresh(0).map_err(io::Error::other),
        })
    }

    pub fn take_inspection(&mut self) -> io::Result<Recovery> {
        if !matches!(self.source, Some(Source::Existing(_))) {
            return Err(io::Error::other(
                "retained attachment requires an existing image",
            ));
        }
        let Some(Source::Existing(inspected)) = self.source.take() else {
            unreachable!()
        };
        Ok(inspected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::sync::mpsc;

    #[test]
    fn inspection_retries_actual_segment_contention_until_release_or_deadline() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("log");
        drop(local::create_log(&path, 4096).unwrap());
        let holder = File::open(path.join("segment-00000000000000000001.v2")).unwrap();
        holder.try_lock().unwrap();
        let deadline = Deadline::after(Duration::from_millis(40));
        assert_eq!(
            Opening::with_deadline(&path, None, deadline)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::TimedOut
        );
        let (done, result) = mpsc::channel();
        let task_path = path.clone();
        let waiter = std::thread::spawn(move || {
            done.send(Opening::with_deadline(
                &task_path,
                None,
                Deadline::after(Duration::from_secs(2)),
            ))
            .unwrap();
        });
        assert!(matches!(
            result.recv_timeout(Duration::from_millis(30)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(holder);
        let opened = result
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap();
        assert_eq!(opened.status.published, 0);
        waiter.join().unwrap();
        drop(opened);
        // Corrupt headers are not treated as lock contention or repaired.
        let segment = path.join("segment-00000000000000000001.v2");
        std::fs::write(&segment, [0u8; 4096]).unwrap();
        let deadline = Deadline::after(Duration::from_secs(2));
        let error = Opening::with_deadline(&path, None, deadline).err().unwrap();
        assert_ne!(error.kind(), io::ErrorKind::TimedOut);
        deadline.check().unwrap();
        assert_eq!(std::fs::read(segment).unwrap(), [0; 4096]);
    }
}
