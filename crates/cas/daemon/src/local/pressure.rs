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
    WalRotation,
    WalIndex,
    AppendCredits,
}

impl Reason {
    /// Every variant once; counters index by discriminant, not by position here.
    const ALL: [Self; 9] = [
        Self::HostAdmission,
        Self::RequestCredits,
        Self::ReadCredits,
        Self::ReadOwnerAllocation,
        Self::Staging,
        Self::Physical,
        Self::WalRotation,
        Self::WalIndex,
        Self::AppendCredits,
    ];

    /// The report key, matching the serde `snake_case` spelling of the variant.
    const fn name(self) -> &'static str {
        match self {
            Self::HostAdmission => "host_admission",
            Self::RequestCredits => "request_credits",
            Self::ReadCredits => "read_credits",
            Self::ReadOwnerAllocation => "read_owner_allocation",
            Self::Staging => "staging",
            Self::Physical => "physical",
            Self::WalRotation => "wal_rotation",
            Self::WalIndex => "wal_index",
            Self::AppendCredits => "append_credits",
        }
    }
}

/// Storage refusal reasons; the read trace reserves two indices ahead of these.
pub(crate) const COUNT: usize = Reason::ALL.len();
const _: () = {
    let mut index = 0;
    while index < COUNT {
        assert!(
            Reason::ALL[index] as usize == index,
            "ALL lists a variant twice"
        );
        index += 1;
    }
};

/// Serializes as one object keyed by reason name, in declaration order.
#[derive(Default)]
pub(super) struct Counters([AtomicU64; COUNT]);

impl Counters {
    pub fn record(&self, reason: Reason) {
        self.0[reason as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn denied(&self, reason: Reason) -> Reason {
        self.record(reason);
        reason
    }
}

impl serde::Serialize for Counters {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(COUNT))?;
        for reason in Reason::ALL {
            map.serialize_entry(
                reason.name(),
                &self.0[reason as usize].load(Ordering::Relaxed),
            )?;
        }
        map.end()
    }
}

/// A temporary refusal is a normal admission outcome, never a terminal error.
pub(crate) enum Decision<T> {
    Ready(T),
    Waiting(Reason),
}

impl<T> Decision<T> {
    pub fn ready(self) -> Option<T> {
        match self {
            Self::Ready(value) => Some(value),
            Self::Waiting(_) => None,
        }
    }
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> Decision<U> {
        match self {
            Self::Ready(value) => Decision::Ready(map(value)),
            Self::Waiting(reason) => Decision::Waiting(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_keys_match_the_serialized_variant_names() {
        let counters = Counters::default();
        for (times, reason) in Reason::ALL.into_iter().enumerate() {
            for _ in 0..times {
                counters.record(reason);
            }
        }
        let report = serde_json::to_value(&counters).unwrap();
        assert_eq!(report.as_object().unwrap().len(), COUNT);
        for (times, reason) in Reason::ALL.into_iter().enumerate() {
            let key = serde_json::to_value(reason).unwrap();
            assert_eq!(key, reason.name());
            assert_eq!(report[reason.name()], times);
        }
    }
}
