//! Shared allocation credits survive actual final deallocation. The pinned Arc
//! layout is checked by tests/shared_allocation.rs; see docs/shared-allocation.md.
use super::{Amount, Budget, Lease};
use std::{
    alloc::Layout,
    io,
    ops::Deref,
    sync::{Arc, atomic::AtomicUsize},
};

pub struct BudgetArc<T>(Option<Arc<Charged<T>>>);

struct Charged<T> {
    value: T,
    // Field destruction order keeps nested allocations charged through T::drop.
    _lease: Lease,
}

impl<T> BudgetArc<T> {
    pub fn try_new(value: T, budget: &Arc<Budget>) -> io::Result<Self> {
        let (layout, _) = Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<Charged<T>>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let lease = budget
            .reserve(Amount {
                bytes: layout.pad_to_align().size(),
                requests: 0,
            })
            .ok_or(io::ErrorKind::OutOfMemory)?;
        Ok(Self(Some(Arc::new(Charged {
            value,
            _lease: lease,
        }))))
    }

    fn arc(&self) -> &Arc<Charged<T>> {
        self.0.as_ref().expect("shared owner is present until Drop")
    }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.arc(), other.arc())
    }

    pub fn strong_count(&self) -> usize {
        Arc::strong_count(self.arc())
    }
}

impl<T> Deref for BudgetArc<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.arc().value
    }
}

impl<T> Clone for BudgetArc<T> {
    fn clone(&self) -> Self {
        Self(Some(Arc::clone(self.arc())))
    }
}

impl<T> Drop for BudgetArc<T> {
    fn drop(&mut self) {
        if let Some(owner) = self.0.take() {
            // Every clone follows this path and no Weak can escape. Exactly one
            // caller obtains Charged after the Arc allocation has been freed.
            drop(Arc::into_inner(owner));
        }
    }
}
