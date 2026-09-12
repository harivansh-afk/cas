use super::*;
use crate::budget::Amount;
use std::{
    collections::VecDeque,
    sync::atomic::{AtomicUsize, Ordering},
};

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

#[test]
fn wraparound_and_both_ends_match_reference_without_growth() {
    let budget = budget(4096);
    let mut queue = Queue::with_capacity(7, &budget).unwrap();
    let allocated = budget.usage().current;
    let mut reference = VecDeque::new();
    let mut random = 0x434153u64;
    for value in 0..20_000 {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        match random % 4 {
            0 | 1 => {
                let front = random.is_multiple_of(4);
                let result = if front {
                    queue.try_push_front(value)
                } else {
                    queue.try_push_back(value)
                };
                if reference.len() == 7 {
                    assert_eq!(result, Err(value));
                } else {
                    assert_eq!(result, Ok(()));
                    if front {
                        reference.push_front(value);
                    } else {
                        reference.push_back(value);
                    }
                }
            }
            2 => assert_eq!(queue.pop_front(), reference.pop_front()),
            _ => {
                if let Some(front) = queue.front_mut() {
                    *front += 1;
                }
                if let Some(front) = reference.front_mut() {
                    *front += 1;
                }
            }
        }
        assert!(queue.iter().eq(reference.iter()));
        assert_eq!(queue.front(), reference.front());
        assert_eq!(queue.len(), reference.len());
        assert_eq!(queue.is_full(), reference.len() == 7);
        assert_eq!(budget.usage().current, allocated);
    }
    queue.clear();
    assert!(queue.is_empty());
    assert_eq!(queue.front(), None);
    assert_eq!(queue.pop_front(), None);
    assert_eq!(budget.usage().peak, allocated);
    assert_eq!(budget.usage().current, allocated);
    drop(queue);
    assert_eq!(budget.usage().current, Amount::default());
}

struct Owner(Arc<AtomicUsize>);
impl Drop for Owner {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn full_queue_returns_ownership_and_clear_retains_empty_slot_allocation() {
    let budget = budget(4096);
    let mut queue = Queue::with_capacity(2, &budget).unwrap();
    let allocated = budget.usage().current;
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(queue.try_push_back(Owner(Arc::clone(&drops))).is_ok());
    assert!(queue.try_push_front(Owner(Arc::clone(&drops))).is_ok());
    let rejected = queue
        .try_push_front(Owner(Arc::clone(&drops)))
        .err()
        .unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    let removed = queue.pop_front().unwrap();
    queue.clear();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_eq!(budget.usage().current, allocated);
    drop((rejected, removed, queue));
    assert_eq!(drops.load(Ordering::Relaxed), 3);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn invalid_or_unfunded_storage_does_not_allocate() {
    let budget = budget(8 * size_of::<Option<u64>>() - 1);
    for capacity in [0, 8, usize::MAX] {
        assert!(Queue::<u64>::with_capacity(capacity, &budget).is_err());
        assert_eq!(budget.usage().current, Amount::default());
        assert_eq!(budget.usage().peak, Amount::default());
    }
}
