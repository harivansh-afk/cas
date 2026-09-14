//! First refusing storage gate per reserve attempt, not unique requests or wait time.
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reason {
    HostAdmission,
    RequestCredits,
    ReadCredits,
    ReadOwnerAllocation,
    Staging,
    Physical,
    WalFailed,
    WalRotation,
    WalIndex,
    AppendCredits,
    InvalidWrite,
}

const NAMES: [&str; 11] = [
    "host_admission",
    "request_credits",
    "read_credits",
    "read_owner_allocation",
    "staging",
    "physical",
    "wal_failed",
    "wal_rotation",
    "wal_index",
    "append_credits",
    "invalid_write",
];

#[derive(Default)]
pub(super) struct Counters([AtomicU64; NAMES.len()]);

impl Counters {
    pub fn record(&self, reason: Reason) {
        self.0[reason as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn denied(&self, reason: Reason) -> Reason {
        self.record(reason);
        reason
    }

    pub fn report(&self) -> serde_json::Value {
        NAMES
            .iter()
            .zip(&self.0)
            .map(|(name, count)| {
                (
                    (*name).to_owned(),
                    serde_json::Value::from(count.load(Ordering::Relaxed)),
                )
            })
            .collect()
    }
}
