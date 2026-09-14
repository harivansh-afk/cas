use super::super::insert::Publication;
use super::*;
use arrayvec::ArrayVec;

impl Collection<'_> {
    pub(super) fn clean(&mut self, victim: Victim) -> io::Result<Collected> {
        let (file, end, batches) = {
            let state = self.guard.shared.lock();
            state.healthy()?;
            require(
                !self.guard.shared.tickets.status().failed,
                "chunk collection tickets failed",
            )?;
            let segment = &state.segments[candidate(&state, victim.ticket)?];
            require(
                Arc::strong_count(&segment.file) == 1,
                "chunk victim file pinned",
            )?;
            (
                Arc::clone(&segment.file),
                segment.end,
                segment.batches.len(),
            )
        };
        require(file.metadata()?.len() == end, "chunk victim EOF changed")?;
        direct::read_bytes(&file, self.header.as_mut_slice(), 0)?;
        require(
            SegmentHeader::decode(self.header.as_slice())?
                == SegmentHeader {
                    store: self.store.config.store,
                    number: victim.ticket,
                    capacity: self.store.config.segment_bytes,
                },
            "chunk victim header changed",
        )?;
        let mut cleaned = Collected::default();
        if victim.destination_bytes != 0 {
            for index in 0..batches {
                let location = {
                    let state = self.guard.shared.lock();
                    state.segments[candidate(&state, victim.ticket)?].batches[index]
                };
                let publications = self.load_batch(&file, victim.ticket, index, location)?;
                if publications.is_empty() {
                    continue;
                }
                let builder = self
                    .builder
                    .take()
                    .expect("collection owns output allocation");
                let (bytes, builder) =
                    self.store
                        .append(builder, &publications, cleaned.chunks_copied == 0)?;
                self.builder = Some(builder);
                cleaned.chunks_copied += publications.len();
                cleaned.encoded_bytes_copied += bytes as u64;
            }
            require(
                cleaned.chunks_copied == victim.live_chunks,
                "chunk victim live data unavailable",
            )?;
            let (destination, end) = {
                let state = self.guard.shared.lock();
                let segment = state.segments.last().expect("copied destination");
                (Arc::clone(&segment.file), segment.end)
            };
            trim(&destination, end)?;
        } else if victim.live_chunks != 0 {
            trim(&file, end)?;
            return Ok(cleaned);
        }

        let highest = self.guard.shared.tickets.status();
        require(!highest.failed, "chunk collection tickets failed")?;
        if victim.ticket == highest.highest {
            // A greatest dead ticket keeps its original header and never reuses
            // offsets in this owner, even if a cache retained an old identity.
            require(
                victim.live_chunks == 0,
                "live greatest ticket cannot be truncated",
            )?;
            trim(&file, BLOCK_SIZE as u64)?;
            let mut state = self.guard.shared.lock();
            let index = candidate(&state, victim.ticket)?;
            let segment = &mut state.segments[index];
            segment.batches.clear();
            segment.end = BLOCK_SIZE as u64;
            segment.file_bytes = segment.end;
            segment.sealed = true;
            cleaned.headers_retained = 1;
        } else {
            require(victim.ticket < highest.highest, "chunk ticket order")?;
            {
                let state = self.guard.shared.lock();
                require(
                    Arc::strong_count(&state.segments[candidate(&state, victim.ticket)?].file) == 2,
                    "chunk victim gained a file pin",
                )?;
            }
            self.guard
                .shared
                .directory
                .remove(&segments::name(victim.ticket))?;
            self.guard.shared.directory.sync()?;
            let mut state = self.guard.shared.lock();
            let index = candidate(&state, victim.ticket)?;
            state.segments.remove(index);
            cleaned.segments_removed = 1;
        }
        Ok(cleaned)
    }

    fn load_batch(
        &mut self,
        file: &File,
        ticket: u64,
        index: usize,
        location: BatchLocation,
    ) -> io::Result<ArrayVec<Publication, MAX_CHUNKS>> {
        let offset = u64::from(location.offset);
        direct::read_bytes(file, self.header.as_mut_slice(), offset)?;
        let header = Header::decode(self.header.as_slice())?;
        let batch_id = index as u64 + 1;
        require(
            batch_identity(&header, ticket, batch_id, offset, location.chunks),
            "chunk collection batch identity",
        )?;
        let payload = &mut self.payload.as_mut_slice()[..header.payload_bytes()];
        direct::read_bytes(file, payload, offset + BLOCK_SIZE as u64)?;
        header.verify_payload(payload)?;
        let mut publications = ArrayVec::new();
        let state = self.guard.shared.lock();
        let builder = self
            .builder
            .as_mut()
            .expect("collection owns output allocation");
        for descriptor in header.descriptors() {
            let address = Address::new(
                ticket,
                offset + BLOCK_SIZE as u64 + u64::from(descriptor.payload_offset),
            )?;
            if state.index.marked_at(&descriptor.hash, address) {
                let start = descriptor.payload_offset as usize;
                let block = payload[start..start + BLOCK_SIZE].try_into().unwrap();
                let chunk =
                    Chunk::new(block).ok_or_else(|| io::Error::other("live chunk became zero"))?;
                require(chunk.hash() == descriptor.hash, "live chunk hash changed")?;
                builder.push(chunk)?;
                publications.push(Publication {
                    hash: descriptor.hash,
                    previous: Some(address),
                });
            }
        }
        Ok(publications)
    }
}

fn candidate(state: &State, ticket: u64) -> io::Result<usize> {
    state
        .segments
        .binary_search_by_key(&ticket, |s| s.header.number)
        .map_err(|_| io::Error::other("chunk collection candidate disappeared"))
}

fn trim(file: &File, end: u64) -> io::Result<()> {
    // Probe support before mutation; success alone cannot prove tail removal.
    direct::next_extent(file, end)?;
    direct::truncate(file, end)?;
    direct::sync_data(file)?;
    require(
        direct::next_extent(file, end)?.is_none(),
        "chunk tail allocation remains",
    )
}
