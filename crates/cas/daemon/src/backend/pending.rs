//! Preallocated completion owners; IDs need not complete in submission order.
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator};
use std::{io, sync::Arc};

pub(super) struct Pending<T> {
    entries: Vec<(u64, T), BudgetAllocator>,
    limit: usize,
}

impl<T> Pending<T> {
    pub fn new(limit: usize, metadata: &Arc<Budget>) -> io::Result<Self> {
        let entries = crate::local::reserved_vec(limit, metadata)?;
        Ok(Self { entries, limit })
    }

    pub fn insert(&mut self, id: u64, value: T) -> io::Result<()> {
        if self.entries.len() == self.limit {
            return Err(io::Error::other("pending request limit exceeded"));
        }
        if self.entries.iter().any(|entry| entry.0 == id) {
            return Err(io::Error::other("duplicate pending request ID"));
        }
        self.entries.push((id, value));
        Ok(())
    }

    #[cfg(test)]
    pub fn get_mut(&mut self, id: &u64) -> Option<&mut T> {
        self.entries
            .iter_mut()
            .find(|entry| entry.0 == *id)
            .map(|entry| &mut entry.1)
    }

    pub fn remove(&mut self, id: &u64) -> Option<T> {
        let index = self.entries.iter().position(|entry| entry.0 == *id)?;
        Some(self.entries.swap_remove(index).1)
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut T> {
        self.entries.iter_mut().map(|entry| &mut entry.1)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cas_core::budget::Amount;

    #[test]
    fn old_owner_survives_reuse_without_allocation_or_replacement() {
        let budget = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let mut pending = Pending::new(2, &budget).unwrap();
        let allocated = budget.usage();
        assert!(allocated.current.bytes >= 2 * size_of::<(u64, u64)>());
        pending.insert(1, 99_u64).unwrap();
        assert!(pending.insert(1, 0).is_err());
        for id in 2..10_000 {
            pending.insert(id, id).unwrap();
            assert!(pending.insert(id + 1, 0).is_err());
            assert_eq!(pending.remove(&id), Some(id));
            assert_eq!(pending.get_mut(&1), Some(&mut 99));
        }
        assert_eq!(budget.usage().admitted, allocated.admitted);
        assert_eq!(budget.usage().current, allocated.current);
        assert_eq!(pending.remove(&1), Some(99));
        assert!(pending.is_empty());
        assert_eq!(budget.usage().current, allocated.current);
        drop(pending);
        assert_eq!(budget.usage().current, Amount::default());
    }

    #[test]
    fn allocation_refusal_precedes_admission() {
        let budget = Budget::new(Amount::default());
        assert!(matches!(
            Pending::<u64>::new(2, &budget),
            Err(error) if error.kind() == io::ErrorKind::OutOfMemory
        ));
        assert_eq!(budget.usage().current, Amount::default());
    }
}
