//! Preallocated reusable nodes for the pinned standard-library BTreeMap port.
use crate::budget::{Budget, BudgetAllocator};
use allocator_api2::{boxed::Box, vec::Vec};
use arena_btreemap::{AllocError, Allocator};
use std::{
    alloc::Layout,
    cell::UnsafeCell,
    io,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::{Arc, Mutex},
};

const BYTES: usize = 1024;
const ALIGN: usize = 64;
const EMPTY: usize = usize::MAX;

#[repr(align(64))]
struct Slot(UnsafeCell<MaybeUninit<[u8; BYTES]>>);

const _: () = assert!(size_of::<Slot>() == BYTES && align_of::<Slot>() == ALIGN);

// Raw node padding and unused fields may be uninitialized. The backing type
// must permit that even after a node is returned to the free list.
// SAFETY: only the allocator exposes slot storage. Free slots are accessed
// under the pool mutex; an allocated slot has one owner until deallocation.
unsafe impl Sync for Slot {}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Usage {
    pub current: usize,
    pub peak: usize,
}
struct State {
    free: usize,
    usage: Usage,
}
struct Pool {
    slots: Box<[Slot], BudgetAllocator>,
    state: Mutex<State>,
}

#[derive(Clone)]
pub(super) struct TreeNodes(Arc<Pool>);

impl TreeNodes {
    /// B=6: every non-root node has at least five keys. Reserve an extra node
    /// per possible tree level for transient split propagation. Keys are u64;
    /// values must be at most 64 bytes with alignment at most 8.
    pub(super) fn new(entries: usize, metadata: Arc<Budget>) -> io::Result<Self> {
        let count = entries
            .checked_add(2)
            .map(|n| n.div_ceil(5))
            .and_then(|n| n.checked_add(usize::BITS as usize + 2))
            .ok_or_else(|| io::Error::other("tree node capacity overflow"))?;
        let mut slots = Vec::new_in(BudgetAllocator::new(metadata));
        slots.try_reserve_exact(count).map_err(|_| {
            io::Error::new(io::ErrorKind::OutOfMemory, "tree node metadata exhausted")
        })?;
        slots.resize_with(count, || Slot(UnsafeCell::new(MaybeUninit::uninit())));
        for (index, slot) in slots.iter().enumerate() {
            let next = if index + 1 == count { EMPTY } else { index + 1 };
            // SAFETY: these new, aligned slots are not exposed to a map yet.
            unsafe { slot.0.get().cast::<usize>().write(next) };
        }
        Ok(Self(Arc::new(Pool {
            slots: slots.into_boxed_slice(),
            state: Mutex::new(State {
                free: 0,
                usage: Usage::default(),
            }),
        })))
    }
    pub(super) fn allocated_bytes(&self) -> usize {
        self.0.slots.len() * BYTES
    }
    pub(super) fn usage(&self) -> Usage {
        self.0.state.lock().expect("tree pool poisoned").usage
    }
}

// SAFETY: each successful allocation removes one unique aligned slot; clones
// share the free list and can deallocate one another's slots. The backing array
// never moves and remains owned until all allocator clones are dropped. Every
// supported layout fits one slot. No new backing allocation occurs here.
unsafe impl Allocator for TreeNodes {
    fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
        if layout.size() == 0 || layout.size() > BYTES || layout.align() > ALIGN {
            return Err(AllocError);
        }
        let mut state = self.0.state.lock().expect("tree pool poisoned");
        if state.free == EMPTY {
            return Err(AllocError);
        }
        let pointer = self.0.slots[state.free].0.get().cast::<u8>();
        // SAFETY: the free-list head identifies an initialized, unowned slot.
        state.free = unsafe { pointer.cast::<usize>().read() };
        state.usage.current += 1;
        state.usage.peak = state.usage.peak.max(state.usage.current);
        // SAFETY: a live boxed slot never has a null address.
        Ok(NonNull::slice_from_raw_parts(
            unsafe { NonNull::new_unchecked(pointer) },
            layout.size(),
        ))
    }
    unsafe fn deallocate(&self, pointer: NonNull<u8>, _layout: Layout) {
        let base = self.0.slots[0].0.get().cast::<u8>().addr();
        let offset = pointer.as_ptr().addr() - base;
        assert!(offset.is_multiple_of(BYTES) && offset / BYTES < self.0.slots.len());
        let mut state = self.0.state.lock().expect("tree pool poisoned");
        // SAFETY: the caller returned an owned slot from this pool with no
        // remaining accesses. It is now safe to reuse its storage as a link.
        unsafe { pointer.as_ptr().cast::<usize>().write(state.free) };
        state.free = offset / BYTES;
        state.usage.current -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::Amount;
    use std::collections::BTreeSet;

    fn budget(bytes: usize) -> Arc<Budget> {
        Budget::new(Amount { bytes, requests: 0 })
    }

    #[derive(Clone)]
    struct Observe {
        nodes: TreeNodes,
        layouts: Arc<Mutex<BTreeSet<(usize, usize)>>>,
    }

    // SAFETY: ownership, layouts and pointers pass unchanged to TreeNodes.
    unsafe impl Allocator for Observe {
        fn allocate(&self, layout: Layout) -> Result<NonNull<[u8]>, AllocError> {
            self.layouts
                .lock()
                .unwrap()
                .insert((layout.size(), layout.align()));
            self.nodes.allocate(layout)
        }
        unsafe fn deallocate(&self, pointer: NonNull<u8>, layout: Layout) {
            // SAFETY: this adapter returns only pointers from the same pool.
            unsafe { self.nodes.deallocate(pointer, layout) };
        }
    }

    #[test]
    fn concrete_map_layouts_and_fragmentation_fit_the_fixed_backing() {
        let metadata = budget(32 * 1024 * 1024);
        let pool = TreeNodes::new(65536, Arc::clone(&metadata)).unwrap();
        let charged = pool.allocated_bytes();
        let layouts = Arc::new(Mutex::new(BTreeSet::new()));
        let mut map = arena_btreemap::BTreeMap::<u64, super::super::Mapping, _>::new_in(Observe {
            nodes: pool.clone(),
            layouts: Arc::clone(&layouts),
        });
        let value = |key| super::super::Mapping {
            end: key + 1,
            sequence: key + 1,
            source: None,
        };
        for key in 0..65536 {
            map.insert(key, value(key));
        }
        for key in (0..65536).step_by(2) {
            map.remove(&key);
        }
        for key in (0..65536).step_by(2) {
            map.insert(key, value(key));
        }
        let mut seed = 123u64;
        for _ in 0..262144 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let key = (seed >> 32) % 65536;
            map.remove(&key);
            map.insert(key, value(key));
        }
        assert_eq!(map.range(..).count(), 65536);
        for (key, entry) in &map {
            assert_eq!(entry.sequence, key + 1);
        }
        assert_eq!(
            *layouts.lock().unwrap(),
            BTreeSet::from([(808, 8), (904, 8)])
        );
        assert_eq!(metadata.usage().current.bytes, charged);
        assert_eq!(metadata.usage().peak.bytes, charged);
        for key in (0..65536).rev() {
            map.remove(&key);
        }
        drop(map);
        assert_eq!(pool.usage().current, 0);
        assert!(pool.usage().peak * BYTES <= charged);
        drop(pool);
        assert_eq!(metadata.usage().current.bytes, 0);
    }

    #[test]
    fn denial_and_exhaustion_keep_backing_and_slots_consistent() {
        let denied = budget(BYTES);
        assert!(TreeNodes::new(65536, Arc::clone(&denied)).is_err());
        assert_eq!(denied.usage().current.bytes, 0);
        assert!(TreeNodes::new(usize::MAX, Arc::clone(&denied)).is_err());
        let metadata = budget(1024 * 1024);
        let pool = TreeNodes::new(1, Arc::clone(&metadata)).unwrap();
        for layout in [
            Layout::from_size_align(1025, 8).unwrap(),
            Layout::from_size_align(8, 128).unwrap(),
        ] {
            assert!(pool.allocate(layout).is_err());
        }
        let layout = Layout::from_size_align(BYTES, ALIGN).unwrap();
        let mut allocated = std::vec::Vec::new();
        while let Ok(pointer) = pool.allocate(layout) {
            allocated.push(pointer.cast::<u8>());
        }
        assert_eq!(allocated.len() * BYTES, pool.allocated_bytes());
        for pointer in allocated {
            assert!(pointer.as_ptr().addr().is_multiple_of(ALIGN));
            // SAFETY: each unique allocation came from this pool with `layout`.
            unsafe { pool.clone().deallocate(pointer, layout) };
        }
        assert_eq!(pool.usage().current, 0);
        drop(pool);
        assert_eq!(metadata.usage().current.bytes, 0);
    }

    #[test]
    fn cloned_allocators_share_unique_slots_across_threads() {
        let metadata = budget(1024 * 1024);
        let pool = TreeNodes::new(256, Arc::clone(&metadata)).unwrap();
        std::thread::scope(|scope| {
            for value in 1..=8u8 {
                let pool = pool.clone();
                scope.spawn(move || {
                    for _ in 0..1000 {
                        let bytes =
                            arena_btreemap::alloc::Box::new_in([value; BYTES], pool.clone());
                        std::thread::yield_now();
                        assert!(bytes.iter().all(|byte| *byte == value));
                    }
                });
            }
        });
        assert_eq!(pool.usage().current, 0);
        drop(pool);
        assert_eq!(metadata.usage().current.bytes, 0);
    }
}
