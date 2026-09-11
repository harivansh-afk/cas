//! Charge the real collection layout, including spare capacity and growth overlap.
use allocator_api2::alloc::{AllocError, Allocator, Global, Layout};
use std::{ptr::NonNull, sync::Arc};

use super::{Amount, Budget};

#[derive(Clone, Debug)]
pub struct BudgetAllocator(Arc<Budget>);

impl BudgetAllocator {
    pub fn new(budget: Arc<Budget>) -> Self {
        Self(budget)
    }
}

// SAFETY: allocations and deallocations delegate to Global with identical
// layouts. Clones share the same budget and can free one another's allocations.
// The default grow/shrink methods allocate before freeing, charging both owners.
unsafe impl Allocator for BudgetAllocator {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        let amount = Amount {
            bytes: layout.size(),
            requests: 0,
        };
        if !self.0.claim(amount) {
            return Err(AllocError);
        }
        Global
            .allocate(layout)
            .inspect_err(|_| self.0.release(amount))
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller supplies a live allocation and the original layout;
        // allocate delegates to this same Global allocator.
        unsafe { Global.deallocate(pointer, layout) };
        self.0.release(Amount {
            bytes: layout.size(),
            requests: 0,
        });
    }
}
