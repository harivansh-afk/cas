//! V2 adapters. The queue thread gathers into its final append allocation.
pub(crate) mod host;
mod pools;
pub(crate) mod pressure;
mod reactor;
mod state;
mod window;
use allocator_api2::vec::Vec as BudgetVec;
use cas_core::budget::channel as mailbox;
use cas_core::budget::{BudgetAllocator, BudgetArc, Queue as BudgetQueue};
use pools::Pools;
pub use state::ImageState;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const IO_DEADLINE: Duration = Duration::from_secs(30);

#[cfg(test)]
use cas_core::append::format::MAX_BATCH_BYTES;
use cas_core::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    append::{
        self, Log,
        format::{Builder, MAX_DESCRIPTORS, RequestId},
    },
    budget::{Amount, Budget, Credits, Share},
};
use vmm_sys_util::eventfd::EventFd;
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK};

use crate::storage::{Completed, CompletionData, Operation, QueueHead};

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
    _read: Option<BudgetArc<Credits>>,
    window: Option<window::Slot>,
    _admission: Option<host::admission::Entry>,
    pub(crate) fair_release: Option<host::fair::Release>,
}

struct Fetched {
    bytes: cas_core::aligned::AlignedBuffer,
    _credits: BudgetArc<Credits>,
}

type Fetches = BudgetArc<cas_core::cache::fills::Registry<Fetched>>;

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
    bulk_submission_wait_ns: u64,
    bulk_submissions: u64,
    append_buffer_allocations: u64,
    append_buffer_bytes: u64,
    append_buffer_ns: u64,
    io_completed: u64,
    peak_awaiting_cqe: usize,
    reordered_appends: u64,
    publication_wait_ns: u64,
    cohort_pause_ns: u64,
}

#[derive(Clone, Copy)]
enum Written {
    Data(usize),
    Zero(usize),
}
impl Written {
    fn payload_bytes(self) -> usize {
        match self {
            Self::Data(bytes) => bytes,
            Self::Zero(_) => 0,
        }
    }
    fn completion(self) -> CompletionData {
        match self {
            Self::Data(bytes) => CompletionData::Write { bytes },
            Self::Zero(bytes) => CompletionData::Zero { bytes },
        }
    }
}
struct Write {
    id: u64,
    data: Written,
    permit: Permit,
}

struct Packing {
    // Drop the buffer before releasing its byte credits.
    builder: Builder,
    credits: Credits,
    writes: BudgetVec<Write, BudgetAllocator>,
}

enum Command {
    Append(Packing),
    Io(Io),
    Pause {
        done: mailbox::Sender<io::Result<append::Status>>,
        _permit: Permit,
    },
    NewAttachment {
        done: mailbox::Sender<io::Result<append::Status>>,
        _permit: Permit,
        rotated: bool,
    },
    Resume,
}

impl Command {
    // The frontend keeps ownership when the worker cannot accept a command.
    // Return a completion for every transferred request, including batch members.
    fn reject(self, error: &str, output: &mut BudgetQueue<Completed>) {
        let mut complete = |id, data, permit| {
            output.push_back(Completed {
                id,
                data,
                result: Err(io::Error::other(error.to_owned())),
                _permit: Some(permit),
            })
        };
        match self {
            Self::Append(Packing {
                builder,
                credits,
                writes,
            }) => {
                drop(builder);
                drop(credits);
                for write in writes {
                    complete(write.id, write.data.completion(), write.permit);
                }
            }
            Self::Io(io) => complete(io.id, io.operation.into(), io.permit),
            Self::Pause { done, _permit } | Self::NewAttachment { done, _permit, .. } => {
                drop(_permit);
                let _ = done.try_send(Err(io::Error::other(error.to_owned())));
            }
            Self::Resume => (),
        }
    }
}

pub(crate) fn reserved_vec<T>(
    capacity: usize,
    metadata: &Arc<Budget>,
) -> io::Result<BudgetVec<T, BudgetAllocator>> {
    let mut values = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(metadata)));
    values
        .try_reserve_exact(capacity)
        .map_err(|_| io::ErrorKind::OutOfMemory)?;
    Ok(values)
}

struct Io {
    id: u64,
    operation: Operation,
    permit: Permit,
    boundary: u64,
}

type Response = (Completed, append::Status);
pub type Health = BudgetArc<state::Gate>;

pub(crate) const IMAGE_REQUEST_LIMIT: usize = pools::IMAGE_REQUESTS + pools::IMAGE_CONTROL;

pub(crate) fn metadata_budget() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 128 * MAX_REQUEST_BYTES,
        requests: 0,
    })
}

pub struct Shared {
    pools: Pools,
    metadata: Arc<Budget>,
    metrics: Mutex<Metrics>,
    pressure: pressure::Counters,
    final_status: Mutex<append::Status>,
    pub health: Health,
    pub injection: OnceLock<crate::fault::Injection>,
    window: Option<BudgetArc<window::Window>>,
    admission: Option<BudgetArc<host::admission::Admission>>,
}

impl Shared {
    pub fn new(status: append::Status) -> io::Result<BudgetArc<Self>> {
        let metadata = metadata_budget();
        Self::with_pools(
            status,
            Pools::new(),
            state::Gate::new(
                ImageState {
                    durable: status.durable,
                    ..ImageState::default()
                },
                None,
                &metadata,
            )?,
            metadata,
            None,
            None,
        )
    }

    fn with_pools(
        status: append::Status,
        pools: Pools,
        health: Health,
        metadata: Arc<Budget>,
        window: Option<BudgetArc<window::Window>>,
        admission: Option<BudgetArc<host::admission::Admission>>,
    ) -> io::Result<BudgetArc<Self>> {
        let budget = Arc::clone(&metadata);
        BudgetArc::try_new(
            Self {
                injection: OnceLock::new(),
                pools,
                metadata,
                metrics: Mutex::new(Metrics::default()),
                pressure: pressure::Counters::default(),
                final_status: Mutex::new(status),
                health,
                window,
                admission,
            },
            &budget,
        )
    }

    pub(crate) fn metadata(&self) -> Arc<Budget> {
        Arc::clone(&self.metadata)
    }

    pub fn hit(&self, point: crate::fault::Point, count: u64) -> io::Result<()> {
        let Some(injection) = self.injection.get() else {
            return Ok(());
        };
        let snapshot = self
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .snapshot();
        injection.hit(point, Some(count), snapshot)
    }

    /// Reference/replay convenience API. Serving uses the explicit decision.
    pub fn reserve(&self, kind: Kind) -> Option<Permit> {
        self.admit(kind).ok()?.ready()
    }

    fn admit(&self, kind: Kind) -> io::Result<pressure::Decision<Permit>> {
        let entry = match &self.admission {
            Some(admission) => {
                if admission.status().failed {
                    return Err(io::Error::other("host admission failed"));
                }
                let Some(entry) = host::admission::Admission::enter(admission) else {
                    return Ok(pressure::Decision::Waiting(
                        self.pressure.denied(pressure::Reason::HostAdmission),
                    ));
                };
                Some(entry)
            }
            None => None,
        };
        self.reserve_with_entry(kind, entry)
    }

    fn reserve_control(&self) -> Option<Permit> {
        self.reserve_with_entry(Kind::Control, None).ok()?.ready()
    }

    fn reserve_with_entry(
        &self,
        kind: Kind,
        entry: Option<host::admission::Entry>,
    ) -> io::Result<pressure::Decision<Permit>> {
        use pressure::{Decision, Reason};
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
            return Ok(Decision::Waiting(
                self.pressure.denied(Reason::RequestCredits),
            ));
        };
        let read = if let Kind::Read(bytes) = kind {
            let Some(credits) = self.pools.read.reserve(Amount {
                bytes: bytes + MAX_REQUEST_BYTES,
                requests: 0,
            }) else {
                return Ok(Decision::Waiting(self.pressure.denied(Reason::ReadCredits)));
            };
            let Ok(owner) = BudgetArc::try_new(credits, &self.metadata) else {
                return Ok(Decision::Waiting(
                    self.pressure.denied(Reason::ReadOwnerAllocation),
                ));
            };
            Some(owner)
        } else {
            None
        };
        let window = match (kind, &self.window) {
            (Kind::Write(bytes), Some(window)) => match window::Window::reserve(window, bytes)? {
                Decision::Ready(slot) => Some(slot),
                Decision::Waiting(reason) => {
                    return Ok(Decision::Waiting(self.pressure.denied(reason)));
                }
            },
            _ => None,
        };
        Ok(Decision::Ready(Permit {
            _request: request,
            _read: read,
            window,
            _admission: entry,
            fair_release: None,
        }))
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
    fair: Option<host::fair::Port>,
    sender: Option<mailbox::Sender<Command>>,
    receiver: Mutex<mailbox::Receiver<Response>>,
    worker: Option<JoinHandle<()>>,
    input_wake: Option<EventFd>,
    execution: Execution,
    admitted: u64,
    packing: Option<Packing>,
    rejected: BudgetQueue<Completed>,
    paused: bool,
    pub shared: BudgetArc<Shared>,
    pub status: append::Status,
}

impl Local {
    pub fn shared_host(&self) -> bool {
        self.shared.window.is_some()
    }
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
        let shared = Shared::new(log.status())?;
        Self::from_log(log, event, execution, shared)
    }

    pub fn from_log(
        log: Log,
        event: &EventFd,
        execution: Execution,
        shared: BudgetArc<Shared>,
    ) -> io::Result<Self> {
        Self::start(log, event, execution, shared, None)
    }

    fn from_host(
        log: Log,
        event: &EventFd,
        shared: BudgetArc<Shared>,
        port: host::Port,
    ) -> io::Result<Self> {
        Self::start(log, event, Execution::Concurrent, shared, Some(port))
    }

    fn start(
        log: Log,
        event: &EventFd,
        execution: Execution,
        shared: BudgetArc<Shared>,
        port: Option<host::Port>,
    ) -> io::Result<Self> {
        let rejected = BudgetQueue::with_capacity(IMAGE_REQUEST_LIMIT, &shared.metadata)?;
        let status = log.status();
        *shared.final_status.lock().expect("status poisoned") = status;
        let (sender, input) = mailbox::bounded(IMAGE_REQUEST_LIMIT + 1, &shared.metadata)?;
        let (output, receiver) = mailbox::bounded(IMAGE_REQUEST_LIMIT, &shared.metadata)?;
        let input_wake = match &port {
            Some(port) => Some(port.wake.try_clone()?),
            None if execution == Execution::Concurrent => {
                Some(EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?)
            }
            None => None,
        };
        if let Some(window) = &shared.window {
            window.bind(
                input_wake
                    .as_ref()
                    .expect("host reactor wake")
                    .try_clone()?,
            )?;
        }
        let fair = port.as_ref().map(host::Port::fair);
        let worker = Worker {
            log,
            output,
            wake: Wake(event.try_clone()?),
            shared: shared.clone(),
            port,
        };
        let builder = thread::Builder::new().name(execution.name().into());
        let worker = match &input_wake {
            Some(wake) => {
                let reactor = reactor::Reactor::new(worker, input, wake.try_clone()?)?;
                builder.spawn(move || reactor.run())?
            }
            None => builder.spawn(move || worker.run(input))?,
        };
        Ok(Self {
            fair,
            sender: Some(sender),
            receiver: Mutex::new(receiver),
            worker: Some(worker),
            input_wake,
            execution,
            admitted: status.published,
            packing: None,
            rejected,
            paused: false,
            shared,
            status,
        })
    }

    fn send(&mut self, command: Command) -> io::Result<()> {
        if let Err(error) = self
            .sender
            .as_ref()
            .expect("live local worker")
            .try_send(command)
        {
            let message = error.to_string();
            let (mailbox::TrySendError::Full(command)
            | mailbox::TrySendError::Disconnected(command)) = error;
            command.reject(&message, &mut self.rejected);
            return Err(io::Error::other(message));
        }
        if let Some(wake) = &self.input_wake {
            notify(wake)?;
        }
        Ok(())
    }

    pub(crate) fn admission_ticket(&self, kind: Kind) -> io::Result<Option<host::fair::Ticket>> {
        self.fair
            .as_ref()
            .map(|fair| fair.ticket(kind))
            .transpose()
            .map(Option::flatten)
    }

    pub fn prepare(&mut self, kind: Kind) -> io::Result<Option<Permit>> {
        self.admit(kind).map(pressure::Decision::ready)
    }

    pub(crate) fn admit(&mut self, kind: Kind) -> io::Result<pressure::Decision<Permit>> {
        if self.paused {
            return Err(io::Error::other("local admission is paused"));
        }
        if !matches!(kind, Kind::Write(_)) {
            self.seal()?;
        }
        let permit = match self.shared.admit(kind)? {
            pressure::Decision::Ready(permit) => permit,
            pressure::Decision::Waiting(reason) => {
                if self.shared.window.is_some() {
                    self.seal()?;
                }
                return Ok(pressure::Decision::Waiting(reason));
            }
        };
        if let Kind::Write(bytes) = kind {
            if self.packing.as_ref().is_some_and(|batch| {
                batch.builder.len() == MAX_DESCRIPTORS
                    || batch.builder.payload_bytes() + bytes
                        > batch.builder.allocation_bytes() - BLOCK_SIZE
            }) {
                self.seal()?;
            }
            if self.packing.is_none() {
                let payload_capacity = if bytes == 0 { 0 } else { MAX_REQUEST_BYTES };
                let allocation_bytes = BLOCK_SIZE + payload_capacity;
                let Some(credits) = self.shared.pools.append.reserve(Amount {
                    bytes: allocation_bytes,
                    requests: 0,
                }) else {
                    self.shared.pressure.record(pressure::Reason::AppendCredits);
                    return Ok(pressure::Decision::Waiting(pressure::Reason::AppendCredits));
                };
                let started = Instant::now();
                let builder = Builder::new(self.status.image_bytes, payload_capacity)
                    .map_err(io::Error::other)?;
                let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
                metrics.append_buffer_allocations += 1;
                metrics.append_buffer_bytes += allocation_bytes as u64;
                metrics.append_buffer_ns +=
                    started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
                drop(metrics);
                assert_eq!(builder.allocation_bytes(), allocation_bytes);
                self.packing = Some(Packing {
                    builder,
                    credits,
                    writes: reserved_vec(MAX_DESCRIPTORS, &self.shared.metadata)?,
                });
            }
            let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
            metrics.admission_retained_peak = metrics
                .admission_retained_peak
                .max(self.shared.pools.append.usage().current.bytes);
        }
        Ok(pressure::Decision::Ready(permit))
    }

    pub fn gather(
        &mut self,
        id: u64,
        head: QueueHead,
        offset: u64,
        length: usize,
        permit: Permit,
        gather: impl FnOnce(&mut [u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.pack(
            id,
            head,
            permit,
            Written::Data(length),
            |builder, identity| builder.write(identity, offset, length, gather),
        )?;
        let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
        metrics.gathered_bytes += length as u64;
        metrics.gather_calls += 1;
        Ok(())
    }

    pub fn zero(
        &mut self,
        id: u64,
        head: QueueHead,
        offset: u64,
        length: usize,
        permit: Permit,
    ) -> io::Result<()> {
        self.pack(
            id,
            head,
            permit,
            Written::Zero(length),
            |builder, identity| builder.zero(identity, offset, length as u64),
        )
    }

    fn pack(
        &mut self,
        id: u64,
        head: QueueHead,
        permit: Permit,
        data: Written,
        encode: impl FnOnce(&mut Builder, RequestId) -> append::format::Result<()>,
    ) -> io::Result<()> {
        if permit
            .window
            .as_ref()
            .is_some_and(|slot| !slot.matches(data.payload_bytes()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "mutation differs from WAL reservation",
            ));
        }
        let batch = self
            .packing
            .as_mut()
            .ok_or_else(|| io::Error::other("mutation without append reservation"))?;
        let serial = id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("operation serial exhausted"))?;
        let mutation = self
            .admitted
            .checked_add(1)
            .ok_or_else(|| io::Error::other("mutation sequence exhausted"))?;
        encode(
            &mut batch.builder,
            RequestId {
                serial,
                attachment: self.status.epoch,
                queue: head.queue,
                head: head.head,
            },
        )
        .map_err(io::Error::other)?;
        self.admitted = mutation;
        batch.writes.push(Write { id, data, permit });
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

    pub fn pause(&mut self, deadline: Instant) -> io::Result<()> {
        if self.paused {
            return Ok(());
        }
        self.seal()?;
        self.barrier(deadline, |done, permit| Command::Pause {
            done,
            _permit: permit,
        })?;
        self.paused = true;
        Ok(())
    }

    pub fn new_attachment(&mut self, deadline: Instant) -> io::Result<append::Status> {
        if !self.paused || self.execution != Execution::Concurrent {
            return Err(io::Error::other("new attachment requires a paused reactor"));
        }
        self.barrier(deadline, |done, permit| Command::NewAttachment {
            done,
            _permit: permit,
            rotated: false,
        })?;
        self.admitted = self.status.published;
        Ok(self.status)
    }

    fn barrier(
        &mut self,
        deadline: Instant,
        command: impl FnOnce(mailbox::Sender<io::Result<append::Status>>, Permit) -> Command,
    ) -> io::Result<()> {
        let permit = self
            .shared
            .reserve_control()
            .ok_or_else(|| io::Error::other("no control credit for storage barrier"))?;
        let (done, completion) = mailbox::bounded(1, &self.shared.metadata)?;
        self.send(command(done, permit))?;
        self.status = completion
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .map_err(|error| {
                io::Error::new(
                    if matches!(error, mailbox::RecvTimeoutError::Timeout) {
                        io::ErrorKind::TimedOut
                    } else {
                        io::ErrorKind::BrokenPipe
                    },
                    format!("storage barrier did not complete: {error}"),
                )
            })??;
        Ok(())
    }

    pub fn resume(&mut self) -> io::Result<()> {
        if self.paused {
            self.send(Command::Resume)?;
            self.paused = false;
        }
        Ok(())
    }

    pub fn enqueue(&mut self, id: u64, operation: Operation, permit: Permit) -> io::Result<()> {
        if matches!(operation, Operation::Write { .. }) {
            return Err(io::Error::other(
                "local writes must gather directly into an append batch",
            ));
        }
        let command = Command::Io(Io {
            id,
            operation,
            permit,
            boundary: self.admitted,
        });
        if self.paused {
            command.reject("local admission is paused", &mut self.rejected);
            return Err(io::Error::other("local admission is paused"));
        }
        if let Err(error) = self.seal() {
            command.reject(&error.to_string(), &mut self.rejected);
            return Err(error);
        }
        self.send(command)
    }

    pub fn receive(&mut self, wait: bool) -> io::Result<Option<Completed>> {
        if let Some(completed) = self.rejected.pop_front() {
            return Ok(Some(completed));
        }
        let receiver = self
            .receiver
            .get_mut()
            .map_err(|_| io::Error::other("local receiver poisoned"))?;
        let response = if wait {
            Some(receiver.recv().map_err(io::Error::other)?)
        } else {
            match receiver.try_recv() {
                Ok(response) => Some(response),
                Err(mailbox::TryRecvError::Empty) => None,
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
            "host_append":self.shared.pools.append.host_usage(), "host_read":self.shared.pools.read.host_usage(),
            "admission_denials": self.shared.pressure.report(),
            "staging_quota": self.shared.window.as_ref().and_then(|window| window.quota()),
            "wal_window": self.shared.window.as_ref().map(|window| window.status()) })
    }

    pub fn name(&self) -> &'static str {
        self.execution.name()
    }

    /// Begin terminal drain without waiting for the worker to join.
    pub(crate) fn close(&mut self) -> io::Result<()> {
        let sealed = self.seal();
        drop(self.sender.take());
        if let Some(wake) = &self.input_wake {
            let _ = wake.write(1);
        }
        sealed
    }

    pub fn stop(&mut self) -> io::Result<()> {
        let sealed = self.close();
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("local IO worker panicked"))?;
        }
        self.rejected.clear();
        let receiver = self
            .receiver
            .get_mut()
            .map_err(|_| io::Error::other("completion receiver poisoned"))?;
        while let Ok((completed, status)) = receiver.try_recv() {
            self.status = status;
            drop(completed);
        }
        sealed
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct Wake(EventFd);

pub(crate) fn notify(event: &EventFd) -> io::Result<()> {
    match event.write(1) {
        // A saturated counter already has a notification for the consumer.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        result => result,
    }
}
impl Drop for Wake {
    fn drop(&mut self) {
        let _ = self.0.write(1);
    }
}

struct Worker {
    log: Log,
    output: mailbox::Sender<Response>,
    wake: Wake,
    shared: BudgetArc<Shared>,
    port: Option<host::Port>,
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
            .try_send((
                Completed {
                    id,
                    data,
                    result,
                    _permit: Some(permit),
                },
                self.log.status(),
            ))
            .map_err(io::Error::other)?;
        notify(&self.wake.0)
    }

    fn run(mut self, input: mailbox::Receiver<Command>) {
        while let Ok(command) = input.recv() {
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
            Command::Pause { done, _permit }
            | Command::NewAttachment {
                done,
                _permit,
                rotated: true,
            } => {
                let status = self.log.status();
                *self.shared.final_status.lock().expect("status poisoned") = status;
                drop(_permit);
                done.try_send(if failed {
                    Err(io::Error::other("local image failed"))
                } else {
                    Ok(status)
                })
                .map_err(io::Error::other)?;
            }
            Command::NewAttachment {
                done,
                _permit,
                rotated: false,
            } => {
                drop(_permit);
                let _ = done.try_send(Err(io::Error::other("attachment did not rotate")));
            }
            Command::Resume => (),
            Command::Append(Packing {
                builder,
                credits,
                writes,
            }) => {
                let address = builder.allocation_address();
                let result = if failed {
                    drop(builder);
                    Err(io::Error::other("local image failed"))
                } else {
                    {
                        let mut metrics = self.shared.metrics.lock().expect("metrics poisoned");
                        metrics.batches_submitted += 1;
                        metrics.encoding_retained_peak = metrics
                            .encoding_retained_peak
                            .max(self.shared.pools.append.usage().current.bytes);
                    }
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
                    self.send(write.id, write.data.completion(), result, write.permit)?;
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
