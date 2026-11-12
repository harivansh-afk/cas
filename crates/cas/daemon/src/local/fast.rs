//! Frontend-inline settled reads (`--fast-settled-reads`), off by default.
//!
//! A read bypasses the reactor only when **all** of these hold at enqueue:
//!
//! 1. The image is host attached, concurrent, healthy and not paused.
//! 2. The reactor's latest status has `published == admitted` (every mutation
//!    this frontend ever admitted, including an unsealed batch, is published:
//!    no in-flight write exists) and `compacted == published` (D == P: the
//!    staging index retains no mapping newer than D, so no staged write can
//!    take precedence over the manifest for any block).
//! 3. The manifest view published by the reactor is exactly the one at that D.
//! 4. No FLUSH admitted by this frontend is outstanding.
//! 5. Every block resolves without local IO: its manifest lookup completes
//!    from the page cache and yields a hole, a clean-cache hit, or a chunk
//!    that the frozen path would also fetch from a peer (force-remote, or no
//!    local store copy). A local store read or a manifest page load declines.
//! 6. Bounded state: one pending-table slot and one GET slot per remote chunk.
//!
//! Why the guest-visible contract is preserved: this frontend is the only
//! admitter of mutations for its image, so (2) means that at the check every
//! mutation that could precede the read in guest order is already published
//! and compacted, and the manifest at D is the complete image state. Nothing
//! staged exists that WAL precedence could select. A mutation discovered after
//! the check is younger than the read and may be excluded, exactly as the
//! frozen path may plan a read before or after a concurrent write publishes.
//! Credits, the host admission entry and fairness release stay in the permit
//! until completion, which flows through the same `receive` path as the
//! reactor's. A remote failure fails the read, the image gate and the shared
//! host gate like the frozen path. Reads that cannot establish these facts are
//! sent to the reactor unchanged.
use super::*;
use cas_core::{
    aligned::AlignedBuffer,
    cache::Cache,
    manifest::{file::PageCache, tree::LookupState},
    store::file::Reader,
};

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Counters {
    pub inline_reads: u64,
    pub remote_reads: u64,
    pub declined_unsettled: u64,
    pub declined_flush: u64,
    pub declined_io: u64,
    pub declined_capacity: u64,
    pub failed_reads: u64,
}

struct Remote {
    block: usize,
    hash: cas_core::chunk_index::Hash,
    ticket: crate::peer::Ticket,
    done: bool,
}

struct Pending {
    id: u64,
    buffer: AlignedBuffer,
    permit: Permit,
    remote: BudgetVec<Remote, BudgetAllocator>,
}

pub(super) struct Context {
    notify: EventFd,
    peer: Option<BudgetArc<crate::peer::Router>>,
    peer_metrics: Option<BudgetArc<crate::peer::metrics::Metrics>>,
    force_remote: bool,
    cache_remote: bool,
    reader: Reader,
    cache: BudgetArc<Cache>,
    pages: BudgetArc<PageCache>,
    gate: BudgetArc<state::HostGate>,
    metadata: Arc<Budget>,
    pending: BudgetVec<Pending, BudgetAllocator>,
    flushes: usize,
    pub(super) counters: Counters,
}

#[expect(
    clippy::large_enum_variant,
    reason = "A declined read hands its buffer and permit straight back to the reactor path"
)]
pub(super) enum Outcome {
    Taken,
    Declined(Operation, Permit),
}

impl Context {
    pub fn new(port: &host::Port, event: &EventFd) -> io::Result<Self> {
        let metadata = port.metadata();
        Ok(Self {
            notify: event.try_clone()?,
            peer: port.peer(),
            peer_metrics: port.peer_metrics(),
            force_remote: port.peer_config().peer_force_remote,
            cache_remote: port.peer_config().peer_cache,
            reader: port.reader(),
            cache: port.cache(),
            pages: port.page_cache(),
            gate: port.host_gate(),
            pending: reserved_vec(IMAGE_REQUEST_LIMIT, &metadata)?,
            metadata,
            flushes: 0,
            counters: Counters::default(),
        })
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    fn decline(
        &mut self,
        count: fn(&mut Counters) -> &mut u64,
        offset: u64,
        buffer: AlignedBuffer,
        permit: Permit,
    ) -> io::Result<Outcome> {
        *count(&mut self.counters) += 1;
        Ok(Outcome::Declined(
            Operation::Read { offset, buffer },
            permit,
        ))
    }
    pub fn flush_enqueued(&mut self) {
        self.flushes += 1;
    }
    pub fn flush_completed(&mut self) {
        self.flushes = self.flushes.saturating_sub(1);
    }

    /// Serve the read inline, start its remote fetches, or decline it.
    pub fn enqueue(
        &mut self,
        shared: &Shared,
        admitted: u64,
        id: u64,
        operation: Operation,
        mut permit: Permit,
        output: &mut BudgetQueue<Completed>,
    ) -> io::Result<Outcome> {
        let Operation::Read { offset, buffer } = operation else {
            return Ok(Outcome::Declined(operation, permit));
        };
        if self.flushes != 0 {
            return self.decline(|c| &mut c.declined_flush, offset, buffer, permit);
        }
        let status = *shared.final_status.lock().expect("status poisoned");
        if status.published != admitted || status.compacted != status.published {
            return self.decline(|c| &mut c.declined_unsettled, offset, buffer, permit);
        }
        let view = match &*shared.manifest.lock().expect("manifest poisoned") {
            Some((compacted, view)) if *compacted == status.compacted => view.clone(),
            _ => return self.decline(|c| &mut c.declined_unsettled, offset, buffer, permit),
        };
        if view.commit().durable != status.compacted {
            return self.decline(|c| &mut c.declined_unsettled, offset, buffer, permit);
        }
        let bytes = buffer.as_slice().len();
        let end = offset.checked_add(bytes as u64);
        if bytes == 0
            || !bytes.is_multiple_of(BLOCK_SIZE)
            || !offset.is_multiple_of(BLOCK_SIZE as u64)
            || end.is_none_or(|end| end > status.image_bytes)
            || bytes > MAX_REQUEST_BYTES
        {
            // The reactor returns the actual range error.
            return self.decline(|c| &mut c.declined_io, offset, buffer, permit);
        }
        if self.pending.len() == self.pending.capacity() {
            return self.decline(|c| &mut c.declined_capacity, offset, buffer, permit);
        }
        // Resolve every block before issuing any request, so a declined read
        // leaves no cancelled GET behind.
        let mut buffer = buffer;
        buffer.as_mut_slice().fill(0);
        let mut remote = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        let chunk = view.chunk_size();
        let serve_cache = !self.force_remote || self.cache_remote;
        for block in 0..bytes / BLOCK_SIZE {
            let absolute = offset / BLOCK_SIZE as u64 + block as u64;
            let mut lookup = match view.lookup(absolute) {
                Ok(lookup) => lookup,
                Err(_) => return self.decline(|c| &mut c.declined_io, offset, buffer, permit),
            };
            let hash = match lookup.cached(&self.pages) {
                Ok(LookupState::Complete(None)) => continue,
                Ok(LookupState::Complete(Some(hash))) => hash,
                _ => return self.decline(|c| &mut c.declined_io, offset, buffer, permit),
            };
            let destination =
                &mut buffer.as_mut_slice()[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE];
            if serve_cache && let Some(bytes) = self.cache.get(&hash) {
                let at = chunk.offset(absolute);
                destination.copy_from_slice(&bytes.as_slice()[at..at + BLOCK_SIZE]);
                continue;
            }
            let local = if self.force_remote {
                false
            } else {
                match self.reader.plan(hash) {
                    Ok(plan) => plan.is_some(),
                    Err(_) => return self.decline(|c| &mut c.declined_io, offset, buffer, permit),
                }
            };
            if local || self.peer.is_none() || chunk.bytes() != BLOCK_SIZE {
                return self.decline(|c| &mut c.declined_io, offset, buffer, permit);
            }
            if remote.try_reserve(1).is_err() {
                return self.decline(|c| &mut c.declined_capacity, offset, buffer, permit);
            }
            remote.push((block, hash));
        }
        if remote.is_empty() {
            self.counters.inline_reads += 1;
            if let Some(trace) = &mut permit.trace {
                trace.responded_ns = trace.at();
            }
            output.push_back(Completed {
                id,
                data: CompletionData::Read(buffer),
                result: Ok(()),
                permit: Some(permit),
            });
            return Ok(Outcome::Taken);
        }
        let peer = self
            .peer
            .clone()
            .expect("remote chunk without a peer router");
        let mut tickets = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        if tickets.try_reserve_exact(remote.len()).is_err() {
            return self.decline(|c| &mut c.declined_capacity, offset, buffer, permit);
        }
        for &(block, hash) in &remote {
            let Ok(notify) = self.notify.try_clone() else {
                return self.decline(|c| &mut c.declined_capacity, offset, buffer, permit);
            };
            // An exhausted GET pool declines the whole read; tickets taken so
            // far are dropped and their slots retained until transport ends.
            let Ok(ticket) = peer.fetch_with_notify(hash, notify) else {
                drop(tickets);
                return self.decline(|c| &mut c.declined_capacity, offset, buffer, permit);
            };
            if !ticket.ready() {
                let local = if self.force_remote && peer.wants_local_plan() {
                    self.reader.plan(hash).ok().flatten()
                } else {
                    None
                };
                peer.race(&ticket, hash, local);
            }
            tickets.push(Remote {
                block,
                hash,
                ticket,
                done: false,
            });
        }
        if let Some(metrics) = &self.peer_metrics {
            permit.remote = Some(crate::peer::metrics::Trace::new(
                metrics.clone(),
                id,
                permit.received.unwrap_or_else(Instant::now),
            ));
        }
        self.pending.push(Pending {
            id,
            buffer,
            permit,
            remote: tickets,
        });
        // A reply that arrived inline completes without any wake.
        self.poll(shared, output)?;
        Ok(Outcome::Taken)
    }

    /// Complete every pending read whose sources have all finished.
    pub fn poll(&mut self, shared: &Shared, output: &mut BudgetQueue<Completed>) -> io::Result<()> {
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &mut self.pending[index];
            let mut failure = None;
            for remote in pending.remote.iter_mut().filter(|r| !r.done) {
                let destination = &mut pending.buffer.as_mut_slice()
                    [remote.block * BLOCK_SIZE..(remote.block + 1) * BLOCK_SIZE];
                let (phases, result) = remote.ticket.finish(destination);
                match result {
                    Ok(false) => continue,
                    Ok(true) => {
                        remote.done = true;
                        if let Some(trace) = &mut pending.permit.remote {
                            trace.fetched(phases);
                        }
                        if self.cache_remote {
                            match self.cache.fill(remote.hash, destination) {
                                Ok(_) => (),
                                Err(e) if e.kind() == io::ErrorKind::OutOfMemory => (),
                                Err(e) => {
                                    failure = Some(e);
                                    break;
                                }
                            }
                        }
                    }
                    Err(error) => {
                        if let Some(trace) = &mut pending.permit.remote {
                            trace.fetched(phases);
                        }
                        failure = Some(error);
                        break;
                    }
                }
            }
            if failure.is_none() && pending.remote.iter().any(|r| !r.done) {
                index += 1;
                continue;
            }
            let Pending {
                id,
                mut buffer,
                mut permit,
                remote,
            } = self.pending.swap_remove(index);
            // Losers and unfinished sources keep their slots until their
            // transport finishes; none of them can touch this buffer again.
            drop(remote);
            let result = match failure {
                None => {
                    self.counters.remote_reads += 1;
                    Ok(())
                }
                Some(error) => {
                    self.counters.failed_reads += 1;
                    buffer.as_mut_slice().fill(0);
                    let message = error.to_string();
                    shared
                        .health
                        .lock()
                        .map_err(|_| io::Error::other("completion gate poisoned"))?
                        .fail(message.clone());
                    self.gate.fail(message);
                    Err(error)
                }
            };
            if let Some(trace) = &mut permit.trace {
                trace.responded_ns = trace.at();
            }
            output.push_back(Completed {
                id,
                data: CompletionData::Read(buffer),
                result,
                permit: Some(permit),
            });
        }
        Ok(())
    }

    /// Wait for a reply or a reactor completion while fast reads are pending.
    pub fn wait(&self, timeout_ms: i32) -> io::Result<()> {
        let mut descriptor = libc::pollfd {
            fd: std::os::fd::AsRawFd::as_raw_fd(&self.notify),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the descriptor is live and the array covers one entry.
        let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        match self.notify.read() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error),
        }
    }
}
