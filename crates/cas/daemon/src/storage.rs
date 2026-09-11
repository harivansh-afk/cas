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

mod opening;
pub use opening::Opening;

use crate::local::{self, Local};
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    staging::{StagingLog, Status},
};
use io_uring::{IoUring, opcode, squeue, types};
use vmm_sys_util::eventfd::EventFd;

pub enum Permit {
    Reference,
    Local { _credits: local::Permit },
}

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
    pub data: CompletionData,
    pub result: io::Result<()>,
    // Owned data is destroyed before its admission and byte credits.
    pub _permit: Option<local::Permit>,
}

pub enum CompletionData {
    Read(AlignedBuffer),
    Write { bytes: usize },
    Flush,
}

impl From<Operation> for CompletionData {
    fn from(operation: Operation) -> Self {
        match operation {
            Operation::Read { buffer, .. } => Self::Read(buffer),
            Operation::Write { buffer, .. } => Self::Write {
                bytes: buffer.as_slice().len(),
            },
            Operation::Flush => Self::Flush,
        }
    }
}

pub enum Storage {
    Raw(Raw),
    Staging(Staging),
    Local(Box<Local>),
    Opening(Box<Opening>),
}

impl Storage {
    pub fn local(path: &Path, create_bytes: Option<u64>, event: &EventFd) -> io::Result<Self> {
        Ok(Self::Local(Box::new(Local::open(
            path,
            create_bytes,
            event,
        )?)))
    }

    pub fn local_async(
        path: &Path,
        create_bytes: Option<u64>,
        event: &EventFd,
    ) -> io::Result<Self> {
        Ok(Self::Local(Box::new(Local::open_with_execution(
            path,
            create_bytes,
            event,
            local::Execution::Concurrent,
        )?)))
    }

    pub fn local_live(path: &Path, create_bytes: Option<u64>) -> io::Result<Self> {
        Ok(Self::Opening(Box::new(Opening::new(path, create_bytes)?)))
    }

    pub fn prepare(&mut self, kind: local::Kind) -> io::Result<Option<Permit>> {
        match self {
            Self::Opening(_) => Err(io::Error::other("IO before inflight recovery")),
            Self::Local(local) => local
                .prepare(kind)
                .map(|permit| permit.map(|credits| Permit::Local { _credits: credits })),
            _ => Ok(Some(Permit::Reference)),
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Self::Local(_) | Self::Opening(_))
    }

    pub fn completion_gate(&self) -> Option<local::Health> {
        match self {
            Self::Local(local) => Some(std::sync::Arc::clone(&local.shared.health)),
            Self::Opening(opening) => Some(std::sync::Arc::clone(&opening.shared.health)),
            _ => None,
        }
    }

    pub fn gather(
        &mut self,
        id: u64,
        head: u16,
        offset: u64,
        length: usize,
        permit: Permit,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        match self {
            Self::Local(local) => match permit {
                Permit::Local { _credits } => {
                    local.gather(id, head, offset, length, _credits, gather)
                }
                Permit::Reference => Err(io::Error::other("local write without admission credits")),
            },
            _ => Err(io::Error::other("packed gather requires local storage")),
        }
    }

    pub fn local_report(&self) -> Option<serde_json::Value> {
        if let Self::Local(local) = self {
            Some(local.report())
        } else {
            None
        }
    }

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

    #[cfg(test)]
    pub fn staging(
        path: &Path,
        create_bytes: Option<u64>,
        event: &EventFd,
        capacity: usize,
    ) -> io::Result<Self> {
        Self::staging_with_durability(path, create_bytes, event, capacity, false)
    }

    pub fn staging_with_durability(
        path: &Path,
        create_bytes: Option<u64>,
        event: &EventFd,
        capacity: usize,
        durable_writes: bool,
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
                        operation
                            .execute(&mut log)
                            .and_then(|()| {
                                if durable_writes && matches!(operation, Operation::Write { .. }) {
                                    log.flush()?;
                                }
                                Ok(())
                            })
                            .map_err(io::Error::other)
                    };
                    failed |= result.is_err();
                    if output
                        .sender
                        .send((
                            Completed {
                                id,
                                data: operation.into(),
                                result,
                                _permit: None,
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
                // Closing the channel does not flush. Ordinary staging fences
                // on guest FLUSH; restartable staging also fences every write.
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
            Self::Local(local) => local.name(),
            Self::Opening(_) => local::Execution::Concurrent.name(),
        }
    }

    pub fn status(&self) -> Option<Status> {
        match self {
            Self::Raw(_) => None,
            Self::Staging(staging) => Some(staging.status),
            Self::Local(_) | Self::Opening(_) => None,
        }
    }

    pub fn image_bytes(&self) -> u64 {
        match self {
            Self::Raw(raw) => raw.image_bytes,
            Self::Staging(staging) => staging.status.image_bytes,
            Self::Local(local) => local.status.image_bytes,
            Self::Opening(opening) => opening.config.image_bytes,
        }
    }

    #[cfg(test)]
    pub fn enqueue(&mut self, id: u64, operation: Operation) -> io::Result<()> {
        self.enqueue_owned(id, operation, Permit::Reference)
    }

    pub fn enqueue_owned(
        &mut self,
        id: u64,
        operation: Operation,
        permit: Permit,
    ) -> io::Result<()> {
        match self {
            Self::Raw(raw) => raw.enqueue(id, operation),
            Self::Opening(_) => Err(io::Error::other("IO before inflight recovery")),
            Self::Local(local) => match permit {
                Permit::Local { _credits } => local.enqueue(id, operation, _credits),
                Permit::Reference => Err(io::Error::other("local IO without admission credits")),
            },
            Self::Staging(staging) => staging
                .sender
                .as_ref()
                .expect("live worker")
                .try_send((id, operation))
                .map_err(io::Error::other),
        }
    }

    pub fn submit(&mut self) -> io::Result<()> {
        if let Self::Local(local) = self {
            local.seal()?;
        }
        if let Self::Raw(raw) = self {
            raw.ring.submit()?;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> io::Result<()> {
        if let Self::Local(local) = self {
            local.stop()?;
        }
        Ok(())
    }

    pub fn try_complete(&mut self) -> io::Result<Option<Completed>> {
        match self {
            Self::Raw(raw) => raw.try_complete(),
            Self::Opening(_) => Ok(None),
            Self::Local(local) => local.receive(false),
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
        self.submit()?;
        loop {
            if let Some(completed) = self.try_complete()? {
                return Ok(completed);
            }
            match self {
                Self::Opening(_) => {
                    return Err(io::Error::other("completion before inflight recovery"));
                }
                Self::Local(local) => {
                    return local
                        .receive(true)?
                        .ok_or_else(|| io::Error::other("missing local completion"));
                }
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
            data: operation.into(),
            result,
            _permit: None,
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
