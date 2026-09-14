//! Descriptor ownership precedes storage admission. Only reads may pass a queue head.
use super::*;
use allocator_api2::vec::Vec as BudgetVec;
use cas_core::budget::{Amount, BudgetAllocator, Lease};

const SNAPSHOT_BYTES: usize =
    (CONCURRENT_QUEUES * CONCURRENT_QUEUE_SIZE + 1) * CONCURRENT_QUEUE_SIZE * size_of::<Segment>();

pub(super) struct Discovered {
    request: Request,
    available: u16,
    order: u64,
}

pub(super) struct Frontier {
    queues: [BudgetVec<Discovered, BudgetAllocator>; CONCURRENT_QUEUES],
    pub(super) spans: Arc<Budget>,
    // Reserve the worst-case descriptor snapshots before accepting a frontend.
    // Actual snapshots use the inner budget and transfer to Pending on admission.
    _spans: Lease,
    pub(super) next_order: u64,
    pub(super) reads: admission::QueueAdmission,
    pub(super) bypassed: u64,
}

impl Frontier {
    pub(super) fn new(metadata: &Arc<Budget>) -> io::Result<Self> {
        let bytes = SNAPSHOT_BYTES;
        let credit = metadata
            .reserve(Amount { bytes, requests: 0 })
            .ok_or(io::ErrorKind::OutOfMemory)?;
        let queues = [
            local::reserved_vec(CONCURRENT_QUEUE_SIZE, metadata)?,
            local::reserved_vec(CONCURRENT_QUEUE_SIZE, metadata)?,
            local::reserved_vec(CONCURRENT_QUEUE_SIZE, metadata)?,
            local::reserved_vec(CONCURRENT_QUEUE_SIZE, metadata)?,
        ];
        Ok(Self {
            queues,
            spans: Budget::new(Amount { bytes, requests: 0 }),
            _spans: credit,
            next_order: 0,
            reads: admission::QueueAdmission::default(),
            bypassed: 0,
        })
    }

    pub(super) fn report(&self) -> serde_json::Value {
        serde_json::json!({
            "discovered": self.next_order,
            "waiting_per_queue": self.queues.each_ref().map(|queue| queue.len()),
            "bypassed_reads": self.bypassed,
            "descriptor_reserve_bytes": SNAPSHOT_BYTES,
            "descriptor_allocations": self.spans.usage(),
            "read_admission": self.reads.snapshot(),
        })
    }

    pub(super) fn retry_at(&self) -> Option<Instant> {
        self.reads.retry_at()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queues.iter().all(|queue| queue.is_empty())
    }

    pub(super) fn restore(
        &mut self,
        order: u64,
        identity: crate::inflight::Request,
        request: Request,
    ) -> io::Result<()> {
        let queue = &mut self.queues[usize::from(identity.queue)];
        if queue.len() == CONCURRENT_QUEUE_SIZE {
            return Err(io::Error::other("discovered queue exceeds capacity"));
        }
        queue.push(Discovered {
            request,
            available: identity.available,
            order,
        });
        self.next_order = self.next_order.max(order);
        Ok(())
    }

    /// All comparisons use the order in which this frontend observed headers.
    /// Admitted mutations are separately covered by the reactor publication gate.
    fn eligible(&self, queue: usize, index: usize) -> bool {
        let candidate = &self.queues[queue][index];
        if index != 0 && !matches!(candidate.request, Request::Read(_)) {
            return false;
        }
        let identity = candidate
            .request
            .inflight(queue as u16, candidate.available);
        self.queues
            .iter()
            .flatten()
            .filter(|older| older.order < candidate.order)
            .all(|older| !conflicts(older.request.inflight(0, 0), identity))
    }
}

fn conflicts(older: crate::inflight::Request, later: crate::inflight::Request) -> bool {
    use crate::inflight::Kind;
    if matches!(older.kind, Kind::Flush | Kind::Protocol)
        || matches!(later.kind, Kind::Flush | Kind::Protocol)
    {
        return true;
    }
    if older.length == 0
        || later.length == 0
        || (older.kind == Kind::Read && later.kind == Kind::Read)
    {
        return false;
    }
    older.offset < later.offset + later.length && later.offset < older.offset + older.length
}

impl Backend {
    pub(super) fn progress_queue(
        &mut self,
        mem: &GuestMemoryLoadGuard<GuestMemoryMmap>,
        queue: u16,
        vring: &VringMutex,
        discover: bool,
    ) -> io::Result<()> {
        let index = usize::from(queue);
        if self.blocked_queues[index] {
            return Ok(());
        }
        let locking = self.read_trace.as_ref().map(|_| Instant::now());
        let mut state = vring.get_mut();
        if let (Some(observer), Some(locking)) = (&mut self.read_trace, locking) {
            let waited = read_trace::ns(locking.elapsed());
            observer.queue_lock_wait_ns += waited;
            observer.max_queue_lock_wait_ns = observer.max_queue_lock_wait_ns.max(waited);
        }
        if !state.is_enabled() || !state.get_queue().ready() {
            return Ok(());
        }
        let queue_size = usize::from(state.get_queue().size());
        let features = self.negotiated_features & self.features();
        if discover {
            // One ring's worth per visit, including malformed/immediate requests.
            for _ in 0..queue_size {
                let frontier = self.frontier.as_mut().expect("concurrent frontier");
                if frontier.queues[index].len() == queue_size {
                    break;
                }
                let Some(NextChain { chain, next_avail }) = peek(mem.clone(), &mut state)? else {
                    break;
                };
                let observed = self.read_trace.as_ref().map(|_| Instant::now());
                let request = decode_chain(mem, chain, self.capacity_bytes, &frontier.spans)?
                    .negotiated(features);
                let identity = request.inflight(queue, next_avail.wrapping_sub(1));
                let order = frontier
                    .next_order
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("discovery IDs exhausted"))?;
                let gate = self.storage.completion_gate();
                let mut guard = gate.as_ref().map(|gate| gate.lock_checked()).transpose()?;
                if let Some(carrier) = guard
                    .as_deref_mut()
                    .and_then(|state| state.carrier.as_mut())
                    && carrier.discover(identity)? != order
                {
                    return Err(io::Error::other("discovery order differs from carrier"));
                }
                if let Some(observer) = &mut self.read_trace {
                    observer.discover(
                        queue,
                        identity.head,
                        identity.available,
                        order,
                        frontier.queues[index].len() as u16,
                        observed.expect("tracing clock"),
                    );
                }
                frontier.restore(order, identity, request)?;
                state.get_queue_mut().set_next_avail(next_avail);
            }
        }
        let mut admitted = 0;
        for _ in 0..queue_size {
            let frontier = self.frontier.as_mut().expect("concurrent frontier");
            if frontier.queues[index].is_empty() {
                break;
            }
            let gate = self.storage.completion_gate();
            let mut guard = gate.as_ref().map(|gate| gate.lock_checked()).transpose()?;
            let mut selected = None;
            for candidate in 0..frontier.queues[index].len() {
                if !frontier.eligible(index, candidate) {
                    continue;
                }
                let discovered = &frontier.queues[index][candidate];
                if candidate == 0 {
                    frontier.reads.cancel_matching(
                        queue,
                        discovered.available,
                        discovered.request.completion().head,
                    );
                }
                let admission = if candidate == 0 {
                    &mut self.admission
                } else {
                    &mut frontier.reads
                };
                if let Some(observer) = &mut self.read_trace {
                    observer.discovered_head(
                        queue,
                        discovered.request.completion().head,
                        Instant::now(),
                    );
                }
                match admission.prepare_admission(
                    queue,
                    discovered.available,
                    &discovered.request,
                    &mut self.storage,
                    true,
                    self.pending.len() >= local::IMAGE_REQUEST_LIMIT,
                )? {
                    Admission::Accepted(permit) => {
                        admission.finish_wait(queue, true);
                        selected = Some((candidate, permit));
                        break;
                    }
                    Admission::Waiting => {
                        if let Some(observer) = &mut self.read_trace {
                            observer.waiting(
                                queue,
                                discovered.available,
                                match &discovered.request {
                                    Request::Write(data) => {
                                        Some((data.offset, data.offset + data.len as u64))
                                    }
                                    _ => None,
                                },
                                admission.reason_index(queue),
                                Instant::now(),
                            );
                            observer.discovered_wait(queue, discovered.order);
                        }
                        // One read candidate per queue keeps fairness tickets bounded.
                        if matches!(discovered.request, Request::Read(_)) {
                            break;
                        }
                    }
                }
            }
            let Some((candidate, permit)) = selected else {
                break;
            };
            let discovered = frontier.queues[index].remove(candidate);
            if candidate != 0 {
                frontier.bypassed += 1;
            }
            self.accept(
                mem,
                &mut state,
                Prepared {
                    queue,
                    available: discovered.available,
                    request: discovered.request,
                    permit,
                },
                guard.as_deref_mut(),
            )?;
            admitted += 1;
        }
        if admitted != 0 {
            self.completion_event.write(1)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
