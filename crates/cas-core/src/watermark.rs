//! Contiguous confirmations: completing N never hides an unfinished N - 1.

use std::collections::BTreeSet;

#[derive(Debug, Default)]
pub struct DurablePrefix {
    issued: u64,
    confirmed: u64,
    pending: BTreeSet<u64>,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("sequence numbers exhausted")]
    Exhausted,
    #[error("sequence {0} was not issued")]
    Unissued(u64),
    #[error("watermarks must advance monotonically with O <= D <= E")]
    InvalidWatermark,
}

impl DurablePrefix {
    /// Called in the same critical section that orders appends.
    pub fn issue(&mut self) -> Result<u64, Error> {
        self.issued = self.issued.checked_add(1).ok_or(Error::Exhausted)?;
        Ok(self.issued)
    }

    /// Duplicate acknowledgments are idempotent. Out-of-order acknowledgments
    /// are retained until all preceding appends have been confirmed.
    pub fn confirm(&mut self, sequence: u64) -> Result<u64, Error> {
        if sequence == 0 || sequence > self.issued {
            return Err(Error::Unissued(sequence));
        }
        if sequence > self.confirmed {
            self.pending.insert(sequence);
            while let Some(next) = self.confirmed.checked_add(1) {
                if !self.pending.remove(&next) {
                    break;
                }
                self.confirmed = next;
            }
        }
        Ok(self.confirmed)
    }

    pub fn confirmed(&self) -> u64 {
        self.confirmed
    }
}

/// D and O accept *prefix* evidence from the future manifest/owner pipeline.
/// These bounds alone do not establish that a disk operation completed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Watermarks {
    pub e: u64,
    pub d: u64,
    pub o: u64,
}

impl Watermarks {
    pub fn advance(&mut self, next: Self) -> Result<(), Error> {
        if next.o > next.d
            || next.d > next.e
            || next.e < self.e
            || next.d < self.d
            || next.o < self.o
        {
            return Err(Error::InvalidWatermark);
        }
        *self = next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmations_never_skip_a_hole() {
        // Exhaust every completion order for four appends. The max-based
        // negative control disagrees whenever an acknowledgment skips a hole.
        let mut negative_control_detected = false;
        for a in 1..=4 {
            for b in 1..=4 {
                for c in 1..=4 {
                    for d in 1..=4 {
                        let order = [a, b, c, d];
                        if order.iter().copied().collect::<BTreeSet<_>>().len() != 4 {
                            continue;
                        }
                        let mut prefix = DurablePrefix::default();
                        for _ in 0..4 {
                            prefix.issue().unwrap();
                        }
                        let mut seen = BTreeSet::new();
                        for seq in order {
                            seen.insert(seq);
                            let expected = (1..=4).take_while(|s| seen.contains(s)).count() as u64;
                            assert_eq!(prefix.confirm(seq).unwrap(), expected);
                            assert_eq!(prefix.confirm(seq).unwrap(), expected);
                            negative_control_detected |= *seen.last().unwrap() != expected;
                        }
                    }
                }
            }
        }
        assert!(negative_control_detected);
    }

    #[test]
    fn rejects_unissued_confirmations_without_changing_state() {
        let mut prefix = DurablePrefix::default();
        assert_eq!(prefix.confirm(0), Err(Error::Unissued(0)));
        assert_eq!(prefix.confirm(1), Err(Error::Unissued(1)));
        assert_eq!(prefix.confirmed(), 0);
    }

    #[test]
    fn owner_and_manifest_prefixes_cannot_overtake_durability() {
        let mut marks = Watermarks::default();
        let valid = Watermarks { e: 9, d: 7, o: 3 };
        marks.advance(valid).unwrap();
        for invalid in [
            Watermarks { e: 8, ..valid },
            Watermarks { d: 6, ..valid },
            Watermarks { o: 2, ..valid },
            Watermarks { d: 10, ..valid },
            Watermarks { o: 8, ..valid },
        ] {
            assert_eq!(marks.advance(invalid), Err(Error::InvalidWatermark));
            assert_eq!(marks, valid);
        }
    }
}
