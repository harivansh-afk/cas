//! Frontend-inline settled reads (`--fast-settled-reads`), off by default.
//!
//! A read bypasses the reactor only when **all** of these hold at enqueue:
//!
//! 1. The image is host attached, concurrent, not paused, and its completion
//!    gate has no failure (a failed image rejects through the reactor).
//! 2. The reactor's latest status has `published == admitted` (every mutation
//!    this frontend ever admitted, including an unsealed batch, is published:
//!    no in-flight write exists) and `compacted == published` (D == P: the
//!    staging index retains no mapping newer than D, so no staged write can
//!    take precedence over the manifest for any block).
//! 3. The manifest view published by the reactor is exactly the one at that D.
//! 4. No FLUSH this frontend sent to the reactor is outstanding (tracked by
//!    request id, so a FLUSH rejected before it was sent is not a barrier and
//!    cannot cancel one that is).
//! 5. Every block resolves without local IO through the resolution shared
//!    with the reactor (`chunk.rs`): a hole, a clean-cache hit, or a chunk the
//!    reactor would also fetch from a peer (force-remote, or no local store
//!    copy). A manifest page load or a local store read declines.
//! 6. Bounded state: one pending-table slot, one fetch-registry entry and one
//!    GET slot per remote chunk.
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
//! reactor's. Remote chunks join the shared fetch registry: a leader fetches
//! and publishes for every waiter, a waiter is woken through the frontend's
//! own eventfd. Once a registry entry is held the read is never declined (a
//! dropped leader would fail its waiters); it completes or fails exactly as a
//! reactor read does. A remote failure fails the read, the image gate and the
//! shared host gate like the frozen path. Reads that cannot establish these
//! facts are sent to the reactor unchanged.
use super::*;
use crate::local::chunk::{Block, Sources, copy_chunk};
use cas_core::{
    aligned::AlignedBuffer,
    cache::fills::{Leader, Lookup, Registry, Waiter},
    chunk_index::Hash,
    manifest::file::View,
};
use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Counters {
    pub inline_reads: u64,
    pub remote_reads: u64,
    /// Remote chunks served by another reader's fetch through the registry.
    pub shared_waits: u64,
    pub declined_unsettled: u64,
    pub declined_flush: u64,
    pub declined_io: u64,
    pub declined_capacity: u64,
    pub declined_failed: u64,
    pub failed_reads: u64,
}

enum Source {
    /// This read leads the fetch: its GET fills the cell for every waiter.
    Fetch {
        ticket: crate::peer::Ticket,
        leader: Option<Leader<Fetched>>,
        fetched: Option<BudgetArc<Fetched>>,
    },
    /// Another reader leads; its publication wakes the frontend.
    Wait(Waiter<Fetched>),
}

struct Remote {
    block: usize,
    hash: Hash,
    source: Source,
    done: bool,
}

struct Pending {
    id: u64,
    offset: u64,
    buffer: AlignedBuffer,
    permit: Permit,
    view: View,
    remote: BudgetVec<Remote, BudgetAllocator>,
}

pub(super) struct Context {
    notify: EventFd,
    peer: Option<BudgetArc<crate::peer::Router>>,
    peer_metrics: Option<BudgetArc<crate::peer::metrics::Metrics>>,
    fetches: Fetches,
    sources: Sources,
    gate: BudgetArc<state::HostGate>,
    metadata: Arc<Budget>,
    pending: BudgetVec<Pending, BudgetAllocator>,
    /// Ids of FLUSHes sent to the reactor and not yet completed.
    flushes: BudgetVec<u64, BudgetAllocator>,
    pub(super) counters: Counters,
}

#[expect(
    clippy::large_enum_variant,
    reason = "A declined read hands its request straight back to the reactor path"
)]
pub(super) enum Outcome {
    Taken,
    Declined(Io),
}

impl Context {
    pub fn new(port: &host::Port, event: &EventFd) -> io::Result<Self> {
        let metadata = port.metadata();
        Ok(Self {
            notify: event.try_clone()?,
            peer: port.peer(),
            peer_metrics: port.peer_metrics(),
            fetches: port.fetches(),
            sources: Sources::new(port),
            gate: port.host_gate(),
            pending: reserved_vec(IMAGE_REQUEST_LIMIT, &metadata)?,
            flushes: reserved_vec(IMAGE_REQUEST_LIMIT, &metadata)?,
            metadata,
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
        id: u64,
        boundary: u64,
    ) -> io::Result<Outcome> {
        *count(&mut self.counters) += 1;
        Ok(Outcome::Declined(Io {
            id,
            operation: Operation::Read { offset, buffer },
            permit,
            boundary,
        }))
    }
    pub fn flush_sent(&mut self, id: u64) {
        if self.flushes.len() < self.flushes.capacity() {
            self.flushes.push(id);
        } else {
            // Cannot happen within the admission limit; stay conservative.
            self.flushes.clear();
            self.flushes.push(u64::MAX);
        }
    }
    pub fn flush_completed(&mut self, id: u64) {
        if let Some(index) = self.flushes.iter().position(|&flush| flush == id) {
            self.flushes.swap_remove(index);
        }
    }
    /// A duplicate of the frontend wake for a registry waiter or a GET.
    fn wake(&self) -> io::Result<OwnedFd> {
        let raw = self.notify.try_clone()?.into_raw_fd();
        // SAFETY: the freshly cloned eventfd transfers its unique FD owner here.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    /// Serve the read inline, start its remote fetches, or decline it. The
    /// request's boundary is the frontend's `admitted` at enqueue.
    pub fn enqueue(
        &mut self,
        shared: &Shared,
        io: Io,
        output: &mut BudgetQueue<Completed>,
    ) -> io::Result<Outcome> {
        let Io {
            id,
            operation,
            mut permit,
            boundary,
        } = io;
        let admitted = boundary;
        let Operation::Read { offset, buffer } = operation else {
            return Ok(Outcome::Declined(Io {
                id,
                operation,
                permit,
                boundary,
            }));
        };
        if !self.flushes.is_empty() {
            return self.decline(
                |c| &mut c.declined_flush,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
        }
        if shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .failure
            .is_some()
        {
            return self.decline(
                |c| &mut c.declined_failed,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
        }
        let status = *shared.final_status.lock().expect("status poisoned");
        if status.published != admitted || status.compacted != status.published {
            return self.decline(
                |c| &mut c.declined_unsettled,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
        }
        let view = match &*shared.manifest.lock().expect("manifest poisoned") {
            Some((compacted, view)) if *compacted == status.compacted => view.clone(),
            _ => {
                return self.decline(
                    |c| &mut c.declined_unsettled,
                    offset,
                    buffer,
                    permit,
                    id,
                    boundary,
                );
            }
        };
        if view.commit().durable != status.compacted {
            return self.decline(
                |c| &mut c.declined_unsettled,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
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
            return self.decline(|c| &mut c.declined_io, offset, buffer, permit, id, boundary);
        }
        if self.pending.len() == self.pending.capacity() {
            return self.decline(
                |c| &mut c.declined_capacity,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
        }
        // Pass 1: resolve every block through the shared resolution before
        // touching the registry or the transport, so a decline holds nothing.
        // Holes are zeroed and cache hits copied by the resolution itself.
        let mut buffer = buffer;
        let mut remote = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        for block in 0..bytes / BLOCK_SIZE {
            let absolute = offset / BLOCK_SIZE as u64 + block as u64;
            let destination =
                &mut buffer.as_mut_slice()[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE];
            match self.sources.resolve(&view, absolute, destination) {
                Ok(Block::Hole | Block::Cached) => continue,
                Ok(Block::Remote(hash))
                    if self.peer.is_some() && view.chunk_size().bytes() == BLOCK_SIZE =>
                {
                    if remote.try_reserve(1).is_err() {
                        return self.decline(
                            |c| &mut c.declined_capacity,
                            offset,
                            buffer,
                            permit,
                            id,
                            boundary,
                        );
                    }
                    remote.push((block, hash));
                }
                Ok(Block::Remote(_) | Block::Page { .. } | Block::Store(..)) | Err(_) => {
                    return self.decline(
                        |c| &mut c.declined_io,
                        offset,
                        buffer,
                        permit,
                        id,
                        boundary,
                    );
                }
            }
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
        let mut sources = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        if sources.try_reserve_exact(remote.len()).is_err() {
            return self.decline(
                |c| &mut c.declined_capacity,
                offset,
                buffer,
                permit,
                id,
                boundary,
            );
        }
        // Pass 2: join the shared fetch registry. Nothing is held until the
        // first entry; after that a problem fails the read like a reactor
        // read, never declines it (declining would drop a leader and fail
        // every waiter that joined it).
        let peer = self
            .peer
            .clone()
            .expect("remote chunk without a peer router");
        let credits = permit
            ._read
            .as_ref()
            .expect("read byte reservation")
            .clone();
        let mut failure: Option<io::Error> = None;
        for &(block, hash) in &remote {
            let destination =
                &mut buffer.as_mut_slice()[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE];
            let lookup = match self.wake() {
                Ok(wake) => Registry::lookup_with(&self.fetches, hash, Some(wake)),
                Err(error) => Err(error),
            };
            let source = match lookup {
                Ok(Some(Lookup::Waiter(waiter))) => {
                    self.counters.shared_waits += 1;
                    Source::Wait(waiter)
                }
                Ok(Some(Lookup::Leader(leader))) => {
                    let fetched =
                        AlignedBuffer::try_new_in(BLOCK_SIZE, allocator_api2::alloc::Global)
                            .and_then(|bytes| {
                                BudgetArc::try_new(
                                    Fetched {
                                        remote: None,
                                        bytes,
                                        _credits: credits.clone(),
                                    },
                                    &self.metadata,
                                )
                            });
                    let Ok(mut fetched) = fetched else {
                        failure = Some(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            "fetch payload metadata exhausted",
                        ));
                        break;
                    };
                    // A previous leader may have filled the cache between the
                    // miss and this lookup; publish it through the new cell.
                    if let Some(bytes) = self.sources.peek(&hash) {
                        destination.copy_from_slice(bytes.as_slice());
                        fetched
                            .get_mut()
                            .expect("unpublished fetch is unique")
                            .bytes
                            .as_mut_slice()
                            .copy_from_slice(bytes.as_slice());
                        if let Err(error) = leader.complete(fetched) {
                            failure = Some(error);
                            break;
                        }
                        continue;
                    }
                    let ticket = match self.wake() {
                        Ok(wake) => {
                            // SAFETY: the owned eventfd transfers its unique FD owner here.
                            let notify = unsafe { EventFd::from_raw_fd(wake.into_raw_fd()) };
                            peer.fetch_with_notify(hash, notify)
                        }
                        Err(error) => Err(error),
                    };
                    let ticket = match ticket {
                        Ok(ticket) => ticket,
                        Err(error) => {
                            drop(leader);
                            failure = Some(error);
                            break;
                        }
                    };
                    if !ticket.ready() {
                        let local = if peer.wants_local_plan() {
                            self.sources.race_plan(hash)
                        } else {
                            None
                        };
                        peer.race(&ticket, hash, local);
                    }
                    Source::Fetch {
                        ticket,
                        leader: Some(leader),
                        fetched: Some(fetched),
                    }
                }
                Ok(None) if sources.is_empty() => {
                    return self.decline(
                        |c| &mut c.declined_capacity,
                        offset,
                        buffer,
                        permit,
                        id,
                        boundary,
                    );
                }
                Ok(None) => {
                    failure = Some(io::Error::other(
                        "admitted read exceeded host fetch capacity",
                    ));
                    break;
                }
                Err(_) if sources.is_empty() => {
                    return self.decline(
                        |c| &mut c.declined_capacity,
                        offset,
                        buffer,
                        permit,
                        id,
                        boundary,
                    );
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            };
            sources.push(Remote {
                block,
                hash,
                source,
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
        let pending = Pending {
            id,
            offset,
            buffer,
            permit,
            view,
            remote: sources,
        };
        if let Some(error) = failure {
            self.fail(shared, pending, error, output)?;
            return Ok(Outcome::Taken);
        }
        self.pending.push(pending);
        // A reply that arrived inline completes without any wake.
        self.poll(shared, output)?;
        Ok(Outcome::Taken)
    }

    /// Fail closed like a reactor read: IOERR to the guest, the image gate and
    /// the shared host gate failed, unfinished leaders fail their waiters.
    fn fail(
        &mut self,
        shared: &Shared,
        pending: Pending,
        error: io::Error,
        output: &mut BudgetQueue<Completed>,
    ) -> io::Result<()> {
        let Pending {
            id,
            mut buffer,
            mut permit,
            remote,
            ..
        } = pending;
        drop(remote);
        self.counters.failed_reads += 1;
        buffer.as_mut_slice().fill(0);
        let message = error.to_string();
        shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .fail(message.clone());
        self.gate.fail(message);
        if let Some(trace) = &mut permit.trace {
            trace.responded_ns = trace.at();
        }
        output.push_back(Completed {
            id,
            data: CompletionData::Read(buffer),
            result: Err(error),
            permit: Some(permit),
        });
        Ok(())
    }

    /// Complete every pending read whose sources have all finished.
    pub fn poll(&mut self, shared: &Shared, output: &mut BudgetQueue<Completed>) -> io::Result<()> {
        let mut index = 0;
        while index < self.pending.len() {
            let Pending {
                offset,
                buffer,
                permit,
                view,
                remote: remotes,
                ..
            } = &mut self.pending[index];
            let mut failure = None;
            for remote in remotes.iter_mut().filter(|r| !r.done) {
                let destination = &mut buffer.as_mut_slice()
                    [remote.block * BLOCK_SIZE..(remote.block + 1) * BLOCK_SIZE];
                match &mut remote.source {
                    Source::Fetch {
                        ticket,
                        leader,
                        fetched,
                    } => {
                        let (phases, result) = ticket.finish(destination);
                        match result {
                            Ok(false) => continue,
                            Ok(true) => {
                                if let Err(error) =
                                    self.sources.fill(remote.hash, destination, true)
                                {
                                    failure = Some(error);
                                    break;
                                }
                                let mut cell = fetched.take().expect("unpublished fetch payload");
                                let unique = cell.get_mut().expect("unpublished fetch is unique");
                                unique.bytes.as_mut_slice().copy_from_slice(destination);
                                unique.remote = Some(phases);
                                if let Err(error) =
                                    leader.take().expect("fetch leader").complete(cell)
                                {
                                    failure = Some(error);
                                    break;
                                }
                                if let Some(trace) = &mut permit.remote {
                                    trace.fetched(phases);
                                }
                                remote.done = true;
                            }
                            Err(error) => {
                                if let Some(trace) = &mut permit.remote {
                                    trace.fetched(phases);
                                }
                                failure = Some(error);
                                break;
                            }
                        }
                    }
                    Source::Wait(waiter) => match waiter.poll() {
                        Ok(None) => continue,
                        Ok(Some(fetched)) => {
                            let absolute = *offset / BLOCK_SIZE as u64 + remote.block as u64;
                            if let Err(error) =
                                copy_chunk(view, absolute, fetched.bytes.as_slice(), destination)
                            {
                                failure = Some(error);
                                break;
                            }
                            if let (Some(phases), Some(trace)) =
                                (fetched.remote, &mut permit.remote)
                            {
                                trace.fetched(phases);
                            }
                            remote.done = true;
                        }
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    },
                }
            }
            if failure.is_none() && remotes.iter().any(|r| !r.done) {
                index += 1;
                continue;
            }
            let pending = self.pending.swap_remove(index);
            if let Some(error) = failure {
                self.fail(shared, pending, error, output)?;
                continue;
            }
            let Pending {
                id,
                buffer,
                mut permit,
                remote,
                ..
            } = pending;
            // Losers and finished sources keep their transport slots until
            // their transport ends; none can touch this buffer again.
            drop(remote);
            self.counters.remote_reads += 1;
            if let Some(trace) = &mut permit.trace {
                trace.responded_ns = trace.at();
            }
            output.push_back(Completed {
                id,
                data: CompletionData::Read(buffer),
                result: Ok(()),
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
