use super::*;

struct Rejected {
    number: u64,
    file: File,
}

/// The coordinator verifies every required manifest hash before consuming this
/// inspection. No index entry is available to normal reads until recovery sync.
pub struct Inspection {
    store: Store,
    rejected: Vec<Rejected, BudgetAllocator>,
}

impl Inspection {
    pub fn contains(&self, hash: &Hash) -> bool {
        self.store.shared.lock().index.get(hash).is_some()
    }

    pub fn status(&self) -> Status {
        self.store.status()
    }

    pub fn recover(mut self) -> io::Result<Store> {
        // Inspection has never exposed a reader: recovery owns the state
        // exclusively and performs IO without holding a lookup mutex.
        let shared = Arc::get_mut(&mut self.store.shared).expect("private inspection owner");
        let directory = &shared.directory;
        let state = shared.state.get_mut().expect("private inspection state");
        for segment in &state.segments {
            if segment.file_bytes > segment.end {
                directory.archive(
                    &segments::name(segment.header.number),
                    &segment.file,
                    segment.end,
                )?;
                segment.file.set_len(segment.end)?;
            }
            direct::sync_data(&segment.file)?;
        }
        for rejected in &self.rejected {
            let name = segments::name(rejected.number);
            directory.archive(&name, &rejected.file, 0)?;
            fs::remove_file(directory.path.join(name))?;
            directory.sync()?;
        }
        for segment in &mut state.segments {
            segment.file_bytes = segment.end;
        }
        state.failed = false;
        Ok(self.store)
    }
}

impl Store {
    pub fn inspect(
        tickets: Arc<Tickets>,
        config: Config,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Inspection> {
        config.validate()?;
        let directory = Directory::open(&tickets.root().join("chunks"))?;
        let mut numbers = Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata)));
        for entry in fs::read_dir(&directory.path)? {
            let entry = entry?;
            if entry.file_name() == "rejected" {
                continue;
            }
            let filename = entry.file_name();
            let filename = filename
                .to_str()
                .ok_or_else(|| io::Error::other("chunk filename encoding"))?;
            let number = segments::number(filename)
                .ok_or_else(|| io::Error::other("invalid chunk filename"))?;
            require(
                filename == segments::name(number) && entry.file_type()?.is_file(),
                "invalid chunk file",
            )?;
            reserve(&mut numbers, 1)?;
            numbers.push(number);
        }
        numbers.sort_unstable();
        let mut rejected = Vec::new_in(BudgetAllocator::new(Arc::clone(&metadata)));
        let mut scratch =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(&metadata)))?;
        let mut payload = AlignedBuffer::try_new_in(
            MAX_CHUNKS * BLOCK_SIZE,
            BudgetAllocator::new(Arc::clone(&io_memory)),
        )?;
        let mut store = Self::empty(directory, tickets, config, metadata, io_memory);
        let shared = Arc::get_mut(&mut store.shared).expect("private inspection owner");
        let state = shared.state.get_mut().expect("private inspection state");
        state.failed = true;
        for number in numbers {
            let file = direct::open(&shared.directory.path.join(segments::name(number)), false)?;
            direct::Alignment::query(&file)?;
            let file_bytes = file.metadata()?.len();
            if file_bytes < BLOCK_SIZE as u64 {
                reserve(&mut rejected, 1)?;
                rejected.push(Rejected { number, file });
                continue;
            }
            direct::read_bytes(&file, scratch.as_mut_slice(), 0)?;
            let header = match SegmentHeader::decode(scratch.as_slice()) {
                Ok(header) => header,
                Err(_) if file_bytes == BLOCK_SIZE as u64 => {
                    reserve(&mut rejected, 1)?;
                    rejected.push(Rejected { number, file });
                    continue;
                }
                Err(error) => return Err(error),
            };
            require(
                header.number == number
                    && header.store == config.store
                    && header.capacity == config.segment_bytes,
                "chunk segment identity/geometry mismatch",
            )?;
            require(
                file_bytes <= header.capacity,
                "chunk file exceeds segment capacity",
            )?;
            let mut segment = Segment {
                file: Arc::new(file),
                header,
                batches: Vec::new_in(BudgetAllocator::new(Arc::clone(&shared.metadata))),
                end: BLOCK_SIZE as u64,
                file_bytes,
                next_batch: 1,
                next_ordinal: 1,
                sealed: false,
            };
            inspect_batches(
                &mut segment,
                &mut state.index,
                scratch.as_mut_slice(),
                payload.as_mut_slice(),
            )?;
            reserve(&mut state.segments, 1)?;
            state.segments.push(segment);
        }
        Ok(Inspection { store, rejected })
    }
}

fn inspect_batches(
    segment: &mut Segment,
    index: &mut Index,
    scratch: &mut [u8],
    payload: &mut [u8],
) -> io::Result<()> {
    while segment.file_bytes - segment.end >= BLOCK_SIZE as u64 {
        direct::read_bytes(&segment.file, scratch, segment.end)?;
        let Ok(header) = Header::decode(scratch) else {
            break;
        };
        require(
            header.segment() == segment.header.number
                && header.batch() == segment.next_batch
                && header.first() == segment.next_ordinal,
            "chunk batch identity/order mismatch",
        )?;
        let bytes = header.payload_bytes();
        let end = segment.end + (BLOCK_SIZE + bytes) as u64;
        require(
            end <= segment.header.capacity,
            "chunk batch exceeds segment capacity",
        )?;
        if end > segment.file_bytes {
            break;
        }
        direct::read_bytes(
            &segment.file,
            &mut payload[..bytes],
            segment.end + BLOCK_SIZE as u64,
        )?;
        if header.verify_payload(&payload[..bytes]).is_err() {
            break;
        }
        reserve(&mut segment.batches, 1)?;
        for descriptor in header.descriptors() {
            index.insert(
                descriptor.hash,
                Address::new(
                    segment.header.number,
                    segment.end + BLOCK_SIZE as u64 + u64::from(descriptor.payload_offset),
                )?,
            )?;
        }
        segment.batches.push(BatchLocation {
            offset: segment.end as u32,
            chunks: header.descriptors().len() as u16,
        });
        segment.end = end;
        segment.next_batch = segment
            .next_batch
            .checked_add(1)
            .ok_or_else(|| io::Error::other("chunk batch IDs exhausted"))?;
        segment.next_ordinal = header
            .last()
            .checked_add(1)
            .ok_or_else(|| io::Error::other("chunk ordinals exhausted"))?;
    }
    Ok(())
}
