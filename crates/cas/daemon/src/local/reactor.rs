//! One image's io_uring owner. Kernel completion and logical publication are
//! distinct; every pending entry retains its allocation, file and credits.
use std::collections::{BTreeMap, VecDeque};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use cas_core::aligned::AlignedBuffer;
use cas_core::append::{ReadPlan, Submission};
use io_uring::{IoUring, opcode, squeue, types};

use super::*;

const IO_DEADLINE: Duration = Duration::from_secs(30);
const IDLE_SYNC: Duration = Duration::from_millis(50);

#[cfg(test)]
mod tests;

struct Append {
    submission: Submission,
    allocation_address: usize,
    credits: Credits,
    writes: Vec<Write>,
}

struct Read {
    // Scratch must be destroyed before the IO's response and byte credits.
    scratch: Option<AlignedBuffer>,
    plan: ReadPlan,
    io: Io,
    range: usize,
}

impl Read {
    fn entry(&mut self) -> squeue::Entry {
        let range = &self.plan.ranges()[self.range];
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        let pointer = if range.direct_to_response() {
            buffer.as_mut_slice()[range.destination()].as_mut_ptr()
        } else {
            if self
                .scratch
                .as_ref()
                .is_none_or(|buffer| buffer.as_slice().len() < range.input_bytes())
            {
                drop(self.scratch.take());
                self.scratch = Some(AlignedBuffer::new(range.input_bytes()));
            }
            self.scratch.as_mut().unwrap().as_mut_slice().as_mut_ptr()
        };
        opcode::Read::new(
            types::Fd(range.file().as_raw_fd()),
            pointer,
            range.input_bytes() as u32,
        )
        .offset(range.offset())
        .build()
    }

    fn advance(&mut self) -> io::Result<bool> {
        let range = &self.plan.ranges()[self.range];
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        if range.direct_to_response() {
            range.verify(&buffer.as_slice()[range.destination()])?;
        } else {
            let input = &self.scratch.as_ref().unwrap().as_slice()[..range.input_bytes()];
            range.verify(input)?;
            buffer.as_mut_slice()[range.destination()].copy_from_slice(&input[range.source()]);
        }
        self.range += 1;
        Ok(self.range == self.plan.ranges().len())
    }

    fn into_io(self) -> Io {
        // Remaining fields, including scratch, drop before the caller receives
        // the IO and can release its permit through a completion response.
        self.io
    }
}

struct Fence {
    submission: Submission,
    _credits: Credits,
    waiters: Vec<Io>,
    syncing: bool,
    rollover: bool,
}

enum Work {
    Append(Append),
    Read(Read),
    Fence(Fence),
}

struct Pending {
    work: Work,
    submitted: Instant,
    ready: Option<Instant>,
}

impl Pending {
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
            Work::Read(read) => read.plan.ranges()[read.range].input_bytes(),
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
    pending: BTreeMap<u64, Pending>,
    appends: VecDeque<u64>,
    commands: VecDeque<Command>,
    reads: VecDeque<Io>,
    worker: Worker,
    input: mpsc::Receiver<Command>,
    input_wake: EventFd,
    kernel_wake: EventFd,
    next_token: u64,
    cohort: Option<u64>,
    paused_since: Option<Instant>,
    last_write: Instant,
    closed: bool,
    failed: bool,
    #[cfg(test)]
    control: Option<Arc<tests::Control>>,
    #[cfg(test)]
    withheld: Option<(u64, i32)>,
}

impl Reactor {
    pub fn new(
        worker: Worker,
        input: mpsc::Receiver<Command>,
        input_wake: EventFd,
    ) -> io::Result<Self> {
        let ring = IoUring::new(256)?;
        let kernel_wake = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
        ring.submitter().register_eventfd(kernel_wake.as_raw_fd())?;
        Ok(Self {
            ring,
            pending: BTreeMap::new(),
            appends: VecDeque::new(),
            commands: VecDeque::new(),
            reads: VecDeque::new(),
            worker,
            input,
            input_wake,
            kernel_wake,
            next_token: 1,
            cohort: None,
            paused_since: None,
            last_write: Instant::now(),
            closed: false,
            failed: false,
            #[cfg(test)]
            control: None,
            #[cfg(test)]
            withheld: None,
        })
    }

    fn fail(&mut self, error: &io::Error) {
        self.failed = true;
        self.worker.log.fail();
        let mut failed = self.worker.health.lock().expect("completion gate poisoned");
        if failed.is_none() {
            *failed = Some(error.to_string());
        }
        self.appends.clear();
        let _ = self.worker.wake.0.write(1);
    }

    fn failure(&self) -> io::Error {
        io::Error::other(
            self.worker
                .health
                .lock()
                .expect("completion gate poisoned")
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
            let mut metrics = self.worker.metrics.lock().expect("metrics poisoned");
            metrics.allocation_identity_checks += 1;
            metrics.allocations_released += 1;
        }
        for write in writes {
            let result = result
                .as_ref()
                .copied()
                .map_err(|error| io::Error::other(error.to_string()));
            self.worker.send(
                write.id,
                CompletionData::Write { bytes: write.bytes },
                result,
                write.permit,
            )?;
        }
        Ok(())
    }

    fn reject(&mut self, work: Work) -> io::Result<()> {
        match work {
            Work::Append(append) => self.finish_append(append, Err(self.failure())),
            Work::Read(read) => self.send_io(read.into_io(), Err(self.failure())),
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
        let entry = pending.entry().user_data(token);
        #[cfg(test)]
        let entry = if self.control.as_ref().is_some_and(|control| control.short)
            && matches!(&pending.work, Work::Append(append) if append.submission.batch().envelope().first == 1)
        {
            let Work::Append(append) = &pending.work else {
                unreachable!()
            };
            // Persist only A's header. The expected CQE still requires its full
            // encoded length, exactly as for a short write returned by the OS.
            opcode::Write::new(
                types::Fd(append.submission.file().as_raw_fd()),
                append.submission.batch().bytes().as_ptr(),
                BLOCK_SIZE as u32,
            )
            .offset(append.submission.offset())
            .build()
            .user_data(token)
        } else {
            entry
        };
        // SAFETY: pending owns every referenced buffer and the locked file.
        // No owner is removed until its CQE. Drop drains or retains all owners.
        unsafe { self.ring.submission().push(&entry) }.map_err(io::Error::other)?;
        pending.submitted = Instant::now();
        pending.ready = None;
        let mut metrics = self.worker.metrics.lock().expect("metrics poisoned");
        metrics.io_queued += 1;
        metrics.peak_awaiting_cqe = metrics
            .peak_awaiting_cqe
            .max(self.pending.values().filter(|p| p.ready.is_none()).count());
        Ok(())
    }

    fn enqueue(&mut self, work: Work) -> io::Result<Option<u64>> {
        let Some(next) = self.next_token.checked_add(1) else {
            self.fail(&io::Error::other("IO token exhausted"));
            self.reject(work)?;
            return Ok(None);
        };
        let token = self.next_token;
        self.next_token = next;
        self.pending.insert(
            token,
            Pending {
                work,
                submitted: Instant::now(),
                ready: Some(Instant::now()),
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
                    if matches!(command, Command::Append(_)) {
                        self.last_write = Instant::now();
                    }
                    self.commands.push_back(command);
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.closed = true;
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
                && let Some(Pending {
                    work: Work::Append(append),
                    ..
                }) = self.pending.get(&token)
            {
                control.observe(append.submission.batch().envelope().last);
                if append.submission.batch().envelope().first == 1 && !control.released() {
                    self.withheld = Some((token, result));
                    continue;
                }
            }
            self.complete_cqe(token, result)?;
        }
        Ok(())
    }

    fn complete_cqe(&mut self, token: u64, result: i32) -> io::Result<()> {
        let Some(pending) = self.pending.get_mut(&token) else {
            return Err(io::Error::other("unknown local CQE"));
        };
        if pending.ready.is_some() {
            return Err(io::Error::other("duplicate local CQE"));
        }
        pending.ready = Some(Instant::now());
        self.worker
            .metrics
            .lock()
            .expect("metrics poisoned")
            .io_completed += 1;
        let error = if result < 0 {
            Some(io::Error::from_raw_os_error(-result))
        } else if result as usize != pending.expected_bytes() {
            Some(io::Error::other("short local IO completion"))
        } else {
            None
        };
        if let Some(error) = error {
            self.fail(&error);
        } else if matches!(pending.work, Work::Append(_)) && self.appends.front() != Some(&token) {
            self.worker
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
                let gate = Arc::clone(&self.worker.health);
                let failed = gate.lock().expect("completion gate poisoned");
                if failed.is_some() {
                    break;
                }
                self.worker
                    .log
                    .publish_append(&append.submission)
                    .map_err(io::Error::other)?;
            }
            let pending = self.pending.remove(&token).unwrap();
            self.appends.pop_front();
            let Work::Append(append) = pending.work else {
                unreachable!()
            };
            let mut metrics = self.worker.metrics.lock().expect("metrics poisoned");
            metrics.publication_wait_ns = metrics
                .publication_wait_ns
                .saturating_add(nanos(ready.elapsed()));
            metrics.completion_retained_peak = metrics
                .completion_retained_peak
                .max(self.worker.pools.append.usage().current.bytes);
            drop(metrics);
            self.finish_append(append, Ok(()))?;
        }
        Ok(())
    }

    fn finish_ready(&mut self) -> io::Result<()> {
        let ready: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(&token, pending)| pending.ready.map(|_| token))
            .collect();
        for token in ready {
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
                        self.send_io(read.into_io(), Ok(()))?;
                    }
                    Ok(false) => {
                        self.pending.insert(token, pending);
                        self.push(token)?;
                    }
                    Err(error) => {
                        self.fail(&error);
                        self.reject(pending.work)?;
                    }
                },
                Work::Fence(fence) if fence.syncing => {
                    let gate = Arc::clone(&self.worker.health);
                    let guard = gate.lock().expect("completion gate poisoned");
                    if let Some(error) = guard.as_ref() {
                        return Err(io::Error::other(error.clone()));
                    }
                    self.worker
                        .log
                        .complete_sync(&fence.submission)
                        .map_err(io::Error::other)?;
                    drop(guard);
                    let Work::Fence(fence) = pending.work else {
                        unreachable!()
                    };
                    let Fence {
                        submission,
                        _credits,
                        waiters,
                        rollover,
                        ..
                    } = fence;
                    drop(submission);
                    drop(_credits);
                    self.cohort = None;
                    for waiter in waiters {
                        self.send_io(waiter, Ok(()))?;
                    }
                    if rollover {
                        self.worker.log.rollover().map_err(io::Error::other)?;
                    }
                }
                Work::Fence(fence) => {
                    if self
                        .worker
                        .log
                        .ready_to_sync(&fence.submission)
                        .map_err(io::Error::other)?
                    {
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
            .pools
            .control
            .reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 0,
            })
            .ok_or_else(|| io::Error::other("internal fence control reserve exhausted"))?;
        let submission = self.worker.log.prepare_fence().map_err(io::Error::other)?;
        self.cohort = self.enqueue(Work::Fence(Fence {
            submission,
            _credits: credits,
            waiters: waiter.into_iter().collect(),
            syncing: false,
            rollover,
        }))?;
        Ok(())
    }

    fn dispatch(&mut self) -> io::Result<()> {
        while let Some(command) = self.commands.front() {
            match command {
                Command::Append(packing) => match self.worker.log.check_append(&packing.builder) {
                    Err(append::Error::Pending) => {
                        self.paused_since.get_or_insert_with(Instant::now);
                        break;
                    }
                    Err(append::Error::Rollover) => {
                        let status = self.worker.log.status();
                        if status.issued == status.published && status.durable == status.published {
                            self.worker.log.rollover().map_err(io::Error::other)?;
                        } else {
                            self.start_fence(None, true)?;
                        }
                        continue;
                    }
                    Err(error) => return Err(io::Error::other(error)),
                    Ok(()) => (),
                },
                Command::Io(io) => {
                    if self.worker.log.covers_flush(io.boundary) {
                        let Command::Io(io) = self.commands.pop_front().unwrap() else {
                            unreachable!()
                        };
                        self.send_io(io, Ok(()))?;
                        continue;
                    }
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
                    writes,
                }) => {
                    if let Some(start) = self.paused_since.take() {
                        let mut metrics = self.worker.metrics.lock().expect("metrics poisoned");
                        metrics.cohort_pause_ns = metrics
                            .cohort_pause_ns
                            .saturating_add(nanos(start.elapsed()));
                    }
                    let address = builder.allocation_address();
                    let submission = self
                        .worker
                        .log
                        .prepare_append(builder)
                        .map_err(io::Error::other)?;
                    assert_eq!(submission.batch().allocation_address(), address);
                    let mut metrics = self.worker.metrics.lock().expect("metrics poisoned");
                    metrics.batches_submitted += 1;
                    metrics.encoded_bytes += submission.batch().bytes().len() as u64;
                    metrics.encoding_retained_peak = metrics
                        .encoding_retained_peak
                        .max(self.worker.pools.append.usage().current.bytes);
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
            }
        }
        while self
            .reads
            .front()
            .is_some_and(|io| io.boundary <= self.worker.log.status().published)
        {
            let mut io = self.reads.pop_front().unwrap();
            let Operation::Read { offset, buffer } = &mut io.operation else {
                unreachable!()
            };
            let plan = self
                .worker
                .log
                .read_plan(*offset, buffer.as_slice().len(), io.boundary)
                .map_err(io::Error::other)?;
            buffer.as_mut_slice().fill(0);
            if plan.ranges().is_empty() {
                self.send_io(io, Ok(()))?;
            } else {
                self.enqueue(Work::Read(Read {
                    scratch: None,
                    plan,
                    io,
                    range: 0,
                }))?;
            }
        }
        if !self.closed
            && self.cohort.is_none()
            && self.commands.is_empty()
            && self.worker.log.status().durable < self.worker.log.status().issued
            && self.last_write.elapsed() >= IDLE_SYNC
        {
            self.start_fence(None, false)?;
        }
        Ok(())
    }

    fn reject_queued(&mut self) -> io::Result<()> {
        while let Some(command) = self.commands.pop_front() {
            self.worker.execute(command)?;
        }
        while let Some(io) = self.reads.pop_front() {
            self.send_io(io, Err(self.failure()))?;
        }
        Ok(())
    }

    fn wait(&self) -> io::Result<()> {
        let active = !self.pending.is_empty()
            || self.worker.log.status().issued > self.worker.log.status().durable;
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
                if self
                    .worker
                    .health
                    .lock()
                    .expect("completion gate poisoned")
                    .is_some()
                {
                    self.failed = true;
                    self.worker.log.fail();
                }
                if !self.failed
                    && self.pending.values().any(|pending| {
                        pending.ready.is_none() && pending.submitted.elapsed() >= IO_DEADLINE
                    })
                {
                    self.fail(&io::Error::new(
                        io::ErrorKind::TimedOut,
                        "local IO deadline expired",
                    ));
                }
                if !self.failed {
                    self.publish()?;
                }
                self.finish_ready()?;
                if self.failed {
                    self.reject_queued()?;
                } else {
                    self.dispatch()?;
                }
                self.ring.submit()?;
                Ok::<(), io::Error>(())
            })();
            if let Err(error) = result {
                self.fail(&error);
                // Unexpected reactor failure ends this producer. Drop first
                // drains kernel ownership; channel closure releases waiters.
                break;
            }
            if self.closed
                && self.pending.is_empty()
                && self.commands.is_empty()
                && self.reads.is_empty()
            {
                break;
            }
            if let Err(error) = self.wait() {
                self.fail(&error);
                break;
            }
        }
    }
}

impl Drop for Reactor {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some((token, _)) = self.withheld.take()
            && let Some(pending) = self.pending.get_mut(&token)
        {
            // This CQE was already observed by the test interposer. Its buffer
            // has no kernel owner even if publication remains withheld.
            pending.ready = Some(Instant::now());
        }
        while self.pending.values().any(|pending| pending.ready.is_none()) {
            match self.ring.submit_and_wait(1) {
                Ok(_) => (),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    // Retain credits and file descriptions with every allocation
                    // whose kernel ownership cannot be disproved.
                    self.pending.retain(|_, pending| pending.ready.is_none());
                    std::mem::forget(std::mem::take(&mut self.pending));
                    break;
                }
            }
            for completion in self.ring.completion() {
                if let Some(pending) = self.pending.get_mut(&completion.user_data()) {
                    pending.ready = Some(Instant::now());
                }
            }
        }
    }
}

fn nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
