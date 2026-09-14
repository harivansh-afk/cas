//! One image's io_uring owner. Kernel completion and logical publication are
//! distinct; every pending entry retains its allocation, file and credits.
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use allocator_api2::boxed::Box as BudgetBox;
use cas_core::aligned::AlignedBuffer;
use cas_core::append::{ReadPlan, Submission};
use cas_core::budget::BudgetAllocator;
use io_uring::{IoUring, opcode, squeue, types};

use super::*;

mod slots;
use slots::Slots;

const IDLE_SYNC: Duration = Duration::from_millis(50);
// enqueue() requires a successor token, so this value is never an IO owner.
const CANCEL_CQE: u64 = u64::MAX;

#[cfg(test)]
mod tests;

struct Append {
    submission: Submission,
    allocation_address: usize,
    credits: Credits,
    writes: BudgetVec<Write, BudgetAllocator>,
}

mod read;
use read::Read;

struct Fence {
    submission: Submission,
    _credits: Credits,
    waiters: BudgetVec<Io, BudgetAllocator>,
    syncing: bool,
    rollover: bool,
}

enum Work {
    Append(Append),
    Read(BudgetBox<Read, BudgetAllocator>),
    Fence(Fence),
}

struct Pending {
    work: Work,
    submitted: Option<Instant>,
    queued: Instant,
    ready: Option<Instant>,
    cancel_requested: bool,
}

impl Pending {
    fn dependency(&self) -> bool {
        matches!(&self.work, Work::Read(read) if read.notification_only())
    }

    fn in_kernel(&self) -> bool {
        self.submitted.is_some() && self.ready.is_none()
    }

    fn bulk(&self) -> bool {
        match &self.work {
            Work::Append(_) => true,
            Work::Read(read) => !read.notification_only(),
            Work::Fence(_) => false,
        }
    }

    fn entry(&mut self) -> squeue::Entry {
        match &mut self.work {
            Work::Append(append) => write_entry(&append.submission),
            Work::Read(read) => read.entry(),
            Work::Fence(fence) if fence.syncing => {
                opcode::Fsync::new(types::Fd(fence.submission.file().as_raw_fd()))
                    .flags(types::FsyncFlags::DATASYNC)
                    .build()
            }
            Work::Fence(fence) => write_entry(&fence.submission),
        }
    }

    fn expected_bytes(&self) -> usize {
        match &self.work {
            Work::Append(append) => append.submission.batch().bytes().len(),
            Work::Read(read) => read.expected_bytes(),
            Work::Fence(fence) if fence.syncing => 0,
            Work::Fence(fence) => fence.submission.batch().bytes().len(),
        }
    }
}

fn write_entry(submission: &Submission) -> squeue::Entry {
    opcode::Write::new(
        types::Fd(submission.file().as_raw_fd()),
        submission.batch().bytes().as_ptr(),
        submission.batch().bytes().len() as u32,
    )
    .offset(submission.offset())
    .build()
}

pub(super) struct Reactor {
    // Drop drains the ring before these owners can be destroyed.
    ring: IoUring,
    scheduler: Option<cas_core::scheduler::Port>,
    bulk: BudgetQueue<u64>,
    pending: Slots<Pending>,
    ready: BudgetVec<u64, BudgetAllocator>,
    fence_waiters: Option<BudgetVec<Io, BudgetAllocator>>,
    appends: BudgetQueue<u64>,
    commands: BudgetQueue<Command>,
    reads: BudgetQueue<Io>,
    worker: Worker,
    input: mailbox::Receiver<Command>,
    input_wake: EventFd,
    kernel_wake: EventFd,
    next_token: u64,
    cohort: Option<u64>,
    paused_since: Option<Instant>,
    last_write: Instant,
    last_sync: Instant,
    closed: bool,
    closed_since: Option<Instant>,
    failed: bool,
    admission_paused: bool,
    #[cfg(test)]
    control: Option<Arc<tests::Control>>,
    #[cfg(test)]
    withheld: Option<(u64, i32)>,
}

impl Reactor {
    pub fn new(
        worker: Worker,
        input: mailbox::Receiver<Command>,
        input_wake: EventFd,
    ) -> io::Result<Self> {
        let ring = IoUring::new(256)?;
        let kernel_wake = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
        ring.submitter().register_eventfd(kernel_wake.as_raw_fd())?;
        let capacity = IMAGE_REQUEST_LIMIT + 1;
        let metadata = &worker.shared.metadata;
        let pending = Slots::new(capacity, metadata)?;
        let ready = reserved_vec(capacity, metadata)?;
        let fence_waiters = Some(reserved_vec(pools::IMAGE_CONTROL, metadata)?);
        let appends = BudgetQueue::with_capacity(capacity, metadata)?;
        let commands = BudgetQueue::with_capacity(capacity, metadata)?;
        let reads = BudgetQueue::with_capacity(capacity, metadata)?;
        let bulk = BudgetQueue::with_capacity(capacity, metadata)?;
        let scheduler = worker
            .port
            .as_ref()
            .map(host::Port::io_scheduler)
            .transpose()?;
        Ok(Self {
            ring,
            scheduler,
            bulk,
            pending,
            ready,
            fence_waiters,
            appends,
            commands,
            reads,
            worker,
            input,
            input_wake,
            kernel_wake,
            next_token: 1,
            cohort: None,
            paused_since: None,
            last_write: Instant::now(),
            last_sync: Instant::now(),
            closed: false,
            closed_since: None,
            failed: false,
            admission_paused: false,
            #[cfg(test)]
            control: None,
            #[cfg(test)]
            withheld: None,
        })
    }

    fn fail(&mut self, error: &io::Error) {
        self.failed = true;
        self.worker.log.fail();
        let mut failed = self
            .worker
            .shared
            .health
            .lock()
            .expect("completion gate poisoned");
        failed.fail(error.to_string());
        if let Some(port) = &mut self.worker.port {
            port.abort(error);
        }
        self.appends.clear();
        let _ = self.worker.wake.0.write(1);
    }

    fn failure(&self) -> io::Error {
        io::Error::other(
            self.worker
                .shared
                .health
                .lock()
                .expect("completion gate poisoned")
                .failure
                .as_deref()
                .unwrap_or("local image failed")
                .to_owned(),
        )
    }

    fn send_io(&self, io: Io, result: io::Result<()>) -> io::Result<()> {
        self.worker
            .send(io.id, io.operation.into(), result, io.permit)
    }

    fn finish_append(&self, append: Append, result: io::Result<()>) -> io::Result<()> {
        let Append {
            submission,
            allocation_address,
            credits,
            writes,
        } = append;
        assert_eq!(submission.batch().allocation_address(), allocation_address);
        drop(submission);
        drop(credits);
        {
            let mut metrics = self.worker.shared.metrics.lock().expect("metrics poisoned");
            metrics.allocation_identity_checks += 1;
            metrics.allocations_released += 1;
        }
        for write in writes {
            let result = result
                .as_ref()
                .copied()
                .map_err(|error| io::Error::other(error.to_string()));
            self.worker
                .send(write.id, write.data.completion(), result, write.permit)?;
        }
        Ok(())
    }

    fn reject(&mut self, work: Work) -> io::Result<()> {
        match work {
            Work::Append(append) => self.finish_append(append, Err(self.failure())),
            Work::Read(read) => {
                self.send_io(BudgetBox::into_inner(read).into_io(), Err(self.failure()))
            }
            Work::Fence(fence) => {
                let Fence {
                    submission,
                    _credits,
                    waiters,
                    ..
                } = fence;
                drop(submission);
                drop(_credits);
                self.cohort = None;
                for waiter in waiters {
                    self.send_io(waiter, Err(self.failure()))?;
                }
                Ok(())
            }
        }
    }

    fn push(&mut self, token: u64) -> io::Result<()> {
        let pending = self.pending.get_mut(&token).expect("owned IO before SQE");
        if let Some(scheduler) = &self.scheduler
            && pending.bulk()
        {
            pending.submitted = None;
            pending.queued = Instant::now();
            pending.ready = None;
            scheduler.ready(true)?;
            self.bulk.push_back(token);
            return Ok(());
        }
        self.push_now(token)
    }

    fn submit_bulk(&mut self) -> io::Result<()> {
        while let Some(&token) = self.bulk.front() {
            let bytes = self
                .pending
                .get(&token)
                .expect("queued bulk owner")
                .expected_bytes();
            if !self
                .scheduler
                .as_ref()
                .expect("bulk scheduler")
                .take(bytes, self.bulk.len() > 1)?
            {
                break;
            }
            self.bulk.pop_front();
            if let Err(error) = self.push_now(token) {
                let pending = self.pending.remove(&token).expect("unsubmitted bulk owner");
                self.fail(&error);
                self.reject(pending.work)?;
                return Err(error);
            }
        }
        Ok(())
    }

    fn push_now(&mut self, token: u64) -> io::Result<()> {
        let pending = self.pending.get_mut(&token).expect("owned IO before SQE");
        let entry = pending.entry();
        #[cfg(test)]
        let entry = match &self.control {
            Some(control) => control.entry(&pending.work, entry),
            None => entry,
        };
        let entry = entry.user_data(token);
        // SAFETY: pending owns every referenced buffer and the locked file.
        // No owner is removed until its CQE. Drop drains or retains all owners.
        unsafe { self.ring.submission().push(&entry) }.map_err(io::Error::other)?;
        let now = Instant::now();
        let bulk = self.scheduler.is_some() && pending.bulk();
        let waited = if bulk {
            now.duration_since(pending.queued)
                .as_nanos()
                .min(u64::MAX as u128) as u64
        } else {
            0
        };
        pending.submitted = Some(now);
        pending.ready = None;
        let mut metrics = self.worker.shared.metrics.lock().expect("metrics poisoned");
        metrics.io_queued += 1;
        metrics.bulk_submission_wait_ns += waited;
        metrics.bulk_submissions += u64::from(bulk);
        metrics.peak_awaiting_cqe = metrics
            .peak_awaiting_cqe
            .max(self.pending.values().filter(|p| p.in_kernel()).count());
        Ok(())
    }

    fn enqueue(&mut self, work: Work) -> io::Result<Option<u64>> {
        let Some((token, next)) = self
            .pending
            .next(self.next_token)
            .and_then(|token| token.checked_add(1).map(|next| (token, next)))
        else {
            self.fail(&io::Error::other("IO token or slot capacity exhausted"));
            self.reject(work)?;
            return Ok(None);
        };
        self.next_token = next;
        self.pending.insert(
            token,
            Pending {
                work,
                submitted: None,
                queued: Instant::now(),
                ready: Some(Instant::now()),
                cancel_requested: false,
            },
        );
        if let Err(error) = self.push(token) {
            // push did not publish this SQE, so its allocation has no kernel owner.
            let pending = self.pending.remove(&token).unwrap();
            self.fail(&error);
            self.reject(pending.work)?;
            return Ok(None);
        }
        Ok(Some(token))
    }

    fn receive(&mut self) {
        loop {
            match self.input.try_recv() {
                Ok(Command::Io(io)) if matches!(io.operation, Operation::Read { .. }) => {
                    self.reads.push_back(io)
                }
                Ok(command) => {
                    let pause = matches!(command, Command::Pause { .. });
                    if matches!(command, Command::Append(_)) {
                        self.last_write = Instant::now();
                    }
                    self.commands.push_back(command);
                    if pause && !self.failed {
                        break;
                    }
                }
                Err(mailbox::TryRecvError::Empty) => break,
                Err(mailbox::TryRecvError::Disconnected) => {
                    self.closed = true;
                    self.closed_since.get_or_insert_with(Instant::now);
                    break;
                }
            }
        }
    }

    fn reap(&mut self) -> io::Result<()> {
        #[cfg(test)]
        if self
            .control
            .as_ref()
            .is_some_and(|control| control.released())
            && let Some((token, result)) = self.withheld.take()
        {
            self.complete_cqe(token, result)?;
        }
        loop {
            let completed = self
                .ring
                .completion()
                .next()
                .map(|c| (c.user_data(), c.result()));
            let Some((token, result)) = completed else {
                break;
            };
            #[cfg(test)]
            if let Some(control) = &self.control
                && let Some(pending) = self.pending.get(&token)
                && control.hold(&pending.work)
            {
                assert!(self.withheld.replace((token, result)).is_none());
                continue;
            }
            self.complete_cqe(token, result)?;
        }
        Ok(())
    }

    fn complete_cqe(&mut self, token: u64, result: i32) -> io::Result<()> {
        if token == CANCEL_CQE {
            // The original poll's CQE, not this acknowledgment, releases ownership.
            return Ok(());
        }
        let Some(pending) = self.pending.get_mut(&token) else {
            return Err(io::Error::other("unknown local CQE"));
        };
        if pending.ready.is_some() {
            return Err(io::Error::other("duplicate local CQE"));
        }
        pending.ready = Some(Instant::now());
        self.worker
            .shared
            .metrics
            .lock()
            .expect("metrics poisoned")
            .io_completed += 1;
        if pending.cancel_requested {
            // Cancellation can race readiness. The closing/failed image returns
            // its own error, without poisoning the shared fetch or its leader.
            return Ok(());
        }
        let error = if result < 0 {
            Some(io::Error::from_raw_os_error(-result))
        } else if result as usize != pending.expected_bytes() {
            Some(io::Error::other("short local IO completion"))
        } else {
            None
        };
        if error.is_none() {
            use crate::fault::Point;
            let boundary = match &pending.work {
                Work::Append(append) => Some((
                    Point::AfterAppendCqe,
                    append.submission.batch().envelope().last,
                )),
                Work::Fence(fence) if fence.syncing => {
                    Some((Point::AfterSync, fence.submission.batch().envelope().last))
                }
                _ => None,
            };
            if let Some((point, boundary)) = boundary {
                self.worker.shared.hit(point, boundary)?;
            }
        }
        if let Some(error) = error {
            if matches!(&pending.work, Work::Read(read) if read.shared_io())
                && let Some(port) = &self.worker.port
            {
                port.fail_shared(&error);
            }
            self.fail(&error);
        } else if matches!(pending.work, Work::Append(_)) && self.appends.front() != Some(&token) {
            self.worker
                .shared
                .metrics
                .lock()
                .expect("metrics poisoned")
                .reordered_appends += 1;
        }
        Ok(())
    }

    fn publish(&mut self) -> io::Result<()> {
        while let Some(&token) = self.appends.front() {
            let pending = self.pending.get(&token).expect("ordered append owner");
            let Some(ready) = pending.ready else {
                break;
            };
            let Work::Append(append) = &pending.work else {
                unreachable!()
            };
            {
                // Shares the frontend's success/failure linearization lock.
                let gate = self.worker.shared.health.clone();
                let mut state = gate.lock().expect("completion gate poisoned");
                if state.failure.is_some() {
                    break;
                }
                let result = self
                    .worker
                    .log
                    .publish_append(&append.submission)
                    .map_err(io::Error::other)
                    .and_then(|()| state.publish(self.worker.log.status().published));
                if let Err(error) = result {
                    state.fail(error.to_string());
                    return Err(error);
                }
            }
            let pending = self.pending.remove(&token).unwrap();
            self.appends.pop_front();
            let Work::Append(append) = pending.work else {
                unreachable!()
            };
            let mut metrics = self.worker.shared.metrics.lock().expect("metrics poisoned");
            metrics.publication_wait_ns = metrics
                .publication_wait_ns
                .saturating_add(nanos(ready.elapsed()));
            metrics.completion_retained_peak = metrics
                .completion_retained_peak
                .max(self.worker.shared.pools.append.usage().current.bytes);
            drop(metrics);
            self.finish_append(append, Ok(()))?;
        }
        Ok(())
    }

    fn finish_ready(&mut self) -> io::Result<()> {
        self.ready.clear();
        self.ready.extend(
            self.pending
                .iter()
                .filter_map(|(&token, pending)| pending.ready.map(|_| token)),
        );
        self.ready.sort_unstable();
        for index in 0..self.ready.len() {
            let token = self.ready[index];
            let Some(mut pending) = self.pending.remove(&token) else {
                continue;
            };
            if self.failed {
                self.reject(pending.work)?;
                continue;
            }
            match &mut pending.work {
                Work::Append(_) => {
                    self.pending.insert(token, pending);
                }
                Work::Read(read) => match read.advance() {
                    Ok(true) => {
                        let Work::Read(read) = pending.work else {
                            unreachable!()
                        };
                        self.send_io(BudgetBox::into_inner(read).into_io(), Ok(()))?;
                    }
                    Ok(false) => {
                        self.pending.insert(token, pending);
                        self.push(token)?;
                    }
                    Err(error) => {
                        let Work::Read(read) = pending.work else {
                            unreachable!()
                        };
                        self.fail_read(read, error)?;
                    }
                },
                Work::Fence(fence) if fence.syncing => {
                    let gate = self.worker.shared.health.clone();
                    let mut guard = gate.lock().expect("completion gate poisoned");
                    if let Some(error) = guard.failure.as_ref() {
                        return Err(io::Error::other(error.clone()));
                    }
                    self.worker
                        .log
                        .complete_sync(&fence.submission)
                        .map_err(io::Error::other)?;
                    self.last_sync = Instant::now();
                    guard.durable = self.worker.log.status().durable;
                    drop(guard);
                    let Work::Fence(fence) = pending.work else {
                        unreachable!()
                    };
                    let Fence {
                        submission,
                        _credits,
                        mut waiters,
                        rollover,
                        ..
                    } = fence;
                    drop(submission);
                    drop(_credits);
                    self.cohort = None;
                    for waiter in waiters.drain(..) {
                        self.send_io(waiter, Ok(()))?;
                    }
                    self.fence_waiters = Some(waiters);
                    if rollover {
                        self.rotate(append::RotationKind::Rollover)?;
                    }
                }
                Work::Fence(fence) => {
                    if self
                        .worker
                        .log
                        .ready_to_sync(&fence.submission)
                        .map_err(io::Error::other)?
                    {
                        self.worker.shared.hit(
                            crate::fault::Point::BeforeSync,
                            fence.submission.batch().envelope().last,
                        )?;
                        fence.syncing = true;
                        self.pending.insert(token, pending);
                        self.push(token)?;
                    } else {
                        self.pending.insert(token, pending);
                    }
                }
            }
        }
        Ok(())
    }

    fn start_fence(&mut self, waiter: Option<Io>, rollover: bool) -> io::Result<()> {
        let credits = self
            .worker
            .shared
            .pools
            .control
            .reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 0,
            })
            .ok_or_else(|| io::Error::other("internal fence control reserve exhausted"))?;
        let submission = match self.prepare_fence() {
            Err(append::Error::Rollover) if self.worker.port.is_none() => {
                self.rotate(append::RotationKind::Rollover)?;
                self.prepare_fence().map_err(io::Error::other)?
            }
            Err(append::Error::Rollover) => {
                if let Some(waiter) = waiter {
                    self.commands.push_front(Command::Io(waiter));
                }
                return self.rotate(append::RotationKind::Rollover);
            }
            result => result.map_err(io::Error::other)?,
        };
        let mut waiters = self.fence_waiters.take().expect("one active fence cohort");
        waiters.extend(waiter);
        self.cohort = self.enqueue(Work::Fence(Fence {
            submission,
            _credits: credits,
            waiters,
            syncing: false,
            rollover,
        }))?;
        Ok(())
    }

    fn prepare_fence(&mut self) -> append::Result<append::Submission> {
        match &self.worker.shared.window {
            Some(window) => window.fence(&mut self.worker.log),
            None => self.worker.log.prepare_fence(),
        }
    }

    fn rotate(&mut self, kind: append::RotationKind) -> io::Result<()> {
        if let Some(port) = &mut self.worker.port {
            port.rotate(kind)
        } else {
            let prepared = self
                .worker
                .log
                .prepare_rotation(kind)
                .map_err(io::Error::other)?;
            self.worker
                .log
                .install_rotation(prepared.create()?)
                .map_err(io::Error::other)
        }
    }

    fn dispatch(&mut self) -> io::Result<()> {
        if self.worker.port.as_ref().is_some_and(host::Port::quiescing) {
            // Guest owners have drained. Later lifecycle commands remain queued
            // until the collector explicitly releases this generation.
            if self.pending.is_empty() && self.reads.is_empty() && self.appends.is_empty() {
                if !self
                    .worker
                    .log
                    .covers_flush(self.worker.log.status().published)
                {
                    self.start_fence(None, false)?;
                } else {
                    self.worker
                        .port
                        .as_mut()
                        .unwrap()
                        .acknowledge_quiescence()?;
                }
            }
            return Ok(());
        }

        while let Some(command) = self.commands.front_mut() {
            if matches!(command, Command::Pause { .. })
                && let Some(port) = &mut self.worker.port
            {
                port.cancel_future_rollover(&self.worker.log);
            }
            // A completed prefix needs no new segment or IO. Keep this control
            // path available while the worker allocates for future mutations.
            if matches!(command, Command::Io(io) if self.worker.log.covers_flush(io.boundary)) {
                let Command::Io(io) = self.commands.pop_front().unwrap() else {
                    unreachable!()
                };
                self.send_io(io, Ok(()))?;
                continue;
            }
            if self.worker.port.as_ref().is_some_and(host::Port::rotating) {
                break;
            }
            match command {
                Command::Pause { .. }
                    if !self.pending.is_empty()
                        || !self.reads.is_empty()
                        || self.worker.port.as_ref().is_some_and(host::Port::pending) =>
                {
                    break;
                }
                Command::Pause { .. } => {
                    self.admission_paused = true;
                    let command = self.commands.pop_front().unwrap();
                    self.worker.execute(command)?;
                    break;
                }
                Command::Resume => {
                    self.admission_paused = false;
                    self.commands.pop_front();
                    continue;
                }
                Command::NewAttachment { rotated, .. } => {
                    if !self.admission_paused {
                        return Err(io::Error::other("attachment requires storage pause"));
                    }
                    if !self.pending.is_empty() || !self.reads.is_empty() {
                        break;
                    }
                    let boundary = self.worker.log.status().published;
                    if !self.worker.log.covers_flush(boundary) {
                        self.start_fence(None, false)?;
                        break;
                    }
                    if !*rotated {
                        *rotated = true;
                        self.rotate(append::RotationKind::FreshAttachment)?;
                        if self.worker.port.is_none() {
                            self.start_fence(None, false)?;
                        }
                        break;
                    }
                    let command = self.commands.pop_front().unwrap();
                    self.worker.execute(command)?;
                    continue;
                }
                _ if self.admission_paused => {
                    return Err(io::Error::other("IO during storage pause"));
                }
                Command::Append(packing) => match self.worker.log.check_append(&packing.builder) {
                    Err(append::Error::Pending) => {
                        self.paused_since.get_or_insert_with(Instant::now);
                        break;
                    }
                    Err(append::Error::Rollover) if self.worker.shared.window.is_some() => {
                        return Err(io::Error::other("admitted WRITE exceeded its WAL window"));
                    }
                    Err(append::Error::Rollover) => {
                        let status = self.worker.log.status();
                        if status.issued == status.published && status.durable == status.published {
                            self.rotate(append::RotationKind::Rollover)?;
                        } else {
                            self.start_fence(None, true)?;
                        }
                        continue;
                    }
                    Err(error) => return Err(io::Error::other(error)),
                    Ok(()) => (),
                },
                Command::Io(io) => {
                    if let Some(token) = self.cohort {
                        let Work::Fence(fence) = &mut self.pending.get_mut(&token).unwrap().work
                        else {
                            unreachable!()
                        };
                        if io.boundary > fence.submission.batch().envelope().last {
                            break;
                        }
                        let Command::Io(io) = self.commands.pop_front().unwrap() else {
                            unreachable!()
                        };
                        fence.waiters.push(io);
                        continue;
                    }
                }
            }
            match self.commands.pop_front().unwrap() {
                Command::Append(Packing {
                    builder,
                    credits,
                    mut writes,
                }) => {
                    if let Some(start) = self.paused_since.take() {
                        let mut metrics =
                            self.worker.shared.metrics.lock().expect("metrics poisoned");
                        metrics.cohort_pause_ns = metrics
                            .cohort_pause_ns
                            .saturating_add(nanos(start.elapsed()));
                    }
                    let address = builder.allocation_address();
                    let submission = match &self.worker.shared.window {
                        Some(window) => window::Window::append(
                            window,
                            &mut self.worker.log,
                            builder,
                            &mut writes,
                        )?,
                        None => self
                            .worker
                            .log
                            .prepare_append(builder)
                            .map_err(io::Error::other)?,
                    };
                    assert_eq!(submission.batch().allocation_address(), address);
                    let mut metrics = self.worker.shared.metrics.lock().expect("metrics poisoned");
                    metrics.batches_submitted += 1;
                    metrics.encoded_bytes += submission.batch().bytes().len() as u64;
                    metrics.encoding_retained_peak = metrics
                        .encoding_retained_peak
                        .max(self.worker.shared.pools.append.usage().current.bytes);
                    drop(metrics);
                    if let Some(token) = self.enqueue(Work::Append(Append {
                        submission,
                        allocation_address: address,
                        credits,
                        writes,
                    }))? {
                        self.appends.push_back(token);
                    }
                }
                Command::Io(io) => self.start_fence(Some(io), false)?,
                Command::Pause { .. } | Command::Resume | Command::NewAttachment { .. } => {
                    unreachable!("administrative command handled above")
                }
            }
        }
        while !self.failed
            && self
                .reads
                .front()
                .is_some_and(|io| io.boundary <= self.worker.log.status().published)
        {
            let io = self.reads.pop_front().unwrap();
            self.start_read(io)?;
        }
        if self.failed {
            return self.reject_queued();
        }
        self.sync_for_compaction()?;
        self.drain_window()?;
        if !self.closed
            && !self.admission_paused
            && !self.worker.port.as_ref().is_some_and(host::Port::rotating)
            && self.cohort.is_none()
            && self.commands.is_empty()
            && self.worker.log.status().durable < self.worker.log.status().issued
            && self.last_write.elapsed() >= IDLE_SYNC
        {
            self.start_fence(None, false)?;
        }
        Ok(())
    }

    fn start_read(&mut self, mut io: Io) -> io::Result<()> {
        let Operation::Read { offset, buffer } = &mut io.operation else {
            unreachable!()
        };
        let plan = match self
            .worker
            .log
            .read_plan(*offset, buffer.as_slice().len(), io.boundary)
        {
            Ok(plan) => plan,
            Err(error) => return self.fail_io(io, io::Error::other(error)),
        };
        buffer.as_mut_slice().fill(0);
        let mut allocation = match BudgetBox::<Read, _>::try_new_uninit_in(BudgetAllocator::new(
            Arc::clone(&self.worker.shared.metadata),
        )) {
            Ok(allocation) => allocation,
            Err(_) => {
                return self.fail_io(
                    io,
                    io::Error::new(io::ErrorKind::OutOfMemory, "read state metadata exhausted"),
                );
            }
        };
        allocation.write(Read::new(plan, io));
        // SAFETY: the unique box above was initialized exactly once with Read;
        // no fallible preparation occurs until this owner is fully initialized.
        let mut read = unsafe { allocation.assume_init() };
        if let Err(error) = read.prepare(self.worker.port.as_ref()) {
            return self.fail_read(read, error);
        }
        if read.done() {
            self.send_io(BudgetBox::into_inner(read).into_io(), Ok(()))
        } else {
            self.enqueue(Work::Read(read)).map(|_| ())
        }
    }

    fn fail_io(&mut self, io: Io, error: io::Error) -> io::Result<()> {
        self.fail(&error);
        self.send_io(io, Err(error))
    }

    fn fail_read(
        &mut self,
        read: BudgetBox<Read, BudgetAllocator>,
        error: io::Error,
    ) -> io::Result<()> {
        if read.shared_io()
            && let Some(port) = &self.worker.port
        {
            port.fail_shared(&error);
        }
        self.fail_io(BudgetBox::into_inner(read).into_io(), error)
    }

    fn drain_window(&mut self) -> io::Result<()> {
        let Some(window) = &self.worker.shared.window else {
            return Ok(());
        };
        let state = window.status();
        if self.closed
            || self.admission_paused
            || !(state.rotation_wanted || state.index_pressure)
            || state.unsubmitted != 0
            || self.cohort.is_some()
            || self.worker.port.as_ref().is_some_and(host::Port::rotating)
        {
            return Ok(());
        }
        let status = self.worker.log.status();
        if status.issued != status.published {
            return Ok(());
        }
        if status.durable != status.published {
            self.start_fence(None, false)
        } else if state.rotation_wanted {
            self.rotate(append::RotationKind::Rollover)
        } else {
            Ok(())
        }
    }

    fn sync_for_compaction(&mut self) -> io::Result<()> {
        if !self.closed
            && !self.admission_paused
            && self.cohort.is_none()
            && !self.worker.port.as_ref().is_some_and(host::Port::rotating)
            && self.worker.log.status().issued > self.worker.log.status().durable
            && self.worker.shared.window.as_ref().is_some_and(|window| {
                window.host_pressure()
                    || window.status().index_pressure
                    || self.last_sync.elapsed() >= Duration::from_secs(1)
            })
        {
            self.start_fence(None, false)?;
        }
        Ok(())
    }

    fn reject_queued(&mut self) -> io::Result<()> {
        if let Some(scheduler) = &self.scheduler {
            scheduler.ready(false)?;
        }
        while let Some(token) = self.bulk.pop_front() {
            let pending = self.pending.remove(&token).expect("unsubmitted bulk owner");
            assert!(!pending.in_kernel());
            self.reject(pending.work)?;
        }
        while let Some(command) = self.commands.pop_front() {
            self.worker.execute(command)?;
        }
        while let Some(io) = self.reads.pop_front() {
            self.send_io(io, Err(self.failure()))?;
        }
        Ok(())
    }

    fn cancel_dependencies(&mut self) -> io::Result<()> {
        loop {
            let next = self.pending.iter().find_map(|(&token, pending)| {
                (pending.in_kernel() && pending.dependency() && !pending.cancel_requested)
                    .then_some(token)
            });
            let Some(token) = next else {
                return Ok(());
            };
            let entry = opcode::PollRemove::new(token).build().user_data(CANCEL_CQE);
            // SAFETY: PollRemove refers to an existing token, with no borrowed
            // memory. The original owner stays in pending until its own CQE.
            if unsafe { self.ring.submission().push(&entry) }.is_err() {
                self.ring.submit()?;
                continue;
            }
            self.pending.get_mut(&token).unwrap().cancel_requested = true;
        }
    }

    fn stop(&mut self, error: &io::Error) {
        self.fail(error);
        self.input.close();
        // Every successful sender published before the close above. Drain it
        // after closing so receiver destruction cannot strand a late command.
        self.receive();
        // These commands never reached the kernel. Return their owned errors
        // before closing the producer; Drop still drains actual pending IO.
        let _ = self.reject_queued();
        let _ = self.cancel_dependencies();
    }

    fn wait(&self) -> io::Result<()> {
        let active = self
            .worker
            .port
            .as_ref()
            .is_some_and(|port| port.needs_wake(&self.worker.log))
            || !self.pending.is_empty()
            || self.worker.shared.window.as_ref().is_some_and(|window| {
                let state = window.status();
                state.rotation_wanted || state.index_pressure
            })
            || (!self.admission_paused
                && self.worker.log.status().issued > self.worker.log.status().durable);
        let timeout = if active { 50 } else { -1 };
        let mut descriptors = [
            libc::pollfd {
                fd: self.input_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: self.kernel_wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: both descriptors are live and the array covers nfds entries.
        let result = unsafe {
            libc::poll(
                descriptors.as_mut_ptr(),
                descriptors.len() as libc::nfds_t,
                timeout,
            )
        };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        for event in [&self.input_wake, &self.kernel_wake] {
            match event.read() {
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    pub fn run(mut self) {
        loop {
            self.receive();
            let result = (|| {
                self.reap()?;
                let failure = self
                    .worker
                    .shared
                    .health
                    .lock()
                    .expect("completion gate poisoned")
                    .failure
                    .clone();
                if let Some(error) = failure {
                    self.fail(&io::Error::other(error));
                }
                if !self.failed
                    && self
                        .closed_since
                        .is_some_and(|since| since.elapsed() >= IO_DEADLINE)
                {
                    self.fail(&io::Error::new(
                        io::ErrorKind::TimedOut,
                        "terminal storage drain deadline expired",
                    ));
                }
                if !self.failed
                    && self.pending.values().any(|pending| {
                        pending.ready.is_none()
                            && !pending.dependency()
                            && pending
                                .submitted
                                .is_some_and(|submitted| submitted.elapsed() >= IO_DEADLINE)
                    })
                {
                    self.fail(&io::Error::new(
                        io::ErrorKind::TimedOut,
                        "local IO deadline expired",
                    ));
                }
                if !self.failed {
                    if let Some(port) = &mut self.worker.port {
                        if self.closed {
                            port.cancel_future_rollover(&self.worker.log);
                        }
                        port.poll(
                            &mut self.worker.log,
                            self.last_write,
                            self.admission_paused || self.closed,
                        )?;
                        if let Some(window) = &self.worker.shared.window
                            && window.installed(&self.worker.log)?
                        {
                            notify(&self.worker.wake.0)?;
                        }
                        *self
                            .worker
                            .shared
                            .final_status
                            .lock()
                            .expect("status poisoned") = self.worker.log.status();
                    }
                    self.publish()?;
                    if let Some(window) = &self.worker.shared.window
                        && window.refresh(&self.worker.log)?
                    {
                        notify(&self.worker.wake.0)?;
                    }
                }
                self.finish_ready()?;
                if self.failed {
                    self.reject_queued()?;
                    self.cancel_dependencies()?;
                } else {
                    self.dispatch()?;
                }
                if !self.failed {
                    self.submit_bulk()?;
                }
                self.ring.submit()?;
                Ok::<(), io::Error>(())
            })();
            if let Err(error) = result {
                self.stop(&error);
                // Unexpected reactor failure ends this producer. Drop first
                // drains kernel ownership and returns the accepted requests.
                break;
            }
            if self.closed
                && self.pending.is_empty()
                && self.commands.is_empty()
                && self.reads.is_empty()
                && !self.worker.port.as_ref().is_some_and(host::Port::pending)
            {
                break;
            }
            if let Err(error) = self.wait() {
                self.stop(&error);
                break;
            }
        }
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        // Also cover unexpected exits before the ordinary failure loop.
        let _ = self.cancel_dependencies();
        if let Some(scheduler) = &self.scheduler {
            let _ = scheduler.ready(false);
        }
        #[cfg(test)]
        if let Some((token, _)) = self.withheld.take()
            && let Some(pending) = self.pending.get_mut(&token)
        {
            // This CQE was already observed by the test interposer. Its buffer
            // has no kernel owner even if publication remains withheld.
            pending.ready = Some(Instant::now());
        }
        while self.pending.values().any(Pending::in_kernel) {
            match self.ring.submit_and_wait(1) {
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // Completed owners can still return below. Keep uncertain
                    // buffers and file descriptions alive after a drain error.
                    break;
                }
            }
            for completion in self.ring.completion() {
                if let Some(pending) = self.pending.get_mut(&completion.user_data()) {
                    pending.ready = Some(Instant::now());
                }
            }
        }
        // An unexpected loop exit may leave accepted requests in pending even
        // after their CQEs arrive. Return their owned errors before the producer
        // closes; never free a buffer still potentially referenced by the ring.
        while let Some(pending) = self.pending.remove_first(|pending| !pending.in_kernel()) {
            let _ = self.reject(pending.work);
        }
        self.pending.leak();
    }
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
