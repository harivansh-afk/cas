//! Opt-in read attribution. Fixed retention; never consumes a guest descriptor.
//! Observation starts when the backend sees avail.idx, not at guest submission.
use allocator_api2::{boxed::Box as BudgetBox, vec::Vec as BudgetVec};
use cas_core::budget::{Budget, BudgetAllocator};
use std::{io, sync::Arc, time::Instant};

const QUEUES: usize = 4;
const SLOTS: usize = 256;
const RETAIN: usize = 32;
pub(crate) const REASONS: usize = 11;

/// Fixed log2 nanosecond buckets. Quantiles are intervals, not exact values.
#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct Histogram {
    pub buckets: [[u64; 32]; 2],
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
}

impl Histogram {
    const EMPTY: Self = Self {
        buckets: [[0; 32]; 2],
        count: 0,
        total_ns: 0,
        max_ns: 0,
    };
    fn add(&mut self, value: u64) {
        let bucket = (64 - value.leading_zeros() as usize).min(63);
        self.buckets[bucket / 32][bucket % 32] += 1;
        self.count += 1;
        self.total_ns = self.total_ns.saturating_add(value);
        self.max_ns = self.max_ns.max(value);
    }
}

pub(crate) type OwnedTrace = BudgetBox<ReadTrace, BudgetAllocator>;

pub(crate) fn ns(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[derive(Clone, Copy)]
struct Seen {
    available: u16,
    observed: Instant,
    head: Option<Instant>,
    ahead: u16,
    behind_write_ns: u64,
    head_stall_ns: u64,
    behind_reason_ns: [u64; REASONS],
    head_reason_ns: [u64; REASONS],
    blocker_range: Option<(u64, u64)>,
}

#[derive(Clone, Copy)]
struct Stall {
    available: u16,
    since: Instant,
    write: bool,
    reason: usize,
    range: Option<(u64, u64)>,
}

/// Milestones are nanoseconds since `observed`; nested durations are labelled.
/// Travels inside the existing read permit, including through error completion.
#[derive(Clone, Copy, serde::Serialize)]
pub(crate) struct ReadTrace {
    #[serde(skip)]
    observed: Instant,
    pub id: u64,
    pub queue: u16,
    pub available: u16,
    pub descriptor: u16,
    pub offset: u64,
    pub bytes: usize,
    pub observed_ns: u64,
    pub ahead: u16,
    pub head_ns: u64,
    pub admitted_ns: u64,
    pub enqueued_ns: u64,
    pub received_ns: u64,
    pub started_ns: u64,
    pub responded_ns: u64,
    pub frontend_received_ns: u64,
    pub finished_ns: u64,
    // Subsets of observed -> head and head -> admitted, not extra wall time.
    pub behind_write_ns: u64,
    pub head_stall_ns: u64,
    pub behind_reason_ns: [u64; REASONS],
    pub head_reason_ns: [u64; REASONS],
    // Envelope of admission-blocked writes encountered before this read.
    // Outside the envelope proves disjointness; inside does not prove overlap.
    pub blocker_range: Option<(u64, u64)>,
    pub synchronous: cas_core::io_metrics::Counters,
    pub boundary: u64,
    pub published_at_receive: u64,
    pub dependency_observed_ns: u64,
    pub prepare_ns: u64,
    pub advance_ns: u64,
    pub submission_wait_ns: u64,
    // SQE publication to CQE observation, including kernel/reactor scheduling.
    // WAL, manifest page, chunk header, chunk payload, shared-fetch notification.
    pub io_ns: [u64; 5],
    pub io_calls: [u64; 5],
    pub cache_hits: u64,
    pub completion_lock_ns: u64,
    pub success: bool,
}

impl ReadTrace {
    pub fn at(&self) -> u64 {
        ns(self.observed.elapsed())
    }
}

#[derive(serde::Serialize)]
pub(crate) struct Observer {
    #[serde(skip)]
    origin: Instant,
    #[serde(skip)]
    slots: BudgetVec<Option<Seen>, BudgetAllocator>,
    #[serde(skip)]
    stalled: [Option<Stall>; QUEUES],
    pub observed_reads: u64,
    pub completed_reads: u64,
    pub failed_reads: u64,
    pub missing_observations: u64,
    pub dropped_traces: u64,
    pub invalid_observations: u64,
    pub peek_observations: u64,
    pub queue_lock_wait_ns: u64,
    pub max_queue_lock_wait_ns: u64,
    // All reads, including those not retained among the slowest records.
    pub total_ns: u64,
    pub before_head_ns: u64,
    pub behind_write_ns: u64,
    pub after_admission_ns: u64,
    // Total, before head, at head, dispatch, command channel, reactor,
    // execution, response channel, frontend completion, after admission.
    pub phases: [Histogram; 10],
    pub synchronous: cas_core::io_metrics::Counters,
    pub io_ns: [u64; 5],
    pub behind_reason_ns: [u64; REASONS],
    pub head_reason_ns: [u64; REASONS],
    pub disjoint_blocked_reads: u64,
    pub disjoint_blocked_ns: u64,
    pub slowest: [Option<ReadTrace>; RETAIN],
}

impl Observer {
    pub fn new(metadata: &Arc<Budget>) -> io::Result<BudgetBox<Self, BudgetAllocator>> {
        let mut slots = crate::local::reserved_vec(QUEUES * SLOTS, metadata)?;
        slots.resize(QUEUES * SLOTS, None);
        BudgetBox::try_new_in(
            Self {
                origin: Instant::now(),
                slots,
                stalled: [None; QUEUES],
                observed_reads: 0,
                completed_reads: 0,
                failed_reads: 0,
                missing_observations: 0,
                dropped_traces: 0,
                invalid_observations: 0,
                peek_observations: 0,
                queue_lock_wait_ns: 0,
                max_queue_lock_wait_ns: 0,
                total_ns: 0,
                before_head_ns: 0,
                behind_write_ns: 0,
                after_admission_ns: 0,
                phases: [Histogram::EMPTY; 10],
                synchronous: Default::default(),
                io_ns: [0; 5],
                behind_reason_ns: [0; REASONS],
                head_reason_ns: [0; REASONS],
                disjoint_blocked_reads: 0,
                disjoint_blocked_ns: 0,
                slowest: [None; RETAIN],
            },
            BudgetAllocator::new(metadata.clone()),
        )
        .map_err(|_| io::ErrorKind::OutOfMemory.into())
    }

    pub fn own(&mut self, trace: ReadTrace) -> Option<OwnedTrace> {
        match BudgetBox::try_new_in(trace, self.slots.allocator().clone()) {
            Ok(trace) => Some(trace),
            Err(_) => {
                self.dropped_traces += 1;
                None
            }
        }
    }

    fn slot(queue: u16, available: u16) -> usize {
        usize::from(queue) * SLOTS + usize::from(available) % SLOTS
    }

    fn account(&mut self, queue: u16, now: Instant) {
        let Some(stall) = &mut self.stalled[usize::from(queue)] else {
            return;
        };
        let start = usize::from(queue) * SLOTS;
        for seen in self.slots[start..start + SLOTS].iter_mut().flatten() {
            let elapsed = ns(now.saturating_duration_since(stall.since.max(seen.observed)));
            let distance = seen.available.wrapping_sub(stall.available);
            if distance == 0 {
                seen.head_stall_ns += elapsed;
                seen.head_reason_ns[stall.reason] += elapsed;
            } else if distance < SLOTS as u16 && stall.write {
                seen.behind_write_ns += elapsed;
                seen.behind_reason_ns[stall.reason] += elapsed;
                if elapsed != 0
                    && let Some((start, end)) = stall.range
                {
                    seen.blocker_range = Some(
                        seen.blocker_range
                            .map_or((start, end), |(a, b)| (a.min(start), b.max(end))),
                    );
                }
            }
        }
        stall.since = now;
    }

    pub fn reset(&mut self, queue: Option<usize>) {
        for index in 0..QUEUES {
            if queue.is_none_or(|queue| queue == index) {
                self.slots[index * SLOTS..(index + 1) * SLOTS].fill(None);
                self.stalled[index] = None;
            }
        }
    }

    /// Read only the published cursor. No decoding, payload access or queue mutation.
    pub fn observe(&mut self, queue: u16, next: u16, end: Option<u16>, size: u16, now: Instant) {
        self.account(queue, now);
        let Some(end) = end.filter(|end| size as usize <= SLOTS && end.wrapping_sub(next) <= size)
        else {
            self.invalid_observations += 1;
            self.reset(Some(usize::from(queue)));
            return;
        };
        for ahead in 0..end.wrapping_sub(next) {
            let available = next.wrapping_add(ahead);
            let slot = &mut self.slots[Self::slot(queue, available)];
            if slot.is_none_or(|seen| seen.available != available) {
                *slot = Some(Seen {
                    available,
                    observed: now,
                    head: None,
                    ahead,
                    behind_write_ns: 0,
                    head_stall_ns: 0,
                    behind_reason_ns: [0; REASONS],
                    head_reason_ns: [0; REASONS],
                    blocker_range: None,
                });
            }
        }
    }

    pub fn head(&mut self, queue: u16, available: u16, now: Instant) {
        self.account(queue, now);
        let slot = &mut self.slots[Self::slot(queue, available)];
        if slot.is_none_or(|seen| seen.available != available) {
            // The guest may publish between observe's cursor load and peek.
            // Peek has now proved this head exists. Start its lower-bound
            // observation here rather than losing the trace at consumption.
            self.peek_observations += 1;
            *slot = Some(Seen {
                available,
                observed: now,
                head: Some(now),
                ahead: 0,
                behind_write_ns: 0,
                head_stall_ns: 0,
                behind_reason_ns: [0; REASONS],
                head_reason_ns: [0; REASONS],
                blocker_range: None,
            });
        }
        slot.as_mut().unwrap().head.get_or_insert(now);
    }

    pub fn waiting(
        &mut self,
        queue: u16,
        available: u16,
        range: Option<(u64, u64)>,
        reason: usize,
        now: Instant,
    ) {
        self.account(queue, now);
        self.stalled[usize::from(queue)] = Some(Stall {
            available,
            since: now,
            write: range.is_some(),
            range,
            reason,
        });
    }

    pub fn consume(
        &mut self,
        queue: u16,
        available: u16,
        read: Option<(u64, u64, usize)>,
        now: Instant,
    ) -> Option<ReadTrace> {
        self.account(queue, now);
        self.stalled[usize::from(queue)] = None;
        let seen = self.slots[Self::slot(queue, available)].take();
        let (id, offset, bytes) = read?;
        let Some(seen) = seen.filter(|seen| seen.available == available) else {
            self.missing_observations += 1;
            return None;
        };
        self.observed_reads += 1;
        Some(ReadTrace {
            observed: seen.observed,
            id,
            queue,
            available,
            descriptor: 0,
            offset,
            bytes,
            observed_ns: ns(seen.observed.duration_since(self.origin)),
            ahead: seen.ahead,
            head_ns: ns(seen.head.unwrap_or(now).duration_since(seen.observed)),
            admitted_ns: ns(now.duration_since(seen.observed)),
            enqueued_ns: 0,
            received_ns: 0,
            started_ns: 0,
            responded_ns: 0,
            frontend_received_ns: 0,
            finished_ns: 0,
            behind_write_ns: seen.behind_write_ns,
            head_stall_ns: seen.head_stall_ns,
            behind_reason_ns: seen.behind_reason_ns,
            head_reason_ns: seen.head_reason_ns,
            blocker_range: seen.blocker_range,
            synchronous: Default::default(),
            boundary: 0,
            published_at_receive: 0,
            dependency_observed_ns: 0,
            prepare_ns: 0,
            advance_ns: 0,
            submission_wait_ns: 0,
            io_ns: [0; 5],
            io_calls: [0; 5],
            cache_hits: 0,
            completion_lock_ns: 0,
            success: false,
        })
    }

    pub fn complete(&mut self, trace: ReadTrace) {
        self.completed_reads += 1;
        self.failed_reads += u64::from(!trace.success);
        self.total_ns += trace.finished_ns;
        self.before_head_ns += trace.head_ns;
        self.behind_write_ns += trace.behind_write_ns;
        self.after_admission_ns += trace.finished_ns.saturating_sub(trace.admitted_ns);
        let times = [
            trace.finished_ns,
            trace.head_ns,
            trace.admitted_ns.saturating_sub(trace.head_ns),
            trace.enqueued_ns.saturating_sub(trace.admitted_ns),
            trace.received_ns.saturating_sub(trace.enqueued_ns),
            trace.started_ns.saturating_sub(trace.received_ns),
            trace.responded_ns.saturating_sub(trace.started_ns),
            trace
                .frontend_received_ns
                .saturating_sub(trace.responded_ns),
            trace.finished_ns.saturating_sub(trace.frontend_received_ns),
            trace.finished_ns.saturating_sub(trace.admitted_ns),
        ];
        for (histogram, value) in self.phases.iter_mut().zip(times) {
            histogram.add(value);
        }
        self.synchronous.add(trace.synchronous);
        for (total, value) in self.io_ns.iter_mut().zip(trace.io_ns) {
            *total += value;
        }
        for (total, value) in self.behind_reason_ns.iter_mut().zip(trace.behind_reason_ns) {
            *total += value;
        }
        for (total, value) in self.head_reason_ns.iter_mut().zip(trace.head_reason_ns) {
            *total += value;
        }
        if let Some((start, end)) = trace.blocker_range
            && (trace.offset >= end || trace.offset.saturating_add(trace.bytes as u64) <= start)
        {
            self.disjoint_blocked_reads += 1;
            self.disjoint_blocked_ns += trace.behind_write_ns;
        }
        let slot = self
            .slowest
            .iter_mut()
            .min_by_key(|slot| slot.map_or(0, |trace| trace.finished_ns))
            .unwrap();
        if slot.is_none_or(|old| trace.finished_ns > old.finished_ns) {
            *slot = Some(trace);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn histogram_covers_zero_boundaries_and_saturates_the_last_bucket() {
        let mut histogram = Histogram::EMPTY;
        for value in [0, 1, 2, 3, 4, u64::MAX] {
            histogram.add(value);
        }
        assert_eq!(histogram.count, 6);
        assert_eq!(&histogram.buckets[0][..4], &[1, 1, 2, 1]);
        assert_eq!(histogram.buckets[1][31], 1);
        assert_eq!(histogram.total_ns, u64::MAX);
    }

    #[test]
    fn reason_transitions_partition_wait_and_disjointness_is_conservative() {
        let mut observer = Observer::new(&crate::local::metadata_budget()).unwrap();
        let start = Instant::now();
        observer.observe(0, 0, Some(2), 256, start);
        observer.waiting(0, 0, Some((8192, 12288)), 6, start);
        observer.waiting(0, 0, Some((8192, 12288)), 0, start + Duration::from_secs(2));
        let end = start + Duration::from_secs(3);
        observer.consume(0, 0, None, end);
        observer.head(0, 1, end);
        let mut trace = observer.consume(0, 1, Some((1, 0, 4096)), end).unwrap();
        assert_eq!(trace.behind_reason_ns[6], 2_000_000_000);
        assert_eq!(trace.behind_reason_ns[0], 1_000_000_000);
        assert_eq!(
            trace.behind_reason_ns.iter().sum::<u64>(),
            trace.behind_write_ns
        );
        trace.finished_ns = trace.admitted_ns;
        observer.complete(trace);
        assert_eq!(observer.disjoint_blocked_reads, 1);
        assert_eq!(observer.disjoint_blocked_ns, 3_000_000_000);
        trace.offset = 8192;
        observer.complete(trace);
        assert_eq!(observer.disjoint_blocked_reads, 1);
        assert_eq!(observer.phases[0].count, 2);
    }

    #[test]
    fn observes_behind_a_blocked_write_without_admitting_the_read() {
        let mut observer = Observer::new(&crate::local::metadata_budget()).unwrap();
        let start = Instant::now();
        observer.observe(0, 65535, Some(1), 256, start); // wrap: write, then read
        observer.head(0, 65535, start);
        observer.waiting(0, 65535, Some((8192, 12288)), 6, start);
        let resumed = start + Duration::from_secs(9);
        assert!(observer.consume(0, 65535, None, resumed).is_none());
        observer.head(0, 0, resumed);
        let trace = observer
            .consume(0, 0, Some((12, 4096, 4096)), resumed)
            .unwrap();
        assert_eq!(trace.head_ns, 9_000_000_000);
        assert_eq!(trace.behind_write_ns, trace.head_ns);
        assert_eq!(trace.head_stall_ns, 0);
        assert_eq!(trace.ahead, 1);
        assert_eq!(observer.observed_reads, 1);
    }

    #[test]
    fn late_observation_and_other_queues_do_not_inherit_earlier_waits() {
        let mut observer = Observer::new(&crate::local::metadata_budget()).unwrap();
        let start = Instant::now();
        observer.observe(0, 0, Some(1), 256, start);
        observer.waiting(0, 0, Some((8192, 12288)), 6, start);
        let later = start + Duration::from_secs(8);
        observer.observe(0, 0, Some(2), 256, later);
        observer.observe(1, 0, Some(1), 256, later);
        observer.head(1, 0, later);
        let separate = observer.consume(1, 0, Some((1, 0, 4096)), later).unwrap();
        assert_eq!(separate.behind_write_ns, 0);
        let end = later + Duration::from_secs(1);
        observer.consume(0, 0, None, end);
        observer.head(0, 1, end);
        let same = observer.consume(0, 1, Some((2, 0, 4096)), end).unwrap();
        assert_eq!(same.behind_write_ns, 1_000_000_000);
        observer.reset(None);
        assert!(observer.consume(0, 1, Some((3, 0, 4096)), end).is_none());
        assert_eq!(observer.missing_observations, 1);
    }

    #[test]
    fn publication_between_cursor_observation_and_peek_starts_at_the_head() {
        let mut observer = Observer::new(&crate::local::metadata_budget()).unwrap();
        let start = Instant::now();
        observer.observe(0, 0, Some(0), 256, start);
        let peeked = start + Duration::from_millis(1);
        observer.head(0, 0, peeked);
        let trace = observer.consume(0, 0, Some((1, 0, 4096)), peeked).unwrap();
        assert_eq!(trace.observed, peeked);
        assert_eq!(trace.head_ns, 0);
        assert_eq!(trace.behind_write_ns, 0);
        assert_eq!(observer.peek_observations, 1);
        assert_eq!(observer.missing_observations, 0);
    }

    #[test]
    fn per_read_trace_allocation_is_charged_and_denial_only_drops_the_trace() {
        let budget = crate::local::metadata_budget();
        let mut observer = Observer::new(&budget).unwrap();
        let now = Instant::now();
        observer.observe(0, 0, Some(1), 256, now);
        let trace = observer.consume(0, 0, Some((1, 0, 4096)), now).unwrap();
        let before = budget.usage().current.bytes;
        let owned = observer.own(trace).unwrap();
        assert!(budget.usage().current.bytes > before);
        drop(owned);
        assert_eq!(budget.usage().current.bytes, before);
        let held = budget
            .reserve(cas_core::budget::Amount {
                bytes: 128 * cas_core::MAX_REQUEST_BYTES - before,
                requests: 0,
            })
            .unwrap();
        assert!(observer.own(trace).is_none());
        assert_eq!(observer.dropped_traces, 1);
        drop(held);
        assert!(observer.own(trace).is_some());
    }

    #[test]
    fn retention_is_bounded_and_observer_allocations_release() {
        let budget = crate::local::metadata_budget();
        let before = budget.usage().current.bytes;
        let mut observer = Observer::new(&budget).unwrap();
        for id in 0..100 {
            let now = Instant::now();
            observer.observe(0, id, Some(id + 1), 256, now);
            observer.head(0, id, now);
            let mut trace = observer
                .consume(0, id, Some((u64::from(id), 0, 4096)), now)
                .unwrap();
            trace.finished_ns = u64::from(id + 1);
            trace.success = true;
            observer.complete(trace);
        }
        assert_eq!(observer.completed_reads, 100);
        assert_eq!(observer.slowest.iter().flatten().count(), RETAIN);
        assert_eq!(
            observer
                .slowest
                .iter()
                .flatten()
                .map(|trace| trace.finished_ns)
                .min(),
            Some(69)
        );
        observer.observe(0, 0, Some(257), 256, Instant::now());
        assert_eq!(observer.invalid_observations, 1);
        drop(observer);
        assert_eq!(budget.usage().current.bytes, before);
    }
}
