//! A selected compaction attempt has exactly one recorded outcome.
use super::*;

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Totals {
    pub batches: u64,
    pub input_bytes: u64,
    pub candidate_output_bytes: u64,
    pub active_ns: u64,
    pub deferred: u64,
    pub deferred_ns: u64,
    pub failed: u64,
    pub failed_ns: u64,
}

enum Outcome {
    Failed,
    Deferred,
    Completed { input: u64, output: u64 },
}

pub(super) struct Attempt {
    totals: BudgetArc<Mutex<Totals>>,
    started: Instant,
    outcome: Outcome,
}

impl Attempt {
    pub fn new(totals: &BudgetArc<Mutex<Totals>>) -> Self {
        Self {
            totals: totals.clone(),
            started: Instant::now(),
            outcome: Outcome::Failed,
        }
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
        let elapsed = self.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let mut totals = self.totals.lock().expect("compaction statistics poisoned");
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
        drop(totals);
        assert_eq!(metadata.usage().current, Amount::default());
    }
}
