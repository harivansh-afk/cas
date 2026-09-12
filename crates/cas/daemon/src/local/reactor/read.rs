use super::*;
use cas_core::{
    budget::BudgetAllocator,
    cache::fills::{Leader, Lookup, Registry, Waiter},
    manifest::{file as manifest, tree::LookupState},
    store::file as store,
};

enum Stage {
    Staging(usize),
    Manifest { read: manifest::Read, offset: u64 },
    Header(store::Read),
    Payload(store::Payload),
    Waiting(Waiter<Fetched>),
    Done,
}

pub(super) struct Read {
    // Scratch and pinned plans drop before the response can release its credits.
    scratch: Option<AlignedBuffer>,
    page: Option<AlignedBuffer<BudgetAllocator>>,
    plan: ReadPlan,
    reader: Option<store::Reader>,
    cache: Option<BudgetArc<cas_core::cache::Cache>>,
    page_cache: Option<BudgetArc<manifest::PageCache>>,
    chunk_hash: Option<cas_core::chunk_index::Hash>,
    fetches: Option<Fetches>,
    leader: Option<Leader<Fetched>>,
    fetched: Option<BudgetArc<Fetched>>,
    metadata: Option<Arc<Budget>>,
    #[cfg(test)]
    control: Option<Arc<Mutex<host::tests::Control>>>,
    io: Io,
    stage: Stage,
    block: usize,
    shared_io: bool,
}

impl Read {
    pub fn new(plan: ReadPlan, io: Io) -> Self {
        Self {
            scratch: None,
            page: None,
            plan,
            reader: None,
            cache: None,
            page_cache: None,
            chunk_hash: None,
            fetches: None,
            leader: None,
            fetched: None,
            metadata: None,
            #[cfg(test)]
            control: None,
            io,
            stage: Stage::Done,
            block: 0,
            shared_io: false,
        }
    }

    pub fn prepare(&mut self, port: Option<&host::Port>) -> io::Result<()> {
        if self.plan.manifest().is_some()
            && (0..self.plan.bytes() / BLOCK_SIZE).any(|block| !self.plan.staged(block))
        {
            let port = port.ok_or_else(|| io::Error::other("manifest read has no shared store"))?;
            self.reader = Some(port.reader());
            self.cache = Some(port.cache());
            self.page_cache = Some(port.page_cache());
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
        matches!(self.stage, Stage::Waiting(_))
    }
    pub fn shared_io(&self) -> bool {
        self.shared_io
    }
    pub fn into_io(self) -> Io {
        self.io
    }

    fn next_block(&mut self) -> io::Result<()> {
        self.shared_io = false;
        self.stage = Stage::Done;
        let Some(view) = self.plan.manifest().cloned() else {
            return Ok(());
        };
        while self.block < self.plan.bytes() / BLOCK_SIZE {
            if !self.plan.staged(self.block) {
                let mut read =
                    view.lookup(self.plan.offset() / BLOCK_SIZE as u64 + self.block as u64)?;
                match read.cached(self.page_cache.as_ref().expect("host page cache"))? {
                    LookupState::Page { offset, .. } => {
                        self.stage = Stage::Manifest { read, offset };
                        return Ok(());
                    }
                    LookupState::Complete(Some(hash)) => {
                        if self.chunk(hash)? {
                            return Ok(());
                        }
                    }
                    LookupState::Complete(None) => (),
                }
            }
            self.block += 1;
        }
        Ok(())
    }

    /// True requires an IO CQE; a hit already filled the current response block.
    fn chunk(&mut self, hash: cas_core::chunk_index::Hash) -> io::Result<bool> {
        self.chunk_hash = Some(hash);
        if let Some(bytes) = self.cache.as_ref().and_then(|cache| cache.get(&hash)) {
            self.response_block().copy_from_slice(bytes.as_slice());
            return Ok(false);
        }
        match Registry::lookup(self.fetches.as_ref().expect("host fetch registry"), hash)? {
            Some(Lookup::Waiter(waiter)) => {
                self.shared_io = true;
                self.stage = Stage::Waiting(waiter);
                return Ok(true);
            }
            Some(Lookup::Leader(leader)) => {
                self.leader = Some(leader);
                let bytes = AlignedBuffer::try_new_in(BLOCK_SIZE, allocator_api2::alloc::Global)?;
                self.fetched = Some(BudgetArc::try_new(
                    Fetched {
                        bytes,
                        _credits: self
                            .io
                            .permit
                            ._read
                            .as_ref()
                            .expect("read byte reservation")
                            .clone(),
                    },
                    self.metadata.as_ref().expect("host metadata"),
                )?);
            }
            None => {
                return Err(io::Error::other(
                    "admitted read exceeded host fetch capacity",
                ));
            }
        }
        // A previous leader may have filled the cache between our miss and
        // registry lookup. Publish that value through our new cell as well.
        if let Some(bytes) = self.cache.as_ref().and_then(|cache| cache.peek(&hash)) {
            let mut fetched = self.fetched.take().expect("owned fetch payload");
            fetched
                .get_mut()
                .expect("unpublished fetch is unique")
                .bytes
                .as_mut_slice()
                .copy_from_slice(bytes.as_slice());
            self.response_block().copy_from_slice(bytes.as_slice());
            self.leader
                .take()
                .expect("fetch leader")
                .complete(fetched)?;
            return Ok(false);
        }
        self.shared_io = true;
        let read = self
            .reader
            .as_ref()
            .ok_or_else(|| io::Error::other("missing chunk reader"))?
            .plan(hash)?
            .ok_or_else(|| io::Error::other("manifest references an unavailable chunk"))?;
        self.stage = Stage::Header(read);
        Ok(true)
    }

    pub fn entry(&mut self) -> squeue::Entry {
        if let Stage::Waiting(waiter) = &self.stage {
            return opcode::PollAdd::new(types::Fd(waiter.notification()), libc::POLLIN as u32)
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
                self.fetched
                    .as_mut()
                    .unwrap()
                    .get_mut()
                    .expect("unpublished fetch is unique")
                    .bytes
                    .as_mut_slice()
                    .as_mut_ptr(),
                BLOCK_SIZE,
                read.offset(),
            ),
            Stage::Waiting(_) => unreachable!("waiter submits readiness above"),
            Stage::Done => unreachable!("completed read has no SQE"),
        };
        opcode::Read::new(types::Fd(file.as_raw_fd()), pointer, bytes as u32)
            .offset(offset)
            .build()
    }

    pub fn expected_bytes(&self) -> usize {
        match self.stage {
            Stage::Staging(index) => self.plan.ranges()[index].input_bytes(),
            Stage::Manifest { .. } | Stage::Header(_) | Stage::Payload(_) => BLOCK_SIZE,
            Stage::Waiting(_) => libc::POLLIN as usize,
            Stage::Done => unreachable!("completed read has no CQE"),
        }
    }

    fn response_block(&mut self) -> &mut [u8] {
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        &mut buffer.as_mut_slice()[self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE]
    }

    pub fn advance(&mut self) -> io::Result<bool> {
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
                    self.page_cache.as_ref().expect("host page cache"),
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
                let fetched = self.fetched.take().expect("owned fetch payload");
                let bytes = fetched.bytes.as_slice();
                read.verify(bytes)?;
                #[cfg(test)]
                if let Some(control) = &self.control {
                    let pause = control.lock().unwrap().fetch.take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                if let Some(cache) = &self.cache {
                    match cache.fill(self.chunk_hash.expect("planned chunk hash"), bytes) {
                        Ok(_) => (),
                        Err(error) if error.kind() == io::ErrorKind::OutOfMemory => (),
                        Err(error) => return Err(error),
                    }
                }
                self.response_block().copy_from_slice(bytes);
                self.leader
                    .take()
                    .expect("fetch leader")
                    .complete(fetched)?;
                self.block += 1;
                self.next_block()?;
            }
            Stage::Waiting(waiter) => {
                let fetched = waiter
                    .poll()?
                    .ok_or_else(|| io::Error::other("fetch readiness preceded publication"))?;
                self.response_block()
                    .copy_from_slice(fetched.bytes.as_slice());
                self.block += 1;
                self.next_block()?;
            }
            Stage::Done => return Err(io::Error::other("duplicate completed read CQE")),
        }
        Ok(self.done())
    }
}
