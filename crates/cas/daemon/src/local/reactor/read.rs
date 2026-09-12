use super::*;
use cas_core::{
    budget::BudgetAllocator,
    manifest::{file as manifest, tree::LookupState},
    store::file as store,
};

enum Stage {
    Staging(usize),
    Manifest { read: manifest::Read, offset: u64 },
    Header(store::Read),
    Payload(store::Payload),
    Done,
}

pub(super) struct Read {
    // Scratch and pinned plans drop before the response can release its credits.
    scratch: Option<AlignedBuffer>,
    page: Option<AlignedBuffer<BudgetAllocator>>,
    plan: ReadPlan,
    reader: Option<store::Reader>,
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
    pub fn shared_io(&self) -> bool {
        self.shared_io
    }
    pub fn into_io(self) -> Io {
        self.io
    }

    fn next_block(&mut self) -> io::Result<()> {
        self.shared_io = false;
        self.stage = Stage::Done;
        let Some(view) = self.plan.manifest() else {
            return Ok(());
        };
        while self.block < self.plan.bytes() / BLOCK_SIZE {
            if !self.plan.staged(self.block) {
                let read =
                    view.lookup(self.plan.offset() / BLOCK_SIZE as u64 + self.block as u64)?;
                match read.state()? {
                    LookupState::Page { offset, .. } => {
                        self.stage = Stage::Manifest { read, offset };
                        return Ok(());
                    }
                    LookupState::Complete(Some(hash)) => {
                        self.chunk(hash)?;
                        return Ok(());
                    }
                    LookupState::Complete(None) => (),
                }
            }
            self.block += 1;
        }
        Ok(())
    }

    fn chunk(&mut self, hash: cas_core::chunk_index::Hash) -> io::Result<()> {
        self.shared_io = true;
        let read = self
            .reader
            .as_ref()
            .ok_or_else(|| io::Error::other("missing chunk reader"))?
            .plan(hash)?
            .ok_or_else(|| io::Error::other("manifest references an unavailable chunk"))?;
        self.stage = Stage::Header(read);
        Ok(())
    }

    pub fn entry(&mut self) -> squeue::Entry {
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
                buffer.as_mut_slice()[self.block * BLOCK_SIZE..].as_mut_ptr(),
                BLOCK_SIZE,
                read.offset(),
            ),
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
            Stage::Done => unreachable!("completed read has no CQE"),
        }
    }

    pub fn advance(&mut self) -> io::Result<bool> {
        let Operation::Read { buffer, .. } = &mut self.io.operation else {
            unreachable!()
        };
        match &mut self.stage {
            Stage::Staging(index) => {
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
                match read.accept(*offset, self.page.as_ref().unwrap().as_slice())? {
                    LookupState::Page { offset: next, .. } => *offset = next,
                    LookupState::Complete(Some(hash)) => self.chunk(hash)?,
                    LookupState::Complete(None) => {
                        self.block += 1;
                        self.next_block()?;
                    }
                }
            }
            Stage::Header(read) => {
                self.stage = Stage::Payload(read.payload(self.page.as_ref().unwrap().as_slice())?);
            }
            Stage::Payload(read) => {
                read.verify(
                    &buffer.as_slice()[self.block * BLOCK_SIZE..(self.block + 1) * BLOCK_SIZE],
                )?;
                self.block += 1;
                self.next_block()?;
            }
            Stage::Done => return Err(io::Error::other("duplicate completed read CQE")),
        }
        Ok(self.done())
    }
}
