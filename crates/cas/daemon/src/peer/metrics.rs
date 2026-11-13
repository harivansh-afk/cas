//! Always-on, bounded remote-read milestones; no formatting on the IO path.
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator, BudgetArc};
use std::{
    io,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

const RETAIN: usize = 1024;
/// Race outcome codes recorded in the ring: no race, first owner, second owner, local copy.
pub const WINNER_NONE: u8 = 0;
pub const WINNER_FIRST: u8 = 1;
pub const WINNER_SECOND: u8 = 2;
pub const WINNER_LOCAL: u8 = 3;
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Phases {
    pub send_ns: u64,
    pub first_byte_ns: u64,
    pub verified_ns: u64,
    /// The winning source and budget travel with the completing transport's
    /// milestones; the record exposes them at its top level.
    #[serde(skip)]
    pub race_budget_us: u32,
    #[serde(skip)]
    pub race_winner: u8,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Record {
    pub id: u64,
    pub received_ns: u64,
    pub phases: Phases,
    pub completed_ns: u64,
    pub chunks: u64,
    pub success: bool,
    /// Last raced chunk of this read: budget in microseconds and WINNER_* code.
    pub race_budget_us: u32,
    pub race_winner: u8,
}
#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct Counters {
    pub inline_attempts: u64,
    pub inline_completed: u64,
    pub gets: u64,
    pub verified: u64,
    pub errors: u64,
    pub connections: u64,
    pub max_inflight: usize,
    pub server_memory: u64,
    pub server_store: u64,
    pub server_missing: u64,
    pub server_errors: u64,
    pub has: u64,
    pub completed_reads: u64,
    pub failed_reads: u64,
    pub races_started: u64,
    pub race_won_first: u64,
    pub race_won_second: u64,
    pub race_won_local: u64,
    pub race_discarded: u64,
    pub race_wasted_wire_bytes: u64,
    /// Armed races never started: racer table full or stopping, or the
    /// challenger's slot pool exhausted.
    pub race_dropped: u64,
}
/// GETs issued without an image slot (harness clients, HAS probes).
pub(crate) const UNATTRIBUTED: usize = usize::MAX;
/// Race-independent per-image remote-read attribution. Cumulative; restarts
/// with the host. `completed_reads`, `failed_reads` and `chunks` count the
/// image's reads and the remote chunks they resolved, including chunks a
/// coalesced waiter inherited from another read's GET; `verified_bytes` is
/// credited once, at transport verification, to the image that issued the GET.
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ImageCounters {
    pub completed_reads: u64,
    pub failed_reads: u64,
    pub chunks: u64,
    pub verified_bytes: u64,
}
#[derive(Default, Clone, Copy)]
struct ImageSlot {
    image: Option<[u8; 16]>,
    counters: ImageCounters,
}
struct State {
    counters: Counters,
    recent: Vec<Record, BudgetAllocator>,
    cursor: usize,
    images: Vec<ImageSlot, BudgetAllocator>,
}
pub struct Metrics {
    wire_sent: AtomicU64,
    wire_received: AtomicU64,
    origin: Instant,
    state: Mutex<State>,
}
impl Metrics {
    /// `slots` preallocates one per-image attribution slot per host image slot.
    pub fn new(budget: &Arc<Budget>, slots: usize) -> io::Result<BudgetArc<Self>> {
        let mut images = crate::local::reserved_vec(slots, budget)?;
        images.resize(slots, ImageSlot::default());
        BudgetArc::try_new(
            Self {
                wire_sent: AtomicU64::new(0),
                wire_received: AtomicU64::new(0),
                origin: Instant::now(),
                state: Mutex::new(State {
                    counters: Counters::default(),
                    recent: crate::local::reserved_vec(RETAIN, budget)?,
                    cursor: 0,
                    images,
                }),
            },
            budget,
        )
    }
    /// Name the image served from a preallocated slot; the host checks the
    /// slot limit once before creating the image.
    pub fn bind(&self, slot: usize, image: [u8; 16]) {
        let mut state = self.state.lock().expect("peer metrics poisoned");
        state.images[slot].image = Some(image);
    }
    /// One GET finished in the transport: host counters, and the verified
    /// payload credited to the issuing image (not to coalesced waiters).
    pub(crate) fn transport_finished(&self, image: usize, verified: bool) {
        let mut state = self.state.lock().expect("peer metrics poisoned");
        state.counters.verified += u64::from(verified);
        state.counters.errors += u64::from(!verified);
        if verified && let Some(slot) = state.images.get_mut(image) {
            slot.counters.verified_bytes += cas_core::BLOCK_SIZE as u64;
        }
    }
    pub fn image(&self, image: [u8; 16]) -> ImageCounters {
        let state = self.state.lock().expect("peer metrics poisoned");
        state
            .images
            .iter()
            .filter(|slot| slot.image == Some(image))
            .fold(ImageCounters::default(), |sum, slot| {
                sum.plus(slot.counters)
            })
    }
    pub fn at(&self, time: Instant) -> u64 {
        time.saturating_duration_since(self.origin)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }
    pub fn now(&self) -> u64 {
        self.at(Instant::now())
    }
    pub fn count(&self, update: impl FnOnce(&mut Counters)) {
        update(&mut self.state.lock().expect("peer metrics poisoned").counters);
    }
    pub fn report(&self) -> serde_json::Value {
        let state = self.state.lock().expect("peer metrics poisoned");
        // Keyed by image ID; a returning migration reuses the ID in a new
        // slot, so equal IDs are summed here rather than on the IO path.
        let mut totals = std::collections::BTreeMap::<[u8; 16], ImageCounters>::new();
        for slot in state.images.iter() {
            if let Some(image) = slot.image {
                let total = totals.entry(image).or_default();
                *total = total.plus(slot.counters);
            }
        }
        let images: serde_json::Map<String, serde_json::Value> = totals
            .into_iter()
            .map(|(image, counters)| {
                (
                    cas_core::id::hex(image),
                    serde_json::to_value(counters).expect("image counters"),
                )
            })
            .collect();
        serde_json::json!({"counters": state.counters, "recent": &*state.recent, "images": images,
            "get_wire":{"sent":self.wire_sent.load(Ordering::Relaxed),"received":self.wire_received.load(Ordering::Relaxed)},
            "retention": RETAIN, "next_slot": state.cursor,
            "clock": "nanoseconds since peer runtime start; monotonic, host local"})
    }
    pub(crate) fn wire(&self, sending: bool, bytes: usize) {
        let count = if sending {
            &self.wire_sent
        } else {
            &self.wire_received
        };
        count.fetch_add(bytes as u64, Ordering::Relaxed);
    }
    fn complete(&self, record: Record, slot: usize) {
        let mut state = self.state.lock().expect("peer metrics poisoned");
        state.counters.completed_reads += 1;
        state.counters.failed_reads += u64::from(!record.success);
        if let Some(image) = state.images.get_mut(slot) {
            image.counters = image.counters.plus(ImageCounters {
                completed_reads: 1,
                failed_reads: u64::from(!record.success),
                chunks: record.chunks,
                verified_bytes: 0,
            });
        }
        let cursor = state.cursor;
        if state.recent.len() < RETAIN {
            state.recent.push(record);
        } else {
            state.recent[cursor] = record;
        }
        state.cursor = (cursor + 1) % RETAIN;
    }
}

impl ImageCounters {
    fn plus(self, other: Self) -> Self {
        Self {
            completed_reads: self.completed_reads + other.completed_reads,
            failed_reads: self.failed_reads + other.failed_reads,
            chunks: self.chunks + other.chunks,
            verified_bytes: self.verified_bytes + other.verified_bytes,
        }
    }
}

/// The shared host metrics seen from one image slot; cloning is pointer-sized.
#[derive(Clone)]
pub struct ImageMetrics {
    metrics: BudgetArc<Metrics>,
    slot: usize,
}
impl ImageMetrics {
    pub(crate) fn new(metrics: BudgetArc<Metrics>, slot: usize) -> Self {
        Self { metrics, slot }
    }
}

pub(crate) struct Trace {
    metrics: ImageMetrics,
    record: Record,
}
impl Trace {
    pub fn new(metrics: ImageMetrics, id: u64, received: Instant) -> Self {
        Self {
            record: Record {
                id,
                received_ns: metrics.metrics.at(received),
                ..Record::default()
            },
            metrics,
        }
    }
    pub fn fetched(&mut self, phases: Phases) {
        // For multi-chunk reads: first send/byte, last verification; chunks disambiguates.
        if self.record.chunks == 0 {
            self.record.phases = phases;
        } else {
            self.record.phases.verified_ns = phases.verified_ns;
        }
        if phases.race_winner != WINNER_NONE {
            self.record.race_winner = phases.race_winner;
            self.record.race_budget_us = phases.race_budget_us;
        }
        self.record.chunks += 1;
    }
    pub fn finish(&mut self, success: bool) {
        if self.record.chunks != 0 && self.record.completed_ns == 0 {
            self.record.completed_ns = self.metrics.metrics.now();
            self.record.success = success;
            self.metrics
                .metrics
                .complete(self.record, self.metrics.slot);
        }
    }
}
impl Drop for Trace {
    fn drop(&mut self) {
        self.finish(false);
    }
}
