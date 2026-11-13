//! Raced settled reads. After a budget, the same chunk is requested from a
//! second source: the next rendezvous owner, or the local chunk store when a
//! verified local copy exists. The first BLAKE3-verified reply completes the
//! read; the loser keeps its slot until its transport finishes and never
//! publishes bytes. Nothing here changes admission, ordering or cache fills.
use super::{
    client::{self, SlotRef, Ticket},
    metrics::{self, Metrics, Phases},
    wire::verify,
};
use allocator_api2::vec::Vec;
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator, BudgetArc, Queue},
    scheduler,
    store::file::Read as Plan,
};
use std::{
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use vmm_sys_util::eventfd::EventFd;

/// Local races are bounded independently of GET slots; an exhausted pool
/// simply leaves the primary request unraced.
const LOCAL_SLOTS: usize = 256;

/// Completion state of one source, read without consuming its bytes.
#[derive(Clone, Copy)]
pub(crate) struct Outcome {
    pub done: bool,
    pub error: Option<io::ErrorKind>,
    pub phases: Phases,
}

/// The second source of a race, owned by the first request's slot.
pub enum Challenger {
    Remote(Ticket),
    Local(LocalTicket),
}

impl Challenger {
    pub(crate) fn peek(&self) -> Outcome {
        match self {
            Self::Remote(ticket) => ticket.peek(),
            Self::Local(ticket) => ticket.peek(),
        }
    }
    /// Copy verified bytes; only call after `peek` reported a verified reply.
    pub(crate) fn take(&self, destination: &mut [u8]) -> io::Result<Phases> {
        match self {
            Self::Remote(ticket) => ticket.take(destination),
            Self::Local(ticket) => ticket.take(destination),
        }
    }
    pub(crate) fn winner(&self) -> u8 {
        match self {
            Self::Remote(_) => metrics::WINNER_SECOND,
            Self::Local(_) => metrics::WINNER_LOCAL,
        }
    }
}

/// The second source a race will ask, with the transport handle to ask it.
pub(crate) enum PendingKind {
    /// The next owning member's GET transport.
    Remote(BudgetArc<client::Shared>),
    /// A verified local store plan captured when the primary GET was sent.
    Local(Plan, BudgetArc<LocalShared>),
}
impl PendingKind {
    /// Issue the challenger; it signals `notify` when it finishes.
    pub(crate) fn start(self, hash: [u8; 32], notify: EventFd) -> io::Result<Challenger> {
        match self {
            Self::Remote(shared) => client::Shared::fetch(&shared, hash, Some(notify), false, true)
                .map(Challenger::Remote),
            Self::Local(plan, shared) => {
                LocalShared::fetch(&shared, hash, plan, notify).map(Challenger::Local)
            }
        }
    }
}
/// A race armed on a primary slot but not started yet.
pub(crate) struct PendingRace {
    pub kind: PendingKind,
    pub budget_us: u32,
}

pub(crate) struct Armed {
    pub deadline: Instant,
    pub first: SlotRef,
}

struct RacerState {
    armed: Vec<Armed, BudgetAllocator>,
}
struct RacerShared {
    state: Mutex<RacerState>,
    changed: Condvar,
    stop: AtomicBool,
}

/// Starts challengers when their budgets elapse. Zero budgets never reach
/// this thread; the router starts those inline on the reader's thread, and a
/// primary that fails earlier starts its own challenger from the transport.
pub struct Racer {
    shared: Arc<RacerShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Racer {
    pub(crate) fn new(budget: &Arc<Budget>) -> io::Result<Self> {
        let shared = Arc::new(RacerShared {
            state: Mutex::new(RacerState {
                armed: crate::local::reserved_vec(crate::local::pools::HOST_REQUESTS, budget)?,
            }),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
        });
        let owner = Arc::clone(&shared);
        let worker = thread::Builder::new()
            .name("cas-peer-race".into())
            .spawn(move || run_racer(owner))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(crate) fn arm(&self, armed: Armed) {
        let mut state = self.shared.state.lock().expect("racer poisoned");
        if state.armed.len() == state.armed.capacity() {
            // Bounded: drop this race rather than allocate.
            return;
        }
        state.armed.push(armed);
        self.shared.changed.notify_one();
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        let mut worker = self.worker.lock().expect("racer worker poisoned");
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("peer racer panicked"))?;
        }
        Ok(())
    }
}

impl Drop for Racer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        if let Some(worker) = self.worker.get_mut().expect("racer worker poisoned").take() {
            let _ = worker.join();
        }
    }
}

fn run_racer(shared: Arc<RacerShared>) {
    let mut state = shared.state.lock().expect("racer poisoned");
    loop {
        if shared.stop.load(Ordering::Acquire) {
            state.armed.clear();
            return;
        }
        let now = Instant::now();
        if let Some(index) = state.armed.iter().position(|a| a.deadline <= now) {
            let armed = state.armed.swap_remove(index);
            drop(state);
            armed.first.due();
            state = shared.state.lock().expect("racer poisoned");
            continue;
        }
        state = match state.armed.iter().map(|a| a.deadline).min() {
            Some(deadline) => {
                shared
                    .changed
                    .wait_timeout(state, deadline.saturating_duration_since(now))
                    .expect("racer poisoned")
                    .0
            }
            None => shared.changed.wait(state).expect("racer poisoned"),
        };
    }
}

struct LocalSlot {
    owned: bool,
    done: bool,
    used: bool,
    hash: [u8; 32],
    plan: Option<Plan>,
    bytes: [u8; BLOCK_SIZE],
    phases: Phases,
    error: Option<io::ErrorKind>,
    notify: Option<EventFd>,
}
struct LocalPool {
    slots: Vec<LocalSlot, BudgetAllocator>,
    free: Vec<usize, BudgetAllocator>,
    queued: Queue<usize>,
}
pub(crate) struct LocalShared {
    pool: Mutex<LocalPool>,
    changed: Condvar,
    stop: AtomicBool,
    metrics: BudgetArc<Metrics>,
    scheduling: scheduler::Port,
}

/// The local chunk store as a race source: a blocking, verified O_DIRECT
/// load on one worker, scheduled as a demand source like GET service reads.
/// This is the experiment arm for `--peer-force-remote --peer-race-local`;
/// it is not the reactor's io_uring local read path.
pub struct LocalSource {
    shared: BudgetArc<LocalShared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
pub struct LocalTicket {
    shared: BudgetArc<LocalShared>,
    index: usize,
}

impl LocalSource {
    pub fn new(
        scheduling: scheduler::Port,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
    ) -> io::Result<Self> {
        let mut slots = crate::local::reserved_vec(LOCAL_SLOTS, budget)?;
        let mut free = crate::local::reserved_vec(LOCAL_SLOTS, budget)?;
        for index in 0..LOCAL_SLOTS {
            slots.push(LocalSlot {
                owned: false,
                done: false,
                used: false,
                hash: [0; 32],
                plan: None,
                bytes: [0; BLOCK_SIZE],
                phases: Phases::default(),
                error: None,
                notify: None,
            });
            free.push(index);
        }
        let shared = BudgetArc::try_new(
            LocalShared {
                pool: Mutex::new(LocalPool {
                    slots,
                    free,
                    queued: Queue::with_capacity(LOCAL_SLOTS, budget)?,
                }),
                changed: Condvar::new(),
                stop: AtomicBool::new(false),
                metrics,
                scheduling,
            },
            budget,
        )?;
        let page = AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(budget)))?;
        let payload =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(budget)))?;
        let owner = shared.clone();
        let worker = thread::Builder::new()
            .name("cas-peer-race-local".into())
            .spawn(move || run_local(owner, page, payload))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }

    pub(crate) fn handle(&self) -> BudgetArc<LocalShared> {
        self.shared.clone()
    }

    pub fn shutdown(&self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        let mut worker = self.worker.lock().expect("local race worker poisoned");
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("local race worker panicked"))?;
        }
        Ok(())
    }
}

impl Drop for LocalSource {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.changed.notify_all();
        if let Some(worker) = self
            .worker
            .get_mut()
            .expect("local race worker poisoned")
            .take()
        {
            let _ = worker.join();
        }
    }
}

impl LocalShared {
    pub(crate) fn fetch(
        shared: &BudgetArc<Self>,
        hash: [u8; 32],
        plan: Plan,
        notify: EventFd,
    ) -> io::Result<LocalTicket> {
        if shared.stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut pool = shared.pool.lock().expect("local race pool poisoned");
        let index = pool.free.pop().ok_or(io::ErrorKind::WouldBlock)?;
        let slot = &mut pool.slots[index];
        slot.owned = true;
        slot.done = false;
        slot.used = false;
        slot.hash = hash;
        slot.plan = Some(plan);
        slot.phases = Phases::default();
        slot.error = None;
        slot.notify = Some(notify);
        pool.queued.push_back(index);
        drop(pool);
        shared.changed.notify_one();
        Ok(LocalTicket {
            shared: shared.clone(),
            index,
        })
    }
}

impl LocalTicket {
    pub(crate) fn peek(&self) -> Outcome {
        let pool = self.shared.pool.lock().expect("local race pool poisoned");
        let slot = &pool.slots[self.index];
        Outcome {
            done: slot.done,
            error: slot.error,
            phases: slot.phases,
        }
    }
    pub(crate) fn take(&self, destination: &mut [u8]) -> io::Result<Phases> {
        let mut pool = self.shared.pool.lock().expect("local race pool poisoned");
        let slot = &mut pool.slots[self.index];
        if !slot.done {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(error) = slot.error {
            return Err(error.into());
        }
        if destination.len() != BLOCK_SIZE {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        destination.copy_from_slice(&slot.bytes);
        slot.used = true;
        Ok(slot.phases)
    }
}

impl Drop for LocalTicket {
    fn drop(&mut self) {
        let mut pool = self.shared.pool.lock().expect("local race pool poisoned");
        let slot = &mut pool.slots[self.index];
        slot.owned = false;
        // A loser's late completion must not signal a reused reader slot.
        slot.notify = None;
        if slot.done {
            if !slot.used {
                self.shared.metrics.count(|c| c.race_discarded += 1);
            }
            pool.free.push(self.index);
        }
        // Pending abandoned slots are recycled only after the load completes.
    }
}

fn run_local(
    shared: BudgetArc<LocalShared>,
    mut page: AlignedBuffer<BudgetAllocator>,
    mut payload: AlignedBuffer<BudgetAllocator>,
) {
    loop {
        let (index, hash, plan) = {
            let mut pool = shared.pool.lock().expect("local race pool poisoned");
            loop {
                if shared.stop.load(Ordering::Acquire) {
                    return;
                }
                if let Some(index) = pool.queued.pop_front() {
                    let slot = &mut pool.slots[index];
                    slot.phases.send_ns = shared.metrics.now();
                    break (index, slot.hash, slot.plan.take());
                }
                pool = shared.changed.wait(pool).expect("local race pool poisoned");
            }
        };
        let result = (|| -> io::Result<()> {
            let plan = plan.ok_or_else(|| io::Error::other("local race without a plan"))?;
            let _ = shared.scheduling.ready(true);
            let granted: io::Result<()> = (|| {
                loop {
                    if shared.stop.load(Ordering::Acquire) {
                        return Err(io::ErrorKind::Interrupted.into());
                    }
                    if shared.scheduling.take(2 * BLOCK_SIZE, false)? {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_micros(20));
                }
            })();
            let _ = shared.scheduling.ready(false);
            granted?;
            plan.load_with_scratch(page.as_mut_slice(), payload.as_mut_slice())?;
            verify(&hash, payload.as_slice())
        })();
        let mut pool = shared.pool.lock().expect("local race pool poisoned");
        let slot = &mut pool.slots[index];
        let first_byte = shared.metrics.now();
        slot.phases.first_byte_ns = first_byte;
        match &result {
            Ok(()) => {
                slot.bytes.copy_from_slice(payload.as_slice());
                slot.phases.verified_ns = first_byte;
                slot.error = None;
            }
            Err(error) => slot.error = Some(error.kind()),
        }
        slot.done = true;
        if slot.owned {
            if let Some(notify) = &slot.notify {
                let _ = notify.write(1);
            }
        } else {
            slot.notify = None;
            if !slot.used {
                shared.metrics.count(|c| c.race_discarded += 1);
            }
            pool.free.push(index);
        }
    }
}
