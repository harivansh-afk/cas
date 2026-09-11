//! Synchronous v2 worker. The queue thread gathers into its final append buffer.
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

use crate::storage::{Completed, CompletionData, Operation};

#[derive(Clone, Copy)]
pub enum Kind {
    Read(usize),
    Write(usize),
    Control,
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
}

struct Write {
    id: u64,
    bytes: usize,
}

struct Packing {
    // Drop the buffer before releasing its byte credits.
    builder: Builder,
    credits: Credits,
    writes: Vec<Write>,
}

enum Command {
    Append(Packing),
    Io(u64, Operation),
}

type Response = (Completed, append::Status);
pub type Health = Arc<Mutex<Option<String>>>;

pub struct Local {
    sender: Option<mpsc::SyncSender<Command>>,
    receiver: Mutex<mpsc::Receiver<Response>>,
    worker: Option<JoinHandle<()>>,
    packing: Option<Packing>,
    pools: Arc<Pools>,
    metrics: Arc<Mutex<Metrics>>,
    pub health: Health,
    pub status: append::Status,
}

impl Local {
    pub fn open(path: &Path, create_bytes: Option<u64>, event: &EventFd) -> io::Result<Self> {
        let log = match create_bytes {
            Some(image_bytes) => {
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
            }
            None => Log::open(path, append::Limits::default()),
        }
        .map_err(io::Error::other)?;
        let status = log.status();
        let pools = Arc::new(Pools::new());
        let metrics = Arc::new(Mutex::new(Metrics::default()));
        let health = Arc::new(Mutex::new(None));
        let (sender, input) = mpsc::sync_channel(136);
        let (output, receiver) = mpsc::channel();
        let worker = Worker {
            log,
            output,
            wake: Wake(event.try_clone()?),
            pools: Arc::clone(&pools),
            metrics: Arc::clone(&metrics),
            health: Arc::clone(&health),
        };
        let worker = thread::Builder::new()
            .name("cas-local-sync".into())
            .spawn(move || worker.run(input))?;
        Ok(Self {
            sender: Some(sender),
            receiver: Mutex::new(receiver),
            worker: Some(worker),
            packing: None,
            pools,
            metrics,
            health,
            status,
        })
    }

    fn send(&self, command: Command) -> io::Result<()> {
        self.sender
            .as_ref()
            .expect("live local worker")
            .try_send(command)
            .map_err(io::Error::other)
    }

    pub fn prepare(&mut self, kind: Kind) -> io::Result<Option<Permit>> {
        if !matches!(kind, Kind::Write(_)) {
            self.seal()?;
        }
        let request = match kind {
            Kind::Control => self.pools.control.reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 1,
            }),
            _ => self.pools.requests.reserve(Amount {
                bytes: 0,
                requests: 1,
            }),
        };
        let Some(request) = request else {
            return Ok(None);
        };
        let read = match kind {
            Kind::Read(bytes) => {
                let Some(credit) = self.pools.read.reserve(Amount {
                    bytes: bytes + MAX_REQUEST_BYTES,
                    requests: 0,
                }) else {
                    return Ok(None);
                };
                Some(credit)
            }
            Kind::Write(bytes) => {
                if self.packing.as_ref().is_some_and(|batch| {
                    batch.builder.len() == MAX_DESCRIPTORS
                        || batch.builder.payload_bytes() + bytes > MAX_REQUEST_BYTES
                }) {
                    self.seal()?;
                }
                if self.packing.is_none() {
                    let Some(credits) = self.pools.append.reserve(Amount {
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
                let mut metrics = self.metrics.lock().expect("metrics poisoned");
                metrics.admission_retained_peak = metrics
                    .admission_retained_peak
                    .max(self.pools.append.usage().current.bytes);
                None
            }
            Kind::Control => None,
        };
        Ok(Some(Permit {
            _request: request,
            _read: read,
        }))
    }

    pub fn gather(
        &mut self,
        id: u64,
        head: u16,
        offset: u64,
        length: usize,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        let batch = self
            .packing
            .as_mut()
            .ok_or_else(|| io::Error::other("write without append reservation"))?;
        let serial = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("operation serial exhausted"))?;
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
        batch.writes.push(Write { id, bytes: length });
        let mut metrics = self.metrics.lock().expect("metrics poisoned");
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

    pub fn enqueue(&mut self, id: u64, operation: Operation) -> io::Result<()> {
        if matches!(operation, Operation::Write { .. }) {
            return Err(io::Error::other(
                "local writes must gather directly into an append batch",
            ));
        }
        self.seal()?;
        self.send(Command::Io(id, operation))
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
        serde_json::json!({ "status":self.status, "metrics":*self.metrics.lock().expect("metrics poisoned"),
            "requests":self.pools.requests.usage(), "append":self.pools.append.usage(),
            "read":self.pools.read.usage(), "control":self.pools.control.usage(),
            "host_append":self.pools.append.host_usage(), "host_read":self.pools.read.host_usage() })
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        let _ = self.seal();
        drop(self.sender.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
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
    pools: Arc<Pools>,
    metrics: Arc<Mutex<Metrics>>,
    health: Health,
}

impl Worker {
    fn send(&self, id: u64, data: CompletionData, result: io::Result<()>) -> io::Result<()> {
        if let Err(error) = &result {
            // The frontend holds this same gate through status and used publication.
            let mut failed = self
                .health
                .lock()
                .map_err(|_| io::Error::other("completion gate poisoned"))?;
            if failed.is_none() {
                *failed = Some(error.to_string());
            }
        }
        self.output
            .send((Completed { id, data, result }, self.log.status()))
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
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .is_some();
        match command {
            Command::Append(Packing {
                builder,
                credits,
                writes,
            }) => {
                let address = builder.allocation_address();
                {
                    let mut metrics = self.metrics.lock().expect("metrics poisoned");
                    metrics.batches_submitted += 1;
                    metrics.encoding_retained_peak = metrics
                        .encoding_retained_peak
                        .max(self.pools.append.usage().current.bytes);
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
                            let mut metrics = self.metrics.lock().expect("metrics poisoned");
                            metrics.allocation_identity_checks += 1;
                            metrics.encoded_bytes += batch.bytes().len() as u64;
                            metrics.completion_retained_peak = metrics
                                .completion_retained_peak
                                .max(self.pools.append.usage().current.bytes);
                            Ok(()) // batch is released only after synchronous IO returned.
                        })
                };
                drop(credits);
                self.metrics
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
                    )?;
                }
            }
            Command::Io(id, mut operation) => {
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
                self.send(id, operation.into(), result)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
