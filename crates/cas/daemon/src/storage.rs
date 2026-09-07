// Owned storage operations. The staging reference runs in submission order on one worker
// Raw IO retains its io_uring baseline and kernel-owned buffers.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::{self, JoinHandle};

use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    staging::{StagingLog, Status},
};
use io_uring::{IoUring, opcode, squeue, types};
use vmm_sys_util::eventfd::EventFd;

pub enum Operation {
    Read { offset: u64, buffer: AlignedBuffer },
    Write { offset: u64, buffer: AlignedBuffer },
    Flush,
}

impl Operation {
    pub fn expected_bytes(&self) -> usize {
        match self {
            Self::Read { buffer, .. } | Self::Write { buffer, .. } => buffer.as_slice().len(),
            Self::Flush => 0,
        }
    }

    fn execute(&mut self, log: &mut StagingLog) -> cas_core::staging::Result<()> {
        match self {
            Self::Read { offset, buffer } => {
                let bytes = log.read(*offset, buffer.as_slice().len())?;
                buffer.as_mut_slice().copy_from_slice(&bytes);
            }
            Self::Write { offset, buffer } => {
                log.write(*offset, buffer.as_slice())?;
            }
            Self::Flush => {
                log.flush()?;
            }
        }
        Ok(())
    }
}

pub struct Completed {
    pub id: u64,
    pub operation: Operation,
    pub result: io::Result<()>,
}

pub enum Storage {
    Raw(Raw),
    Staging(Staging),
}

impl Storage {
    pub fn raw(path: &Path, event: &EventFd, capacity: usize) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() == 0 || !meta.len().is_multiple_of(BLOCK_SIZE as u64) {
            return Err(io::Error::other(
                "image must be a nonempty, block-aligned regular file",
            ));
        }
        file.try_lock().map_err(io::Error::from)?;
        let ring = IoUring::new(capacity as u32)?;
        ring.submitter().register_eventfd(event.as_raw_fd())?;
        Ok(Self::Raw(Raw {
            ring,
            file,
            image_bytes: meta.len(),
            pending: BTreeMap::new(),
        }))
    }

    pub fn staging(
        path: &Path,
        create_bytes: Option<u64>,
        event: &EventFd,
        capacity: usize,
    ) -> io::Result<Self> {
        let mut log = match create_bytes {
            Some(bytes) => StagingLog::create(path, bytes),
            None => StagingLog::open(path),
        }
        .map_err(io::Error::other)?;
        let status = log.status();
        let (sender, requests) = mpsc::sync_channel::<(u64, Operation)>(capacity);
        // Backend bounds total outstanding requests by the virtqueue size, so
        // these completions retain at most that many owned buffers as well.
        let (completed, receiver) = mpsc::channel();
        let event = event.try_clone()?;
        let worker = thread::Builder::new()
            .name("cas-staging".into())
            .spawn(move || {
                let output = WorkerOutput {
                    sender: completed,
                    event: WakeOnDrop(event),
                };
                let mut failed = false;
                for (id, mut operation) in requests {
                    let result = if failed {
                        Err(io::Error::other(
                            "staging worker stopped after an IO failure",
                        ))
                    } else {
                        operation.execute(&mut log).map_err(io::Error::other)
                    };
                    failed |= result.is_err();
                    if output
                        .sender
                        .send((
                            Completed {
                                id,
                                operation,
                                result,
                            },
                            log.status(),
                        ))
                        .is_err()
                    {
                        break;
                    }
                    if output.event.0.write(1).is_err() {
                        break;
                    }
                }
                // Closing the channel does not flush. Only a guest FLUSH creates a fence.
            })?;
        Ok(Self::Staging(Staging {
            sender: Some(sender),
            receiver: Mutex::new(receiver),
            worker: Some(worker),
            status,
        }))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Raw(_) => "raw_io_uring",
            Self::Staging(_) => "staging_sync",
        }
    }

    pub fn status(&self) -> Option<Status> {
        match self {
            Self::Raw(_) => None,
            Self::Staging(staging) => Some(staging.status),
        }
    }

    pub fn image_bytes(&self) -> u64 {
        match self {
            Self::Raw(raw) => raw.image_bytes,
            Self::Staging(staging) => staging.status.image_bytes,
        }
    }

    pub fn enqueue(&mut self, id: u64, operation: Operation) -> io::Result<()> {
        match self {
            Self::Raw(raw) => raw.enqueue(id, operation),
            Self::Staging(staging) => staging
                .sender
                .as_ref()
                .expect("live worker")
                .try_send((id, operation))
                .map_err(io::Error::other),
        }
    }

    pub fn submit(&mut self) -> io::Result<()> {
        if let Self::Raw(raw) = self {
            raw.ring.submit()?;
        }
        Ok(())
    }

    pub fn try_complete(&mut self) -> io::Result<Option<Completed>> {
        match self {
            Self::Raw(raw) => raw.try_complete(),
            Self::Staging(staging) => match staging
                .receiver
                .get_mut()
                .map_err(|_| io::Error::other("completion receiver poisoned"))?
                .try_recv()
            {
                Ok((completed, status)) => {
                    staging.status = status;
                    Ok(Some(completed))
                }
                Err(mpsc::TryRecvError::Empty) => Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err(io::Error::other("staging worker exited"))
                }
            },
        }
    }

    pub fn wait_complete(&mut self) -> io::Result<Completed> {
        loop {
            if let Some(completed) = self.try_complete()? {
                return Ok(completed);
            }
            match self {
                Self::Raw(raw) => match raw.ring.submit_and_wait(1) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    result => {
                        result?;
                    }
                },
                Self::Staging(staging) => {
                    let (completed, status) = staging
                        .receiver
                        .get_mut()
                        .map_err(|_| io::Error::other("completion receiver poisoned"))?
                        .recv()
                        .map_err(io::Error::other)?;
                    staging.status = status;
                    return Ok(completed);
                }
            }
        }
    }
}

pub struct Raw {
    ring: IoUring,
    file: File,
    image_bytes: u64,
    pending: BTreeMap<u64, Operation>,
}

impl Raw {
    fn enqueue(&mut self, id: u64, mut operation: Operation) -> io::Result<()> {
        let fd = types::Fd(self.file.as_raw_fd());
        let entry = match &mut operation {
            Operation::Read { offset, buffer } => opcode::Read::new(
                fd,
                buffer.as_mut_slice().as_mut_ptr(),
                buffer.as_slice().len() as u32,
            )
            .offset(*offset)
            .build(),
            Operation::Write { offset, buffer } => opcode::Write::new(
                fd,
                buffer.as_slice().as_ptr(),
                buffer.as_slice().len() as u32,
            )
            .offset(*offset)
            .build(),
            Operation::Flush => opcode::Fsync::new(fd)
                .flags(types::FsyncFlags::DATASYNC)
                .build()
                .flags(squeue::Flags::IO_DRAIN),
        }
        .user_data(id);
        // SAFETY: the pending map owns the allocation until its CQE is consumed.
        // Drop drains the ring before releasing any buffer or the file.
        unsafe { self.ring.submission().push(&entry) }.map_err(io::Error::other)?;
        self.pending.insert(id, operation);
        Ok(())
    }

    fn try_complete(&mut self) -> io::Result<Option<Completed>> {
        let Some((id, result)) = self
            .ring
            .completion()
            .next()
            .map(|c| (c.user_data(), c.result()))
        else {
            return Ok(None);
        };
        let operation = self
            .pending
            .remove(&id)
            .ok_or_else(|| io::Error::other("unknown IO completion"))?;
        let result = if result < 0 {
            Err(io::Error::from_raw_os_error(-result))
        } else if result as usize != operation.expected_bytes() {
            Err(io::Error::other("short raw IO completion"))
        } else {
            Ok(())
        };
        Ok(Some(Completed {
            id,
            operation,
            result,
        }))
    }
}

impl Drop for Raw {
    fn drop(&mut self) {
        while !self.pending.is_empty() {
            match self.ring.submit_and_wait(1) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // The process retains buffers the kernel may still access.
                    std::mem::forget(std::mem::take(&mut self.pending));
                    return;
                }
                Ok(_) => (),
            }
            for completion in self.ring.completion() {
                self.pending.remove(&completion.user_data());
            }
        }
    }
}

pub struct Staging {
    sender: Option<SyncSender<(u64, Operation)>>,
    // VhostUserBackendMut requires Sync. Access uses &mut self and get_mut;
    // no mutex is taken on the completion path.
    receiver: Mutex<Receiver<(Completed, Status)>>,
    worker: Option<JoinHandle<()>>,
    status: Status,
}

impl Drop for Staging {
    fn drop(&mut self) {
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

// Fields drop in order: disconnect the channel before notifying its consumer.
struct WorkerOutput {
    sender: Sender<(Completed, Status)>,
    event: WakeOnDrop,
}

struct WakeOnDrop(EventFd);
impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        // A worker exit (including unwinding) must wake the queue thread so it
        // can observe channel disconnection instead of stranding guest requests.
        let _ = self.0.write(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cas_core::staging::RECORD_SIZE;
    use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK};

    fn write(offset: u64) -> Operation {
        let mut buffer = AlignedBuffer::new(BLOCK_SIZE);
        buffer.as_mut_slice().fill(0x5a);
        Operation::Write { offset, buffer }
    }

    #[test]
    fn failed_operation_prevents_a_queued_flush_from_committing() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let event = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap();
        let mut storage = Storage::staging(&path, Some(BLOCK_SIZE as u64), &event, 3).unwrap();
        storage.enqueue(0, write(0)).unwrap();
        storage.enqueue(1, write(BLOCK_SIZE as u64)).unwrap();
        storage.enqueue(2, Operation::Flush).unwrap();
        for id in 0..3 {
            let completed = storage.wait_complete().unwrap();
            assert_eq!(completed.id, id);
            assert_eq!(completed.result.is_ok(), id == 0);
        }
        assert_eq!(storage.status().unwrap().durable, 0);
        drop(storage);
        let recovered = StagingLog::open(path).unwrap();
        assert_eq!(recovered.read(0, BLOCK_SIZE).unwrap(), [0; BLOCK_SIZE]);
        assert_eq!(recovered.status().recovered_tail_bytes, RECORD_SIZE as u64);
    }

    #[test]
    fn dropping_a_full_worker_drains_without_creating_a_flush_fence() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.log");
        let event = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC).unwrap();
        let mut storage = Storage::staging(&path, Some(BLOCK_SIZE as u64), &event, 128).unwrap();
        for id in 0..128 {
            storage.enqueue(id, write(0)).unwrap();
        }
        drop(storage);
        let recovered = StagingLog::open(path).unwrap();
        assert_eq!(recovered.status().durable, 0);
        assert_eq!(
            recovered.status().recovered_tail_bytes,
            (128 * RECORD_SIZE) as u64
        );
    }
}
