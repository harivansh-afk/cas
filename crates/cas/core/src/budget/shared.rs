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
        Self::create(value, budget, false)
    }

    /// Pinned System/Arc layout, verified by the shared-allocation test.
    pub fn try_new_tracked(value: T, budget: &Arc<Budget>) -> io::Result<Self> {
        Self::create(value, budget, true)
    }

    fn create(value: T, budget: &Arc<Budget>, tracked: bool) -> io::Result<Self> {
        let (layout, offset) = Layout::new::<[AtomicUsize; 2]>()
            .extend(Layout::new::<Charged<T>>())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let lease = budget
            .reserve(Amount {
                bytes: layout.pad_to_align().size(),
                requests: 0,
            })
            .ok_or(io::ErrorKind::OutOfMemory)?;
        let mut owner = Arc::new(Charged {
            value,
            _lease: lease,
        });
        if tracked {
            // SAFETY: the pinned std Arc allocation puts its two counters before
            // Charged<T> with this computed alignment. CAS uses System globally.
            let base = unsafe { Arc::as_ptr(&owner).cast::<u8>().sub(offset).cast_mut() };
            // SAFETY: base is the live System allocation base described above.
            let bytes = unsafe { super::allocator::usable(base, layout.pad_to_align()) };
            let slack = Amount {
                bytes: bytes - layout.pad_to_align().size(),
                requests: 0,
            };
            if !budget.claim_extra(slack) {
                // Follow the same deallocation-before-credit-release ordering
                // as every published BudgetArc, including this rejected owner.
                drop(Self(Some(owner)));
                return Err(io::ErrorKind::OutOfMemory.into());
            }
            Arc::get_mut(&mut owner)
                .expect("private owner")
                ._lease
                .amount
                .bytes += slack.bytes;
        }
        Ok(Self(Some(owner)))
    }

    fn arc(&self) -> &Arc<Charged<T>> {
        self.0.as_ref().expect("shared owner is present until Drop")
    }

    pub fn get_mut(&mut self) -> Option<&mut T> {
        Arc::get_mut(self.0.as_mut()?).map(|owner| &mut owner.value)
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
