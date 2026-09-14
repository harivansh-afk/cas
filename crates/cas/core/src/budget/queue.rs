//! A fixed ring whose empty and occupied slots retain their allocation credit.
use super::{Budget, BudgetAllocator};
use allocator_api2::vec::Vec;
use std::{io, sync::Arc};

pub struct Queue<T> {
    slots: Vec<Option<T>, BudgetAllocator>,
    head: usize,
    len: usize,
}

impl<T> Queue<T> {
    pub fn with_capacity(capacity: usize, budget: &Arc<Budget>) -> io::Result<Self> {
        if capacity == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut slots = Vec::new_in(BudgetAllocator::new(Arc::clone(budget)));
        slots
            .try_reserve_exact(capacity)
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        slots.resize_with(capacity, || None);
        Ok(Self {
            slots,
            head: 0,
            len: 0,
        })
    }

    pub fn len(&self) -> usize {
        self.len
    }
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn is_full(&self) -> bool {
        self.len == self.capacity()
    }

    pub fn try_push_back(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        let tail = (self.head + self.len) % self.capacity();
        self.slots[tail] = Some(value);
        self.len += 1;
        Ok(())
    }

    pub fn try_push_front(&mut self, value: T) -> Result<(), T> {
        if self.is_full() {
            return Err(value);
        }
        self.head = (self.head + self.capacity() - 1) % self.capacity();
        self.slots[self.head] = Some(value);
        self.len += 1;
        Ok(())
    }

    /// For callers whose request credits prove space; overflow is a bug.
    #[track_caller]
    pub fn push_back(&mut self, value: T) {
        assert!(
            self.try_push_back(value).is_ok(),
            "fixed queue capacity exceeded"
        );
    }

    /// For reinserting an owner removed from this bounded queue.
    #[track_caller]
    pub fn push_front(&mut self, value: T) {
        assert!(
            self.try_push_front(value).is_ok(),
            "fixed queue capacity exceeded"
        );
    }

    pub fn pop_front(&mut self) -> Option<T> {
        if self.is_empty() {
            return None;
        }
        let value = self.slots[self.head].take();
        self.head = (self.head + 1) % self.capacity();
        self.len -= 1;
        value
    }

    pub fn front(&self) -> Option<&T> {
        if self.is_empty() {
            None
        } else {
            self.slots[self.head].as_ref()
        }
    }

    pub fn front_mut(&mut self) -> Option<&mut T> {
        if self.is_empty() {
            None
        } else {
            self.slots[self.head].as_mut()
        }
    }

    pub fn clear(&mut self) {
        while let Some(value) = self.pop_front() {
            drop(value);
        }
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut T> {
        let (before, after) = self.slots.split_at_mut(self.head);
        after
            .iter_mut()
            .chain(before.iter_mut())
            .take(self.len)
            .map(|slot| slot.as_mut().expect("occupied queue slot"))
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &T> {
        (0..self.len).map(|index| {
            self.slots[(self.head + index) % self.capacity()]
                .as_ref()
                .expect("occupied queue slot")
        })
    }
}

#[cfg(test)]
mod tests;
