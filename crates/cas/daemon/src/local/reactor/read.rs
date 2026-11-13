use super::*;
use crate::local::chunk::{Block, Fetch, Joined, Located, Sources};
use cas_core::{
    budget::BudgetAllocator,
    cache::fills::Waiter,
    manifest::{file as manifest, tree::LookupState},
    store::file as store,
};

enum Stage {
    Staging(usize),
    Manifest { read: manifest::Read, offset: u64 },
    Header(store::Read),
    Payload(store::Payload),
    Waiting(Waiter<Fetched>),
    Remote(crate::peer::Ticket),
    Done,
}

pub(super) struct Read {
    // Scratch and pinned plans drop before the response can release its credits.
    scratch: Option<AlignedBuffer>,
    page: Option<AlignedBuffer<BudgetAllocator>>,
    plan: ReadPlan,
    /// Shared per-block resolution; `None` for a read with no manifest blocks.
    sources: Option<Sources>,
    chunk_hash: Option<cas_core::chunk_index::Hash>,
    fetches: Option<Fetches>,
    /// The led fetch of the current chunk (store payload or GET reply).
    fetch: Option<Fetch>,
    metadata: Option<Arc<Budget>>,
    #[cfg(test)]
    control: Option<Arc<Mutex<host::tests::Control>>>,
    io: Io,
    stage: Stage,
    block: usize,
    shared_io: bool,
    peer: Option<BudgetArc<crate::peer::Router>>,
}

impl Read {
    pub fn new(plan: ReadPlan, io: Io) -> Self {
        Self {
            scratch: None,
            page: None,
            plan,
            sources: None,
            chunk_hash: None,
            fetches: None,
            fetch: None,
            metadata: None,
            #[cfg(test)]
            control: None,
            io,
            stage: Stage::Done,
            block: 0,
            shared_io: false,
            peer: None,
        }
    }

    pub fn prepare(&mut self, port: Option<&host::Port>) -> io::Result<()> {
        if self.plan.manifest().is_some()
            && (0..self.plan.bytes() / BLOCK_SIZE).any(|block| !self.plan.staged(block))
        {
            let port = port.ok_or_else(|| io::Error::other("manifest read has no shared store"))?;
            self.peer = port.peer();
            self.sources = Some(Sources::new(port));
            if let Some(metrics) = port.peer_metrics() {
                self.io.permit.remote = Some(crate::peer::metrics::Trace::new(
                    metrics,
                    self.io.id,
                    self.io.permit.received.unwrap_or_else(Instant::now),
                ));
            }
            self.fetches = Some(port.fetches());
            self.metadata = Some(port.metadata());
            #[cfg(test)]
            {
                self.control = Some(port.read_control());
            }
            self.page = Some(AlignedBuffer::try_new_in(
                BLOCK_SIZE,
                BudgetAllocator::new(port.metadata()),
            )?);
        }
        if self.plan.ranges().is_empty() {
            self.next_block()?;
        } else {
            self.stage = Stage::Staging(0);
        }
        Ok(())
    }

    pub fn done(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }
    pub fn notification_only(&self) -> bool {
        matches!(self.stage, Stage::Waiting(_) | Stage::Remote(_))
    }
    pub fn shared_io(&self) -> bool {
        self.shared_io
    }
    pub fn trace(&mut self) -> Option<&mut crate::read_trace::ReadTrace> {
        self.io.permit.trace.as_deref_mut()
    }
    pub fn trace_stage(&self) -> usize {
        match self.stage {
            Stage::Staging(_) => 0,
            Stage::Manifest { .. } => 1,
            Stage::Header(_) => 2,
            Stage::Payload(_) => 3,
            Stage::Waiting(_) | Stage::Remote(_) => 4,
            Stage::Done => unreachable!("done read has no IO stage"),
        }
    }
    pub fn into_io(self) -> Io {
        self.io
    }

    fn absolute_block(&self) -> u64 {
        self.plan.offset() / BLOCK_SIZE as u64 + self.block as u64
    }

    fn next_block(&mut self) -> io::Result<()> {
        self.shared_io = false;
        self.stage = Stage::Done;
        let Some(view) = self.plan.manifest().cloned() else {
            return Ok(());
        };
        while self.block < self.plan.bytes() / BLOCK_SIZE {
            if !self.plan.staged(self.block) {
                let block = self.absolute_block();
                let Operation::Read { buffer, .. } = &mut self.io.operation else {
                    unreachable!()
                };
                let destination = &mut buffer.as_mut_slice()
                    [self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE];
                let resolved = self.sources.as_ref().expect("host sources").resolve(
                    &view,
                    block,
                    destination,
                )?;
                match resolved {
                    Block::Hole => (),
                    Block::Cached => {
                        if let Some(trace) = self.trace() {
                            trace.cache_hits += 1;
                        }
                    }
                    Block::Page { read, offset } => {
                        self.stage = Stage::Manifest { read, offset };
                        return Ok(());
                    }
                    Block::Store(hash, plan) => {
                        if self.chunk_located(hash, Located::Store(plan))? {
                            return Ok(());
                        }
                    }
                    Block::Remote(hash) => {
                        if self.chunk_located(hash, Located::Remote)? {
                            return Ok(());
                        }
                    }
                }
            }
            self.block += 1;
        }
        Ok(())
    }

    /// True requires an IO CQE; a hit already filled the current response block.
    fn chunk(&mut self, hash: cas_core::chunk_index::Hash) -> io::Result<bool> {
        let view = self.plan.manifest().cloned().expect("manifest chunk");
        let block = self.absolute_block();
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        let destination =
            &mut buffer.as_mut_slice()[self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE];
        let located =
            self.sources
                .as_ref()
                .expect("host sources")
                .locate(&view, block, hash, destination)?;
        if matches!(located, Located::Cached) {
            if let Some(trace) = self.trace() {
                trace.cache_hits += 1;
            }
            return Ok(false);
        }
        self.chunk_located(hash, located)
    }

    /// Schedule a chunk the shared resolution could not serve from the cache:
    /// coalesce through the fetch registry, then a local store read or a GET.
    fn chunk_located(
        &mut self,
        hash: cas_core::chunk_index::Hash,
        located: Located,
    ) -> io::Result<bool> {
        self.chunk_hash = Some(hash);
        let view = self.plan.manifest().cloned().expect("manifest chunk");
        let block = self.absolute_block();
        let chunk_bytes = self.chunk_bytes();
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        let destination =
            &mut buffer.as_mut_slice()[self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE];
        let joined = self.sources.as_ref().expect("host sources").join(
            self.fetches.as_ref().expect("host fetch registry"),
            hash,
            chunk_bytes,
            self.io
                .permit
                ._read
                .as_ref()
                .expect("read byte reservation"),
            self.metadata.as_ref().expect("host metadata"),
            &view,
            block,
            destination,
            None,
        )?;
        match joined {
            Joined::Wait(waiter) => {
                self.shared_io = true;
                self.stage = Stage::Waiting(waiter);
                return Ok(true);
            }
            Joined::Served => {
                if let Some(trace) = self.trace() {
                    trace.cache_hits += 1;
                }
                return Ok(false);
            }
            Joined::Full => {
                return Err(io::Error::other(
                    "admitted read exceeded host fetch capacity",
                ));
            }
            Joined::Lead(fetch) => self.fetch = Some(fetch),
        }
        self.shared_io = true;
        self.stage = match located {
            Located::Cached => unreachable!("cache hits never reach the registry"),
            Located::Store(read) => Stage::Header(read),
            Located::Remote => {
                let peer = self
                    .peer
                    .as_ref()
                    .ok_or_else(|| io::Error::other("manifest references an unavailable chunk"))?;
                let ticket = peer.fetch(hash)?;
                // A reply that already failed (refused connection, EOF, a
                // corrupt payload) is raced too: arming starts the challenger
                // now. Only a verified reply leaves nothing to race.
                if !ticket.verified() {
                    // Force-remote never consulted the store above; the race
                    // arm may use a verified local plan explicitly.
                    let local = if peer.wants_local_plan() {
                        self.sources
                            .as_ref()
                            .and_then(|sources| sources.race_plan(hash))
                    } else {
                        None
                    };
                    peer.race(&ticket, hash, local);
                }
                Stage::Remote(ticket)
            }
        };
        if matches!(&self.stage, Stage::Remote(ticket) if ticket.ready()) && self.finish_remote()? {
            self.stage = Stage::Done;
            return Ok(false);
        }
        Ok(true)
    }

    pub fn entry(&mut self) -> squeue::Entry {
        if let Stage::Waiting(waiter) = &self.stage {
            return opcode::PollAdd::new(types::Fd(waiter.notification()), libc::POLLIN as u32)
                .build();
        }
        if let Stage::Remote(ticket) = &self.stage {
            return opcode::PollAdd::new(types::Fd(ticket.notification()), libc::POLLIN as u32)
                .build();
        }
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        let (file, pointer, bytes, offset) = match &self.stage {
            Stage::Staging(index) => {
                let range = &self.plan.ranges()[*index];
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
                (range.file(), pointer, range.input_bytes(), range.offset())
            }
            Stage::Manifest { read, offset } => (
                read.file(),
                self.page.as_mut().unwrap().as_mut_slice().as_mut_ptr(),
                BLOCK_SIZE,
                *offset,
            ),
            Stage::Header(read) => (
                read.file(),
                self.page.as_mut().unwrap().as_mut_slice().as_mut_ptr(),
                BLOCK_SIZE,
                read.header_offset(),
            ),
            Stage::Payload(read) => (
                read.file(),
                self.fetch
                    .as_mut()
                    .expect("owned fetch payload")
                    .payload_mut()
                    .as_mut_ptr(),
                read.bytes(),
                read.offset(),
            ),
            Stage::Waiting(_) | Stage::Remote(_) => {
                unreachable!("notification submits readiness above")
            }
            Stage::Done => unreachable!("completed read has no SQE"),
        };
        opcode::Read::new(types::Fd(file.as_raw_fd()), pointer, bytes as u32)
            .offset(offset)
            .build()
    }

    pub fn expected_bytes(&self) -> usize {
        match self.stage {
            Stage::Staging(index) => self.plan.ranges()[index].input_bytes(),
            Stage::Manifest { .. } | Stage::Header(_) => BLOCK_SIZE,
            Stage::Payload(ref read) => read.bytes(),
            Stage::Waiting(_) | Stage::Remote(_) => libc::POLLIN as usize,
            Stage::Done => unreachable!("completed read has no CQE"),
        }
    }

    fn chunk_bytes(&self) -> usize {
        self.plan
            .manifest()
            .map_or(BLOCK_SIZE, |view| view.chunk_size().bytes())
    }

    /// Publish the current chunk's verified payload through the shared rules
    /// (fill, guest-block copy, waiter completion).
    fn publish(&mut self, remote: Option<crate::peer::metrics::Phases>) -> io::Result<()> {
        let fetch = self.fetch.take().expect("owned fetch payload");
        let view = self.plan.manifest().cloned().expect("manifest chunk");
        let block = self.absolute_block();
        let hash = self.chunk_hash.expect("planned chunk hash");
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        let destination =
            &mut buffer.as_mut_slice()[self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE];
        fetch.publish(
            self.sources.as_ref().expect("host sources"),
            hash,
            remote,
            &view,
            block,
            destination,
        )
    }

    pub fn advance(&mut self) -> io::Result<bool> {
        let started = self.trace().map(|_| Instant::now());
        let scope = started.map(|_| cas_core::io_metrics::Scope::enter());
        let result = self.advance_inner();
        if let Some(scope) = scope {
            let counters = scope.finish();
            self.trace().unwrap().synchronous.add(counters);
        }
        if let (Some(trace), Some(started)) = (self.trace(), started) {
            trace.advance_ns += nanos(started.elapsed());
        }
        result
    }

    /// False keeps the remote stage armed: no source has verified yet.
    fn finish_remote(&mut self) -> io::Result<bool> {
        let Stage::Remote(ticket) = &self.stage else {
            unreachable!()
        };
        let (phases, result) = ticket.finish(
            self.fetch
                .as_mut()
                .expect("remote fetch payload")
                .payload_mut(),
        );
        if matches!(result, Ok(false)) {
            return Ok(false);
        }
        if let Some(trace) = &mut self.io.permit.remote {
            trace.fetched(phases);
        }
        result?;
        self.publish(Some(phases))?;
        Ok(true)
    }

    fn advance_inner(&mut self) -> io::Result<bool> {
        match &mut self.stage {
            Stage::Staging(index) => {
                let Operation::Read { buffer, .. } = &mut self.io.operation else {
                    unreachable!()
                };
                let range = &self.plan.ranges()[*index];
                if range.direct_to_response() {
                    range.verify(&buffer.as_slice()[range.destination()])?;
                } else {
                    let input = &self.scratch.as_ref().unwrap().as_slice()[..range.input_bytes()];
                    range.verify(input)?;
                    buffer.as_mut_slice()[range.destination()]
                        .copy_from_slice(&input[range.source()]);
                }
                *index += 1;
                if *index == self.plan.ranges().len() {
                    self.scratch.take();
                    self.next_block()?;
                }
            }
            Stage::Manifest { read, offset } => {
                match read.accept_cached(
                    *offset,
                    self.page.as_ref().unwrap().as_slice(),
                    self.sources.as_ref().expect("host sources").pages(),
                )? {
                    LookupState::Page { offset: next, .. } => *offset = next,
                    LookupState::Complete(Some(hash)) => {
                        if !self.chunk(hash)? {
                            self.block += 1;
                            self.next_block()?;
                        }
                    }
                    LookupState::Complete(None) => {
                        self.block += 1;
                        self.next_block()?;
                    }
                }
            }
            Stage::Header(read) => {
                self.stage = Stage::Payload(read.payload(self.page.as_ref().unwrap().as_slice())?);
                #[cfg(test)]
                if let Some(control) = &self.control {
                    let pause = control.lock().unwrap().before_fetch.take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
            }
            Stage::Payload(read) => {
                read.verify(self.fetch.as_ref().expect("owned fetch payload").payload())?;
                #[cfg(test)]
                if let Some(control) = &self.control {
                    let pause = control.lock().unwrap().fetch.take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                self.publish(None)?;
                self.block += 1;
                self.next_block()?;
            }
            Stage::Remote(_) => {
                if self.finish_remote()? {
                    self.block += 1;
                    self.next_block()?;
                }
            }
            Stage::Waiting(waiter) => {
                let fetched = waiter
                    .poll()?
                    .ok_or_else(|| io::Error::other("fetch readiness preceded publication"))?;
                if let Some(phases) = fetched.remote
                    && let Some(trace) = &mut self.io.permit.remote
                {
                    trace.fetched(phases);
                }
                let view = self.plan.manifest().cloned().expect("manifest chunk");
                let block = self.absolute_block();
                let Operation::Read { buffer, .. } = &mut self.io.operation else {
                    unreachable!()
                };
                crate::local::chunk::copy_chunk(
                    &view,
                    block,
                    fetched.bytes.as_slice(),
                    &mut buffer.as_mut_slice()
                        [self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE],
                )?;
                self.block += 1;
                self.next_block()?;
            }
            Stage::Done => return Err(io::Error::other("duplicate completed read CQE")),
        }
        Ok(self.done())
    }
}
