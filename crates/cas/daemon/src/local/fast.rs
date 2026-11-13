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
//! reactor's. Remote chunks join the shared fetch registry through the state
//! machine in `chunk.rs`: a leader fetches and publishes for every waiter, a
//! waiter is woken through the frontend's own eventfd. Once a registry entry
//! is held the read is never declined (a dropped leader would fail its
//! waiters); it completes or fails exactly as a reactor read does. A failure
//! of shared IO (a GET reply, a leader's publication, the cache fill) fails
//! the read, the image gate and the shared host gate like the frozen path;
//! local resource exhaustion while joining fails only the read and its image
//! (see `Context::fail`). Reads that cannot establish these facts are sent to
//! the reactor unchanged.
use super::*;
use crate::local::chunk::{Block, Fetch, Joined, Sources, copy_chunk};
use cas_core::{
    aligned::AlignedBuffer, cache::fills::Waiter, chunk_index::Hash, manifest::file::View,
};
use std::os::fd::{AsRawFd, OwnedFd};

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
    /// The FLUSH table overflowed: every later read is declined (`declined_flush`)
    /// until the image restarts. Cannot happen within the admission limit.
    pub flush_overflow: u64,
}

type Count = fn(&mut Counters) -> &mut u64;

enum Source {
    /// This read leads the fetch: its GET fills the cell for every waiter.
    Fetch {
        ticket: crate::peer::Ticket,
        fetch: Option<Fetch>,
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
    peer_metrics: Option<crate::peer::metrics::ImageMetrics>,
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
    /// Hand the request back to the reactor unchanged.
    fn decline(&mut self, count: Count, io: Io) -> Outcome {
        *count(&mut self.counters) += 1;
        Outcome::Declined(io)
    }
    pub fn flush_sent(&mut self, id: u64) {
        if self.flushes.len() < self.flushes.capacity() {
            self.flushes.push(id);
        } else {
            // Sized to the admission limit, so this needs a FLUSH completion
            // the frontend never observed. Stay conservative, and say so.
            self.counters.flush_overflow += 1;
            if self.counters.flush_overflow == 1 {
                eprintln!(
                    "cas-daemon: fast-settled-reads FLUSH table overflowed; \
                     inline reads are declined until the image restarts"
                );
            }
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
        cas_core::eventfd::duplicate(self.notify.as_raw_fd())
    }

    /// Serve the read inline, start its remote fetches, or decline it. The
    /// request's boundary is the frontend's `admitted` at enqueue.
    pub fn enqueue(
        &mut self,
        shared: &Shared,
        mut io: Io,
        output: &mut BudgetQueue<Completed>,
    ) -> io::Result<Outcome> {
        let admitted = io.boundary;
        let Operation::Read { offset, buffer } = &io.operation else {
            return Ok(Outcome::Declined(io));
        };
        let (offset, bytes) = (*offset, buffer.as_slice().len());
        if !self.flushes.is_empty() {
            return Ok(self.decline(|c| &mut c.declined_flush, io));
        }
        if shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .failure
            .is_some()
        {
            return Ok(self.decline(|c| &mut c.declined_failed, io));
        }
        let status = *shared.final_status.lock().expect("status poisoned");
        if status.published != admitted || status.compacted != status.published {
            return Ok(self.decline(|c| &mut c.declined_unsettled, io));
        }
        let view = match &*shared.manifest.lock().expect("manifest poisoned") {
            Some((compacted, view)) if *compacted == status.compacted => view.clone(),
            _ => return Ok(self.decline(|c| &mut c.declined_unsettled, io)),
        };
        if view.commit().durable != status.compacted {
            return Ok(self.decline(|c| &mut c.declined_unsettled, io));
        }
        let end = offset.checked_add(bytes as u64);
        if bytes == 0
            || !bytes.is_multiple_of(BLOCK_SIZE)
            || !offset.is_multiple_of(BLOCK_SIZE as u64)
            || end.is_none_or(|end| end > status.image_bytes)
            || bytes > MAX_REQUEST_BYTES
        {
            // The reactor returns the actual range error.
            return Ok(self.decline(|c| &mut c.declined_io, io));
        }
        if self.pending.len() == self.pending.capacity() {
            return Ok(self.decline(|c| &mut c.declined_capacity, io));
        }
        // Pass 1: resolve every block through the shared resolution before
        // touching the registry or the transport, so a decline holds nothing.
        // Holes are zeroed and cache hits copied by the resolution itself.
        let mut remote = BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&self.metadata)));
        let declined = {
            let Operation::Read { buffer, .. } = &mut io.operation else {
                unreachable!()
            };
            let mut declined: Option<Count> = None;
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
                            declined = Some(|c| &mut c.declined_capacity);
                            break;
                        }
                        remote.push((block, hash));
                    }
                    Ok(Block::Remote(_) | Block::Page { .. } | Block::Store(..)) | Err(_) => {
                        declined = Some(|c| &mut c.declined_io);
                        break;
                    }
                }
            }
            declined
        };
        if let Some(count) = declined {
            return Ok(self.decline(count, io));
        }
        if remote.is_empty() {
            let Io {
                id,
                operation,
                mut permit,
                ..
            } = io;
            let Operation::Read { buffer, .. } = operation else {
                unreachable!()
            };
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
            return Ok(self.decline(|c| &mut c.declined_capacity, io));
        }
        // Pass 2: join the shared fetch registry. Nothing is held until the
        // first entry; after that a problem fails the read like a reactor
        // read, never declines it (declining would drop a leader and fail
        // every waiter that joined it).
        let peer = self
            .peer
            .clone()
            .expect("remote chunk without a peer router");
        let credits = io
            .permit
            ._read
            .as_ref()
            .expect("read byte reservation")
            .clone();
        let mut failure: Option<io::Error> = None;
        for &(block, hash) in &remote {
            let absolute = offset / BLOCK_SIZE as u64 + block as u64;
            let Operation::Read { buffer, .. } = &mut io.operation else {
                unreachable!()
            };
            let destination =
                &mut buffer.as_mut_slice()[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE];
            let joined = self.wake().and_then(|wake| {
                self.sources.join(
                    &self.fetches,
                    hash,
                    BLOCK_SIZE,
                    &credits,
                    &self.metadata,
                    &view,
                    absolute,
                    destination,
                    Some(wake),
                )
            });
            let source = match joined {
                Ok(Joined::Wait(waiter)) => {
                    self.counters.shared_waits += 1;
                    Source::Wait(waiter)
                }
                Ok(Joined::Served) => continue,
                Ok(Joined::Lead(fetch)) => {
                    let ticket = self
                        .wake()
                        .and_then(|wake| peer.fetch_with_notify(hash, wake));
                    let ticket = match ticket {
                        Ok(ticket) => ticket,
                        Err(error) => {
                            drop(fetch);
                            failure = Some(error);
                            break;
                        }
                    };
                    // A reply that already failed is raced too (the
                    // challenger starts at once); only a verified one is not.
                    if !ticket.verified() {
                        let local = if peer.wants_local_plan() {
                            self.sources.race_plan(hash)
                        } else {
                            None
                        };
                        peer.race(&ticket, hash, local);
                    }
                    Source::Fetch {
                        ticket,
                        fetch: Some(fetch),
                    }
                }
                Ok(Joined::Full) if sources.is_empty() => {
                    return Ok(self.decline(|c| &mut c.declined_capacity, io));
                }
                Ok(Joined::Full) => {
                    failure = Some(io::Error::other(
                        "admitted read exceeded host fetch capacity",
                    ));
                    break;
                }
                Err(_) if sources.is_empty() => {
                    return Ok(self.decline(|c| &mut c.declined_capacity, io));
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
        let Io {
            id,
            operation,
            mut permit,
            ..
        } = io;
        let Operation::Read { buffer, .. } = operation else {
            unreachable!()
        };
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
            // Local exhaustion while joining: no shared IO has failed.
            self.fail(shared, pending, error, false, output)?;
            return Ok(Outcome::Taken);
        }
        self.pending.push(pending);
        // A reply that arrived inline completes without any wake.
        self.poll(shared, output)?;
        Ok(Outcome::Taken)
    }

    /// Fail closed like a reactor read: IOERR to the guest with a zeroed
    /// buffer, the image gate failed, unfinished leaders fail their waiters.
    /// The shared host gate fails only when `shared_io` is set: the reactor
    /// fails the host for a read whose shared IO (store or GET payload, a
    /// publication it waited for, the cache fill) failed, and only the image
    /// for errors before that stage. Here that means failures observed in
    /// `poll`; exhaustion while joining (registry capacity after a waiter
    /// joined, metadata budget, descriptor duplication, GET slots) is local.
    fn fail(
        &mut self,
        shared: &Shared,
        pending: Pending,
        error: io::Error,
        shared_io: bool,
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
        if shared_io {
            self.gate.fail(message);
        }
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
                let absolute = *offset / BLOCK_SIZE as u64 + remote.block as u64;
                let destination = &mut buffer.as_mut_slice()
                    [remote.block * BLOCK_SIZE..(remote.block + 1) * BLOCK_SIZE];
                match &mut remote.source {
                    Source::Fetch { ticket, fetch } => {
                        let (phases, result) =
                            ticket.finish(fetch.as_mut().expect("unpublished fetch").payload_mut());
                        match result {
                            Ok(false) => continue,
                            Ok(true) => {
                                if let Some(trace) = &mut permit.remote {
                                    trace.fetched(phases);
                                }
                                let fetch = fetch.take().expect("unpublished fetch");
                                if let Err(error) = fetch.publish(
                                    &self.sources,
                                    remote.hash,
                                    Some(phases),
                                    view,
                                    absolute,
                                    destination,
                                ) {
                                    failure = Some(error);
                                    break;
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
                // Shared IO failed: a reply, a publication or the fill.
                self.fail(shared, pending, error, true, output)?;
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

    /// Wait for a reply, a publication or a reactor completion while fast
    /// reads are pending; every one of them signals the frontend wake.
    pub fn wait(&self, timeout_ms: i32) -> io::Result<()> {
        let mut descriptor = libc::pollfd {
            fd: self.notify.as_raw_fd(),
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
