//! Required pinned-toolchain check of the actual shared allocation and lifetime.
//! The allocator recorder does not allocate, and one test owns its global state.
use cas_core::budget::{Amount, Budget, BudgetArc};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    panic::{AssertUnwindSafe, catch_unwind},
    ptr,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering::SeqCst},
    },
    thread,
};

thread_local! {
    static WATCH: Cell<bool> = const { Cell::new(false) };
}

static ACCOUNT: AtomicPtr<Budget> = AtomicPtr::new(ptr::null_mut());
static ALLOCATION: AtomicPtr<u8> = AtomicPtr::new(ptr::null_mut());
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static SIZE: AtomicUsize = AtomicUsize::new(0);
static ALIGNMENT: AtomicUsize = AtomicUsize::new(0);
static BEFORE_ALLOCATION: AtomicUsize = AtomicUsize::new(0);
static BEFORE_DEALLOCATION: AtomicUsize = AtomicUsize::new(0);
static DEALLOCATED: AtomicBool = AtomicBool::new(false);
static DROPS: AtomicUsize = AtomicUsize::new(0);

struct Recorder;

#[global_allocator]
static ALLOCATOR: Recorder = Recorder;

fn charged() -> usize {
    let account = ACCOUNT.load(SeqCst);
    // SAFETY: the single test installs a live Budget before enabling WATCH and
    // retains its Arc until all recorded allocations and dropping threads end.
    unsafe { &*account }.usage().current.bytes
}

// SAFETY: each operation delegates to System with the original pointer/layout.
// Recording uses atomics and an already initialized thread-local Cell only.
unsafe impl GlobalAlloc for Recorder {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let watch = WATCH.try_with(Cell::get).unwrap_or(false);
        if watch {
            BEFORE_ALLOCATION.store(charged(), SeqCst);
        }
        // SAFETY: the caller supplies the valid requested allocation layout.
        let pointer = unsafe { System.alloc(layout) };
        if watch {
            ALLOCATIONS.fetch_add(1, SeqCst);
            SIZE.store(layout.size(), SeqCst);
            ALIGNMENT.store(layout.align(), SeqCst);
            ALLOCATION.store(pointer, SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let recorded = ALLOCATION
            .compare_exchange(pointer, ptr::null_mut(), SeqCst, SeqCst)
            .is_ok();
        if recorded {
            BEFORE_DEALLOCATION.store(charged(), SeqCst);
        }
        // SAFETY: the caller supplies the live System allocation and its layout.
        unsafe { System.dealloc(pointer, layout) };
        if recorded {
            DEALLOCATED.store(true, SeqCst);
        }
    }
}

struct Probe;

impl Drop for Probe {
    fn drop(&mut self) {
        assert!(
            DEALLOCATED.load(SeqCst),
            "value dropped before control deallocation"
        );
        assert_eq!(charged(), SIZE.load(SeqCst), "lease dropped before value");
        DROPS.fetch_add(1, SeqCst);
    }
}

struct Payload<const N: usize> {
    _bytes: [u8; N],
    _probe: Probe,
}

#[repr(align(64))]
struct Aligned(Payload<1>);

struct Panicking(Probe);

impl Drop for Panicking {
    fn drop(&mut self) {
        assert!(DEALLOCATED.load(SeqCst));
        assert_eq!(charged(), SIZE.load(SeqCst));
        panic!("intentional value destructor failure");
    }
}

fn reset(account: &Arc<Budget>) {
    assert!(ALLOCATION.load(SeqCst).is_null());
    assert!(!WATCH.get());
    ACCOUNT.store(Arc::as_ptr(account).cast_mut(), SeqCst);
    ALLOCATIONS.store(0, SeqCst);
    SIZE.store(0, SeqCst);
    ALIGNMENT.store(0, SeqCst);
    BEFORE_ALLOCATION.store(0, SeqCst);
    BEFORE_DEALLOCATION.store(0, SeqCst);
    DEALLOCATED.store(false, SeqCst);
    DROPS.store(0, SeqCst);
}

fn create<T>(value: T, account: &Arc<Budget>) -> BudgetArc<T> {
    WATCH.set(true);
    let owner = BudgetArc::try_new(value, account);
    WATCH.set(false);
    let owner = owner.unwrap();
    assert_eq!(ALLOCATIONS.load(SeqCst), 1);
    assert_eq!(BEFORE_ALLOCATION.load(SeqCst), SIZE.load(SeqCst));
    assert_eq!(account.usage().current.bytes, SIZE.load(SeqCst));
    assert!(ALIGNMENT.load(SeqCst) >= align_of::<T>());
    owner
}

fn completed(account: &Arc<Budget>) {
    assert!(DEALLOCATED.load(SeqCst));
    assert_eq!(BEFORE_DEALLOCATION.load(SeqCst), SIZE.load(SeqCst));
    assert_eq!(DROPS.load(SeqCst), 1);
    assert_eq!(account.usage().current.bytes, 0);
    assert_eq!(account.usage().admitted, account.usage().released);
    assert!(ALLOCATION.load(SeqCst).is_null());
}

fn concurrent<T: Send + Sync + 'static>(value: T, count: usize) {
    let account = Budget::new(Amount {
        bytes: 4096,
        requests: 0,
    });
    reset(&account);
    let owner = create(value, &account);
    let barrier = Arc::new(Barrier::new(count));
    let mut owners = Vec::new();
    for _ in 1..count {
        owners.push(owner.clone());
        assert!(owner.ptr_eq(owners.last().unwrap()));
    }
    assert_eq!(owner.strong_count(), count);
    owners.push(owner);
    let threads: Vec<_> = owners
        .into_iter()
        .map(|owner| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                drop(owner);
            })
        })
        .collect();
    for worker in threads {
        worker.join().unwrap();
    }
    completed(&account);
}

#[test]
fn actual_layout_credit_order_concurrent_final_drops_and_unwind() {
    for count in [1, 2, 16] {
        concurrent(Probe, count);
        concurrent(
            Payload {
                _bytes: [7; 1],
                _probe: Probe,
            },
            count,
        );
        concurrent(
            Payload {
                _bytes: [7; 128],
                _probe: Probe,
            },
            count,
        );
        let aligned = Aligned(Payload {
            _bytes: [7; 1],
            _probe: Probe,
        });
        assert_eq!(aligned.0._bytes, [7]);
        concurrent(aligned, count);
    }

    let account = Budget::new(Amount {
        bytes: 4096,
        requests: 0,
    });
    reset(&account);
    let owner = create(Panicking(Probe), &account);
    assert!(catch_unwind(AssertUnwindSafe(|| drop(owner))).is_err());
    completed(&account);

    let denied = Budget::new(Amount {
        bytes: 0,
        requests: 0,
    });
    reset(&denied);
    WATCH.set(true);
    let result = BudgetArc::try_new(1u8, &denied);
    WATCH.set(false);
    assert_eq!(
        result.err().unwrap().kind(),
        std::io::ErrorKind::OutOfMemory
    );
    assert_eq!(ALLOCATIONS.load(SeqCst), 0);
    assert_eq!(denied.usage().current.bytes, 0);
    ACCOUNT.store(ptr::null_mut(), SeqCst);
}
