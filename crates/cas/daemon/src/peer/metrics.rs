//! Always-on, bounded remote-read milestones; no formatting on the IO path.
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator, BudgetArc};
use std::{
    io,
    sync::{Arc, Mutex},
    time::Instant,
};

const RETAIN: usize = 1024;
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Phases {
    pub send_ns: u64,
    pub first_byte_ns: u64,
    pub verified_ns: u64,
}
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Record {
    pub id: u64,
    pub received_ns: u64,
    pub phases: Phases,
    pub completed_ns: u64,
    pub chunks: u64,
    pub success: bool,
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
}
struct State {
    counters: Counters,
    recent: Vec<Record, BudgetAllocator>,
    cursor: usize,
}
pub struct Metrics {
    origin: Instant,
    state: Mutex<State>,
}
impl Metrics {
    pub fn new(budget: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        BudgetArc::try_new(
            Self {
                origin: Instant::now(),
                state: Mutex::new(State {
                    counters: Counters::default(),
                    recent: crate::local::reserved_vec(RETAIN, budget)?,
                    cursor: 0,
                }),
            },
            budget,
        )
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
        serde_json::json!({"counters": state.counters, "recent": &*state.recent,
            "retention": RETAIN, "next_slot": state.cursor,
            "clock": "nanoseconds since peer runtime start; monotonic, host local"})
    }
    fn complete(&self, record: Record) {
        let mut state = self.state.lock().expect("peer metrics poisoned");
        state.counters.completed_reads += 1;
        state.counters.failed_reads += u64::from(!record.success);
        let cursor = state.cursor;
        if state.recent.len() < RETAIN {
            state.recent.push(record);
        } else {
            state.recent[cursor] = record;
        }
        state.cursor = (cursor + 1) % RETAIN;
    }
}

pub(crate) struct Trace {
    metrics: BudgetArc<Metrics>,
    record: Record,
}
impl Trace {
    pub fn new(metrics: BudgetArc<Metrics>, id: u64, received: Instant) -> Self {
        Self {
            record: Record {
                id,
                received_ns: metrics.at(received),
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
        self.record.chunks += 1;
    }
    pub fn finish(&mut self, success: bool) {
        if self.record.chunks != 0 && self.record.completed_ns == 0 {
            self.record.completed_ns = self.metrics.now();
            self.record.success = success;
            self.metrics.complete(self.record);
        }
    }
}
impl Drop for Trace {
    fn drop(&mut self) {
        self.finish(false);
    }
}
