//! V2 adapters. The queue thread gathers into its final append allocation.
mod reactor;
mod state;
pub use state::ImageState;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use cas_core::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    append::{
        self, Log,
        format::{Builder, MAX_BATCH_BYTES, MAX_DESCRIPTORS, RequestId},
    },
    budget::{Amount, Budget, Credits, Share},
};
use vmm_sys_util::eventfd::EventFd;
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK};

use crate::storage::{Completed, CompletionData, Operation};

#[derive(Clone, Copy)]
pub enum Kind {
    Read(usize),
    Write(usize),
    Control,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    Synchronous,
    Concurrent,
}

impl Execution {
    pub fn name(self) -> &'static str {
        match self {
            Self::Synchronous => "local_sync",
            Self::Concurrent => "local_async",
        }
    }
}

pub struct Permit {
    _request: Credits,
    _read: Option<Credits>,
}

struct Pools {
    requests: Share,
    append: Share,
    read: Share,
    control: Share,
}

impl Pools {
    fn new() -> Self {
        let share = |host_bytes, host_requests, image_bytes, image_requests| {
            Share::new(
                Budget::new(Amount {
                    bytes: host_bytes,
                    requests: host_requests,
                }),
                Amount {
                    bytes: image_bytes,
                    requests: image_requests,
                },
            )
        };
        Self {
            requests: share(0, 1024, 0, 128),
            append: share(64 * MAX_REQUEST_BYTES, 0, 8 * MAX_REQUEST_BYTES, 0),
            read: share(64 * MAX_REQUEST_BYTES, 0, 8 * MAX_REQUEST_BYTES, 0),
            control: share(256 * 1024, 32, 64 * 1024, 8),
        }
    }
}

#[derive(Default, serde::Serialize)]
struct Metrics {
    gathered_bytes: u64,
    gather_calls: u64,
    batches_submitted: u64,
    allocation_identity_checks: u64,
    allocations_released: u64,
    encoded_bytes: u64,
    admission_retained_peak: usize,
    encoding_retained_peak: usize,
    completion_retained_peak: usize,
    io_queued: u64,
    io_completed: u64,
    peak_awaiting_cqe: usize,
    reordered_appends: u64,
    publication_wait_ns: u64,
    cohort_pause_ns: u64,
}

struct Write {
    id: u64,
    bytes: usize,
    permit: Permit,
}

struct Packing {
    // Drop the buffer before releasing its byte credits.
    builder: Builder,
    credits: Credits,
    writes: Vec<Write>,
}

enum Command {
    Append(Packing),
    Io(Io),
}

struct Io {
    id: u64,
    operation: Operation,
    permit: Permit,
    boundary: u64,
}

type Response = (Completed, append::Status);
pub type Health = Arc<Mutex<ImageState>>;

pub struct Shared {
    pools: Pools,
    metrics: Mutex<Metrics>,
    final_status: Mutex<append::Status>,
    pub health: Health,
}

impl Shared {
    pub fn new(status: append::Status) -> Arc<Self> {
        Arc::new(Self {
            pools: Pools::new(),
            metrics: Mutex::new(Metrics::default()),
            final_status: Mutex::new(status),
            health: Arc::new(Mutex::new(ImageState {
                durable: status.durable,
                ..ImageState::default()
            })),
        })
    }

    pub fn reserve(&self, kind: Kind) -> Option<Permit> {
        let request = match kind {
            Kind::Control => self.pools.control.reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 1,
            }),
            _ => self.pools.requests.reserve(Amount {
                bytes: 0,
                requests: 1,
            }),
        }?;
        let read = if let Kind::Read(bytes) = kind {
            Some(self.pools.read.reserve(Amount {
                bytes: bytes + MAX_REQUEST_BYTES,
                requests: 0,
            })?)
        } else {
            None
        };
        Some(Permit {
            _request: request,
            _read: read,
        })
    }

    pub fn replay_next(
        &self,
        replay: &mut append::LiveRecovery,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let mutation = replay
            .next()
            .ok_or_else(|| io::Error::other("missing replay request"))?;
        let bytes = if mutation.kind == append::format::Kind::Write {
            mutation.length as usize
        } else {
            0
        };
        let _request = self
            .reserve(Kind::Write(bytes))
            .ok_or_else(|| io::Error::other("replay request reserve exhausted"))?;
        let _append = self
            .pools
            .append
            .reserve(Amount {
                bytes: BLOCK_SIZE + bytes,
                requests: 0,
            })
            .ok_or_else(|| io::Error::other("replay append reserve exhausted"))?;
        replay
            .replay_next(gather)
            .map(|_| ())
            .map_err(io::Error::other)
    }

    pub fn finish_replay(&self, replay: append::LiveRecovery) -> io::Result<Log> {
        let _control = self
            .reserve(Kind::Control)
            .ok_or_else(|| io::Error::other("recovery fence reserve exhausted"))?;
        replay.finish().map_err(io::Error::other)
    }
}

pub fn create_log(path: &Path, image_bytes: u64) -> io::Result<Log> {
    let mut identities = [0; 32];
    File::open("/dev/urandom")?.read_exact(&mut identities)?;
    Log::create(
        path,
        append::Config {
            store: identities[..16].try_into().unwrap(),
            image: identities[16..].try_into().unwrap(),
            image_bytes,
            segment_bytes: 64 * MAX_REQUEST_BYTES as u64,
        },
        append::Limits::default(),
    )
    .map_err(io::Error::other)
}

pub struct Local {
    sender: Option<mpsc::SyncSender<Command>>,
    receiver: Mutex<mpsc::Receiver<Response>>,
    worker: Option<JoinHandle<()>>,
    input_wake: Option<EventFd>,
    execution: Execution,
    admitted: u64,
    packing: Option<Packing>,
    pub shared: Arc<Shared>,
    pub status: append::Status,
}

impl Local {
    pub fn open(path: &Path, create_bytes: Option<u64>, event: &EventFd) -> io::Result<Self> {
        Self::open_with_execution(path, create_bytes, event, Execution::Synchronous)
    }

    pub fn open_with_execution(
        path: &Path,
        create_bytes: Option<u64>,
        event: &EventFd,
        execution: Execution,
    ) -> io::Result<Self> {
        let log = match create_bytes {
            Some(image_bytes) => create_log(path, image_bytes),
            None => Log::open(path, append::Limits::default()).map_err(io::Error::other),
        }?;
        let shared = Shared::new(log.status());
        Self::from_log(log, event, execution, shared)
    }

    pub fn from_log(
        log: Log,
        event: &EventFd,
        execution: Execution,
        shared: Arc<Shared>,
    ) -> io::Result<Self> {
        let status = log.status();
        *shared.final_status.lock().expect("status poisoned") = status;
        let (sender, input) = mpsc::sync_channel(136);
        let (output, receiver) = mpsc::channel();
        let worker = Worker {
            log,
            output,
            wake: Wake(event.try_clone()?),
            shared: Arc::clone(&shared),
        };
        let input_wake = if execution == Execution::Concurrent {
            Some(EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?)
        } else {
            None
        };
        let task: Box<dyn FnOnce() + Send> = match &input_wake {
            Some(wake) => {
                let reactor = reactor::Reactor::new(worker, input, wake.try_clone()?)?;
                Box::new(move || reactor.run())
            }
            None => Box::new(move || worker.run(input)),
        };
        let worker = thread::Builder::new()
            .name(execution.name().into())
            .spawn(task)?;
        Ok(Self {
            sender: Some(sender),
            receiver: Mutex::new(receiver),
            worker: Some(worker),
            input_wake,
            execution,
            admitted: status.published,
            packing: None,
            shared,
            status,
        })
    }

    fn send(&self, command: Command) -> io::Result<()> {
        self.sender
            .as_ref()
            .expect("live local worker")
            .try_send(command)
            .map_err(io::Error::other)?;
        if let Some(wake) = &self.input_wake {
            wake.write(1)?;
        }
        Ok(())
    }

    pub fn prepare(&mut self, kind: Kind) -> io::Result<Option<Permit>> {
        if !matches!(kind, Kind::Write(_)) {
            self.seal()?;
        }
        let Some(permit) = self.shared.reserve(kind) else {
            return Ok(None);
        };
        if let Kind::Write(bytes) = kind {
            if self.packing.as_ref().is_some_and(|batch| {
                batch.builder.len() == MAX_DESCRIPTORS
                    || batch.builder.payload_bytes() + bytes > MAX_REQUEST_BYTES
            }) {
                self.seal()?;
            }
            if self.packing.is_none() {
                let Some(credits) = self.shared.pools.append.reserve(Amount {
                    bytes: MAX_BATCH_BYTES,
                    requests: 0,
                }) else {
                    return Ok(None);
                };
                let builder = Builder::new(self.status.image_bytes, MAX_REQUEST_BYTES)
                    .map_err(io::Error::other)?;
                assert_eq!(builder.allocation_bytes(), MAX_BATCH_BYTES);
                self.packing = Some(Packing {
                    builder,
                    credits,
                    writes: Vec::with_capacity(MAX_DESCRIPTORS),
                });
            }
            let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
            metrics.admission_retained_peak = metrics
                .admission_retained_peak
                .max(self.shared.pools.append.usage().current.bytes);
        }
        Ok(Some(permit))
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
        let batch = self
            .packing
            .as_mut()
            .ok_or_else(|| io::Error::other("write without append reservation"))?;
        let serial = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("operation serial exhausted"))?;
        let mutation = self
            .admitted
            .checked_add(1)
            .ok_or_else(|| io::Error::other("mutation sequence exhausted"))?;
        batch
            .builder
            .write(
                RequestId {
                    serial,
                    attachment: self.status.epoch,
                    queue: 0,
                    head,
                },
                offset,
                length,
                gather,
            )
            .map_err(io::Error::other)?;
        self.admitted = mutation;
        batch.writes.push(Write {
            id,
            bytes: length,
            permit,
        });
        let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
        metrics.gathered_bytes += length as u64;
        metrics.gather_calls += 1;
        Ok(())
    }

    pub fn seal(&mut self) -> io::Result<()> {
        if let Some(batch) = self.packing.take()
            && !batch.builder.is_empty()
        {
            self.send(Command::Append(batch))?;
        }
        Ok(())
    }

    pub fn enqueue(&mut self, id: u64, operation: Operation, permit: Permit) -> io::Result<()> {
        if matches!(operation, Operation::Write { .. }) {
            return Err(io::Error::other(
                "local writes must gather directly into an append batch",
            ));
        }
        self.seal()?;
        self.send(Command::Io(Io {
            id,
            operation,
            permit,
            boundary: self.admitted,
        }))
    }

    pub fn receive(&mut self, wait: bool) -> io::Result<Option<Completed>> {
        let receiver = self
            .receiver
            .get_mut()
            .map_err(|_| io::Error::other("local receiver poisoned"))?;
        let response = if wait {
            Some(receiver.recv().map_err(io::Error::other)?)
        } else {
            match receiver.try_recv() {
                Ok(response) => Some(response),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(error) => return Err(io::Error::other(error)),
            }
        };
        Ok(response.map(|(completed, status)| {
            self.status = status;
            completed
        }))
    }

    pub fn report(&self) -> serde_json::Value {
        let status = *self.shared.final_status.lock().expect("status poisoned");
        serde_json::json!({ "status":status, "metrics":*self.shared.metrics.lock().expect("metrics poisoned"),
            "requests":self.shared.pools.requests.usage(), "append":self.shared.pools.append.usage(),
            "read":self.shared.pools.read.usage(), "control":self.shared.pools.control.usage(),
            "host_append":self.shared.pools.append.host_usage(), "host_read":self.shared.pools.read.host_usage() })
    }

    pub fn name(&self) -> &'static str {
        self.execution.name()
    }

    pub fn stop(&mut self) -> io::Result<()> {
        self.seal()?;
        drop(self.sender.take());
        if let Some(wake) = &self.input_wake {
            let _ = wake.write(1);
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("local IO worker panicked"))?;
        }
        let receiver = self
            .receiver
            .get_mut()
            .map_err(|_| io::Error::other("completion receiver poisoned"))?;
        while let Ok((completed, status)) = receiver.try_recv() {
            self.status = status;
            drop(completed);
        }
        Ok(())
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct Wake(EventFd);
impl Drop for Wake {
    fn drop(&mut self) {
        let _ = self.0.write(1);
    }
}

struct Worker {
    log: Log,
    output: mpsc::Sender<Response>,
    wake: Wake,
    shared: Arc<Shared>,
}

impl Drop for Worker {
    fn drop(&mut self) {
        *self.shared.final_status.lock().expect("status poisoned") = self.log.status();
    }
}

impl Worker {
    fn send(
        &self,
        id: u64,
        data: CompletionData,
        result: io::Result<()>,
        permit: Permit,
    ) -> io::Result<()> {
        *self.shared.final_status.lock().expect("status poisoned") = self.log.status();
        if let Err(error) = &result {
            // The frontend holds this same gate through status and used publication.
            let mut failed = self
                .shared
                .health
                .lock()
                .map_err(|_| io::Error::other("completion gate poisoned"))?;
            failed.fail(error.to_string());
        }
        self.output
            .send((
                Completed {
                    id,
                    data,
                    result,
                    _permit: Some(permit),
                },
                self.log.status(),
            ))
            .map_err(io::Error::other)?;
        self.wake.0.write(1)
    }

    fn run(mut self, input: mpsc::Receiver<Command>) {
        for command in input {
            if self.execute(command).is_err() {
                break;
            }
        }
    }

    fn execute(&mut self, command: Command) -> io::Result<()> {
        let failed = self
            .shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .failure
            .is_some();
        match command {
            Command::Append(Packing {
                builder,
                credits,
                writes,
            }) => {
                let address = builder.allocation_address();
                {
                    let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
                    metrics.batches_submitted += 1;
                    metrics.encoding_retained_peak = metrics
                        .encoding_retained_peak
                        .max(self.shared.pools.append.usage().current.bytes);
                }
                let result = if failed {
                    drop(builder);
                    Err(io::Error::other("local image failed"))
                } else {
                    self.log
                        .append(builder)
                        .map_err(io::Error::other)
                        .and_then(|batch| {
                            if batch.allocation_address() != address {
                                return Err(io::Error::other("append allocation changed"));
                            }
                            let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
                            metrics.allocation_identity_checks += 1;
                            metrics.encoded_bytes += batch.bytes().len() as u64;
                            metrics.completion_retained_peak = metrics
                                .completion_retained_peak
                                .max(self.shared.pools.append.usage().current.bytes);
                            Ok(()) // batch is released only after synchronous IO returned.
                        })
                };
                drop(credits);
                self.shared
                    .metrics
                    .lock()
                    .expect("metrics poisoned")
                    .allocations_released += 1;
                for write in writes {
                    let result = result
                        .as_ref()
                        .copied()
                        .map_err(|error| io::Error::other(error.to_string()));
                    self.send(
                        write.id,
                        CompletionData::Write { bytes: write.bytes },
                        result,
                        write.permit,
                    )?;
                }
            }
            Command::Io(Io {
                id,
                mut operation,
                permit,
                ..
            }) => {
                let result = if failed {
                    Err(io::Error::other("local image failed"))
                } else {
                    match &mut operation {
                        Operation::Read { offset, buffer } => self
                            .log
                            .read_into(*offset, buffer)
                            .map_err(io::Error::other),
                        Operation::Flush => self.log.flush().map(|_| ()).map_err(io::Error::other),
                        Operation::Write { .. } => {
                            Err(io::Error::other("unexpected bounced local write"))
                        }
                    }
                };
                self.send(id, operation.into(), result, permit)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
