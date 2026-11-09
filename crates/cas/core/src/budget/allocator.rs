//! Charge the real collection layout, including spare capacity and growth overlap.
use allocator_api2::alloc::{AllocError, Allocator, Global, Layout, System};
use std::{ptr::NonNull, sync::Arc};

use super::{Amount, Budget};

#[derive(Clone, Debug)]
pub struct BudgetAllocator(Arc<Budget>, bool, Option<Arc<Budget>>);

impl BudgetAllocator {
    pub fn new(budget: Arc<Budget>) -> Self {
        Self(budget, false, None)
    }
    /// Linux System-backed allocations charge libc usable bytes, including slack.
    pub fn tracked(budget: Arc<Budget>) -> Self {
        Self(budget, true, None)
    }
    pub fn accounted(budget: Arc<Budget>, account: Arc<Budget>) -> Self {
        Self(budget, false, Some(account))
    }
    pub fn charged_bytes(&self) -> usize {
        self.0.usage().current.bytes
    }
}

/// CAS uses the System global allocator. This is also exercised under the
/// delegating allocation recorder. Non-glibc targets report layout bytes only.
pub(super) unsafe fn usable(pointer: *mut u8, layout: Layout) -> usize {
    if layout.size() == 0 {
        return 0;
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: caller supplies the base of a live System/libc allocation.
        unsafe { libc::malloc_usable_size(pointer.cast()) }.max(layout.size())
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        let _ = pointer;
        layout.size()
    }
}

// SAFETY: allocations and deallocations delegate to the same Global/System with identical
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
        let allocator: &dyn Allocator = if self.1 || self.2.is_some() {
            &System
        } else {
            &Global
        };
        let pointer = allocator
            .allocate(layout)
            .inspect_err(|_| self.0.release(amount))?;
        if self.1 || self.2.is_some() {
            // SAFETY: tracked/accounted allocations explicitly use System.
            let bytes = unsafe { usable(pointer.cast().as_ptr(), layout) };
            if let Some(account) = &self.2
                && !account.claim(Amount { bytes, requests: 0 })
            {
                // SAFETY: this newly allocated block has not escaped.
                unsafe { allocator.deallocate(pointer.cast(), layout) };
                self.0.release(amount);
                return Err(AllocError);
            }
            let slack = Amount {
                bytes: if self.1 { bytes - layout.size() } else { 0 },
                requests: 0,
            };
            if self.1 && !self.0.claim(slack) {
                // SAFETY: this newly allocated block has not escaped.
                unsafe { allocator.deallocate(pointer.cast(), layout) };
                self.0.release(amount);
                return Err(AllocError);
            }
        }
        Ok(pointer)
    }

    unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
        // SAFETY: the caller supplies a live allocation and the original layout;
        // allocate delegates to this same selected allocator.
        let actual = if self.1 || self.2.is_some() {
            // SAFETY: the allocator contract supplies a live allocation base.
            unsafe { usable(pointer.as_ptr(), layout) }
        } else {
            layout.size()
        };
        let allocator: &dyn Allocator = if self.1 || self.2.is_some() {
            &System
        } else {
            &Global
        };
        // SAFETY: live pointer and original layout belong to this allocator.
        unsafe { allocator.deallocate(pointer, layout) };
        if self.1 {
            self.0.release(Amount {
                bytes: actual - layout.size(),
                requests: 0,
            });
        }
        if let Some(account) = &self.2 {
            account.release(Amount {
                bytes: actual,
                requests: 0,
            });
        }
        self.0.release(Amount {
            bytes: layout.size(),
            requests: 0,
        });
    }
}

#[cfg(all(test, target_os = "linux", target_env = "gnu"))]
mod tests {
    use super::*;

    #[test]
    fn usable_size_and_denied_slack_release_parent_and_component() {
        let layout = Layout::from_size_align(1, 8).unwrap();
        let parent = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let component = Budget::child(&parent);
        let allocator = BudgetAllocator::tracked(Arc::clone(&component));
        let pointer = allocator.allocate(layout).unwrap();
        // SAFETY: tracked allocations use System and this pointer is live.
        let actual = unsafe { libc::malloc_usable_size(pointer.cast().as_ptr()) };
        assert!(actual > layout.size());
        assert_eq!(component.usage().current.bytes, actual);
        assert_eq!(parent.usage().current.bytes, actual);
        // SAFETY: allocation above used this layout and has not been freed.
        unsafe { allocator.deallocate(pointer.cast(), layout) };
        assert_eq!(parent.usage().current.bytes, 0);
        assert_eq!(component.usage().current.bytes, 0);

        let denied = Budget::new(Amount {
            bytes: layout.size(),
            requests: 0,
        });
        let child = Budget::child(&denied);
        assert!(
            BudgetAllocator::tracked(Arc::clone(&child))
                .allocate(layout)
                .is_err()
        );
        assert_eq!(denied.usage().current.bytes, 0);
        assert_eq!(child.usage().current.bytes, 0);
        assert_eq!(denied.usage().rejected, 1);
    }
}
