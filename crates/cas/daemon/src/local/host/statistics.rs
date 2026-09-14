//! A selected compaction attempt has exactly one recorded outcome.
use super::*;

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Phases {
    load: u64,
    prepare: u64,
    reserve: u64,
    chunks: u64,
    manifest: u64,
    reclaim: u64,
}

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Load,
    Prepare,
    Reserve,
    Chunks,
    Manifest,
    Reclaim,
}

impl Phases {
    fn add(&mut self, other: Self) {
        self.load += other.load;
        self.prepare += other.prepare;
        self.reserve += other.reserve;
        self.chunks += other.chunks;
        self.manifest += other.manifest;
        self.reclaim += other.reclaim;
    }
    fn record(&mut self, phase: Phase, ns: u64) {
        let value = match phase {
            Phase::Load => &mut self.load,
            Phase::Prepare => &mut self.prepare,
            Phase::Reserve => &mut self.reserve,
            Phase::Chunks => &mut self.chunks,
            Phase::Manifest => &mut self.manifest,
            Phase::Reclaim => &mut self.reclaim,
        };
        *value += ns;
    }
}

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Totals {
    /// Normal image turns, including rotation, reclaim-only work and failures.
    /// Excludes host collection/snapshot work and its quiescent compactions.
    pub operations: cas_core::io_metrics::Counters,
    pub manifest_changes: u64,
    pub manifest_written_pages: u64,
    pub manifest_old_page_reads: u64,
    pub manifest_allocated_bytes: u64,
    pub exchange_calls: u64,
    pub exchange_ns: u64,
    pub staging_reopens: u64,
    pub reclaimed_bytes: u64,
    pub batches: u64,
    pub input_bytes: u64,
    pub candidate_output_bytes: u64,
    pub active_ns: u64,
    pub deferred: u64,
    pub deferred_ns: u64,
    pub failed: u64,
    pub failed_ns: u64,
    /// Elapsed time across all attempts, including partial failed phases.
    pub phases_ns: Phases,
}

enum Outcome {
    Failed,
    Deferred,
    Completed { input: u64, output: u64 },
}

pub(super) struct Attempt {
    totals: BudgetArc<Mutex<Totals>>,
    started: Instant,
    checkpoint: Instant,
    phase: Phase,
    phases: Phases,
    outcome: Outcome,
}

impl Attempt {
    pub fn new(totals: &BudgetArc<Mutex<Totals>>) -> Self {
        let now = Instant::now();
        Self {
            totals: totals.clone(),
            started: now,
            checkpoint: now,
            phase: Phase::Load,
            phases: Phases::default(),
            outcome: Outcome::Failed,
        }
    }
    fn record(&mut self, now: Instant) {
        self.phases.record(
            self.phase,
            now.duration_since(self.checkpoint)
                .as_nanos()
                .min(u64::MAX as u128) as u64,
        );
        self.checkpoint = now;
    }
    pub fn advance(&mut self, next: Phase) {
        self.record(Instant::now());
        self.phase = next;
    }
    pub fn deferred(mut self) {
        self.outcome = Outcome::Deferred;
    }
    pub fn completed(mut self, input: u64, output: u64) {
        self.outcome = Outcome::Completed { input, output };
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let now = Instant::now();
        self.record(now);
        let elapsed = now
            .duration_since(self.started)
            .as_nanos()
            .min(u64::MAX as u128) as u64;
        let mut totals = self.totals.lock().expect("compaction statistics poisoned");
        totals.phases_ns.add(self.phases);
        match self.outcome {
            Outcome::Failed => {
                totals.failed += 1;
                totals.failed_ns += elapsed;
            }
            Outcome::Deferred => {
                totals.deferred += 1;
                totals.deferred_ns += elapsed;
            }
            Outcome::Completed { input, output } => {
                totals.batches += 1;
                totals.input_bytes += input;
                totals.candidate_output_bytes += output;
                totals.active_ns += elapsed;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn early_return_and_deferral_do_not_inflate_successful_drain() {
        let metadata = metadata_budget();
        let totals = BudgetArc::try_new(Mutex::new(Totals::default()), &metadata).unwrap();
        let failed = Attempt::new(&totals);
        drop(failed); // An error before loading/preparing payload still counts.
        Attempt::new(&totals).deferred();
        Attempt::new(&totals).completed(4096, 8192);
        let result = *totals.lock().unwrap();
        assert_eq!((result.batches, result.failed, result.deferred), (1, 1, 1));
        assert_eq!(result.input_bytes, 4096);
        assert_eq!(result.candidate_output_bytes, 8192);
        let phases = result.phases_ns;
        assert_eq!(
            phases.load
                + phases.prepare
                + phases.reserve
                + phases.chunks
                + phases.manifest
                + phases.reclaim,
            result.active_ns + result.failed_ns + result.deferred_ns
        );
        drop(totals);
        assert_eq!(metadata.usage().current, Amount::default());
    }
}
