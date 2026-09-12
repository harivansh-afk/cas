//! Fixed IO slots; full token comparison rejects CQEs from a retired generation.
use super::*;

pub(super) struct Slots<T> {
    entries: BudgetVec<Option<(u64, T)>, BudgetAllocator>,
    len: usize,
}

impl<T> Slots<T> {
    pub fn new(capacity: usize, metadata: &Arc<Budget>) -> io::Result<Self> {
        if capacity == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut entries = reserved_vec(capacity, metadata)?;
        entries.resize_with(capacity, || None);
        Ok(Self { entries, len: 0 })
    }

    fn index(&self, token: u64) -> usize {
        (token % self.entries.len() as u64) as usize
    }

    pub fn next(&self, from: u64) -> Option<u64> {
        for offset in 0..self.entries.len() {
            let token = from.checked_add(offset as u64)?;
            if self.entries[self.index(token)].is_none() {
                return Some(token);
            }
        }
        None
    }

    pub fn insert(&mut self, token: u64, value: T) {
        let index = self.index(token);
        assert!(
            self.entries[index].is_none(),
            "IO token slot is still occupied"
        );
        self.entries[index] = Some((token, value));
        self.len += 1;
    }

    pub fn get(&self, token: &u64) -> Option<&T> {
        self.entries[self.index(*token)]
            .as_ref()
            .filter(|(key, _)| key == token)
            .map(|(_, value)| value)
    }

    pub fn get_mut(&mut self, token: &u64) -> Option<&mut T> {
        let index = self.index(*token);
        self.entries[index]
            .as_mut()
            .filter(|(key, _)| key == token)
            .map(|(_, value)| value)
    }

    pub fn remove(&mut self, token: &u64) -> Option<T> {
        self.get(token)?;
        let index = self.index(*token);
        let (_, value) = self.entries[index].take().unwrap();
        self.len -= 1;
        Some(value)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = (&u64, &T)> {
        self.entries
            .iter()
            .flatten()
            .map(|(token, value)| (token, value))
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.iter().map(|(_, value)| value)
    }

    /// Remove an owned entry without dropping it before its caller can respond.
    pub fn remove_first(&mut self, mut matches: impl FnMut(&T) -> bool) -> Option<T> {
        let token = self
            .iter()
            .find_map(|(token, value)| matches(value).then_some(*token))?;
        self.remove(&token)
    }

    /// Keep uncertain kernel owners alive; their buffers and files own credits.
    pub fn leak(&mut self) {
        for entry in &mut self.entries {
            if let Some((_, value)) = entry.take() {
                std::mem::forget(value);
            }
        }
        self.len = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn churn_skips_live_slots_and_stale_tokens_never_alias_reuse() {
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let mut slots = Slots::new(7, &metadata).unwrap();
        let allocated = metadata.usage().current;
        for token in 1..=7 {
            slots.insert(token, token);
        }
        assert_eq!(slots.next(8), None);
        assert_eq!(slots.remove(&3), Some(3));
        let mut previous = 3;
        let mut next = 8;
        for _ in 0..10_000 {
            let token = slots.next(next).unwrap();
            slots.insert(token, token);
            assert_eq!(slots.get(&previous), None);
            assert_eq!(slots.get_mut(&previous), None);
            assert_eq!(slots.remove(&previous), None);
            assert_eq!(slots.get(&1), Some(&1));
            assert_eq!(slots.remove(&token), Some(token));
            next = token + 1;
            previous = token;
            assert_eq!(metadata.usage().current, allocated);
        }
        while slots.remove_first(|value| *value != 1).is_some() {}
        assert_eq!(slots.values().copied().collect::<Vec<_>>(), [1]);
        assert_eq!(slots.remove(&1), Some(1));
        assert!(slots.is_empty());
        assert_eq!(metadata.usage().peak, allocated);
        drop(slots);
        assert_eq!(metadata.usage().current, Amount::default());
    }

    #[test]
    fn token_limit_and_metadata_denial_do_not_replace_live_owners() {
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let mut slots = Slots::new(1, &metadata).unwrap();
        slots.insert(u64::MAX, 7);
        assert_eq!(slots.next(u64::MAX), None);
        assert_eq!(slots.get(&u64::MAX), Some(&7));
        assert!(Slots::<u64>::new(0, &metadata).is_err());
        let denied = Budget::new(Amount::default());
        assert!(Slots::<u64>::new(1, &denied).is_err());
        assert_eq!(denied.usage().current, Amount::default());
    }
}
