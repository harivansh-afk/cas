//! One in-progress owner per hash; completed readers keep their exact cell.
use crate::{
    budget::{Amount, Budget, BudgetAllocator, BudgetArc, Lease, Usage},
    chunk_index::Hash,
};
use allocator_api2::vec::Vec;
use hashbrown::HashTable;
use std::{
    io,
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::{Arc, Mutex},
};

struct Entry<T> {
    hash: Hash,
    cell: BudgetArc<Cell<T>>,
}

#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct Counters {
    pub started: u64,
    pub joined: u64,
    pub completed: u64,
    pub failed: u64,
}

#[derive(serde::Serialize)]
pub struct Status {
    pub pending_keys: usize,
    pub leaders: Usage,
    pub waiters: Usage,
    pub counters: Counters,
}

struct State<T> {
    entries: HashTable<Entry<T>, BudgetAllocator>,
    counters: Counters,
}

pub struct Registry<T> {
    state: Mutex<State<T>>,
    leaders: Arc<Budget>,
    waiters: Arc<Budget>,
    metadata: Arc<Budget>,
}

pub enum Lookup<T> {
    Leader(Leader<T>),
    Waiter(Waiter<T>),
}

pub struct Leader<T> {
    registry: BudgetArc<Registry<T>>,
    cell: BudgetArc<Cell<T>>,
    hash: Hash,
    finished: bool,
    _credit: Lease,
}

pub struct Waiter<T> {
    cell: BudgetArc<Cell<T>>,
    signal: BudgetArc<Signal>,
    id: u64,
    _credit: Lease,
}

enum Phase<T> {
    Pending,
    Ready(BudgetArc<T>),
    Failed(io::ErrorKind),
}

struct Completion<T> {
    phase: Phase<T>,
    signals: Vec<(u64, BudgetArc<Signal>), BudgetAllocator>,
    next_id: u64,
}

struct Cell<T> {
    completion: Mutex<Completion<T>>,
}

struct Signal(OwnedFd);

fn bucket(hash: &Hash) -> u64 {
    u64::from_le_bytes(hash[..8].try_into().unwrap())
}

fn counter(limit: usize) -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 0,
        requests: limit,
    })
}

fn claim(budget: &Arc<Budget>) -> Option<Lease> {
    budget.reserve(Amount {
        bytes: 0,
        requests: 1,
    })
}

impl<T> Registry<T> {
    pub fn new(
        leaders: usize,
        waiters: usize,
        metadata: &Arc<Budget>,
    ) -> io::Result<BudgetArc<Self>> {
        if leaders == 0 || waiters == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty fetch capacity",
            ));
        }
        let mut entries = HashTable::new_in(BudgetAllocator::new(Arc::clone(metadata)));
        entries
            .try_reserve(leaders, |entry: &Entry<T>| bucket(&entry.hash))
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
        BudgetArc::try_new(
            Self {
                state: Mutex::new(State {
                    entries,
                    counters: Counters::default(),
                }),
                leaders: counter(leaders),
                waiters: counter(waiters),
                metadata: Arc::clone(metadata),
            },
            metadata,
        )
    }

    pub fn lookup(owner: &BudgetArc<Self>, hash: Hash) -> io::Result<Option<Lookup<T>>> {
        let mut state = owner.state.lock().expect("fetch registry poisoned");
        if let Some(entry) = state
            .entries
            .find(bucket(&hash), |entry| entry.hash == hash)
        {
            let Some(credit) = claim(&owner.waiters) else {
                return Ok(None);
            };
            let cell = entry.cell.clone();
            let signal = Signal::new(&owner.metadata)?;
            let mut completion = cell.completion.lock().expect("fetch cell poisoned");
            let id = completion.next_id;
            let next = id
                .checked_add(1)
                .ok_or_else(|| io::Error::other("fetch waiter IDs exhausted"))?;
            completion
                .signals
                .try_reserve(1)
                .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
            completion.signals.push((id, signal.clone()));
            completion.next_id = next;
            drop(completion);
            state.counters.joined += 1;
            return Ok(Some(Lookup::Waiter(Waiter {
                cell,
                signal,
                id,
                _credit: credit,
            })));
        }
        let Some(credit) = claim(&owner.leaders) else {
            return Ok(None);
        };
        let cell = BudgetArc::try_new(
            Cell {
                completion: Mutex::new(Completion {
                    phase: Phase::Pending,
                    signals: Vec::new_in(BudgetAllocator::new(Arc::clone(&owner.metadata))),
                    next_id: 0,
                }),
            },
            &owner.metadata,
        )?;
        assert!(
            state.entries.len() < state.entries.capacity(),
            "fetch table capacity reserved at startup"
        );
        state.entries.insert_unique(
            bucket(&hash),
            Entry {
                hash,
                cell: cell.clone(),
            },
            |entry| bucket(&entry.hash),
        );
        state.counters.started += 1;
        Ok(Some(Lookup::Leader(Leader {
            registry: owner.clone(),
            cell,
            hash,
            finished: false,
            _credit: credit,
        })))
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("fetch registry poisoned");
        Status {
            pending_keys: state.entries.len(),
            leaders: self.leaders.usage(),
            waiters: self.waiters.usage(),
            counters: state.counters,
        }
    }
}

impl<T> Leader<T> {
    pub fn complete(mut self, value: BudgetArc<T>) -> io::Result<()> {
        self.finish(Phase::Ready(value))
    }

    pub fn fail(mut self, error: io::ErrorKind) -> io::Result<()> {
        self.finish(Phase::Failed(error))
    }

    fn finish(&mut self, result: Phase<T>) -> io::Result<()> {
        assert!(!self.finished, "one fetch publication");
        let mut state = self.registry.state.lock().expect("fetch registry poisoned");
        let entry = state
            .entries
            .find_entry(bucket(&self.hash), |entry| {
                entry.hash == self.hash && entry.cell.ptr_eq(&self.cell)
            })
            .unwrap_or_else(|_| panic!("fetch leader lost its exact cell"))
            .remove()
            .0;
        if matches!(result, Phase::Ready(_)) {
            state.counters.completed += 1;
        } else {
            state.counters.failed += 1;
        }
        self.finished = true;
        drop((entry, state));
        let mut completion = self.cell.completion.lock().expect("fetch cell poisoned");
        completion.phase = result;
        let mut notified = Ok(());
        for (_, signal) in completion.signals.drain(..) {
            if let Err(error) = signal.notify() {
                notified = Err(error);
            }
        }
        notified
    }
}

impl<T> Drop for Leader<T> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.finish(Phase::Failed(io::ErrorKind::Interrupted));
        }
    }
}

impl<T> Waiter<T> {
    /// Keep this waiter alive until the poll CQE is observed or retained forever.
    pub fn notification(&self) -> RawFd {
        self.signal.0.as_raw_fd()
    }

    pub fn poll(&self) -> io::Result<Option<BudgetArc<T>>> {
        let completion = self.cell.completion.lock().expect("fetch cell poisoned");
        match &completion.phase {
            Phase::Pending => Ok(None),
            Phase::Ready(value) => Ok(Some(value.clone())),
            Phase::Failed(error) => Err((*error).into()),
        }
    }
}

impl<T> Drop for Waiter<T> {
    fn drop(&mut self) {
        let mut completion = self.cell.completion.lock().expect("fetch cell poisoned");
        if let Some(index) = completion.signals.iter().position(|(id, _)| *id == self.id) {
            completion.signals.swap_remove(index);
        }
    }
}

impl Signal {
    fn new(metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        // SAFETY: eventfd takes scalar arguments and returns a new owned FD.
        let raw = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the successful call returned a fresh FD owned only here.
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        BudgetArc::try_new(Self(owned), metadata)
    }

    fn notify(&self) -> io::Result<()> {
        let one = 1u64;
        loop {
            // SAFETY: the FD is owned; one is an initialized eight-byte value.
            let result = unsafe {
                libc::write(
                    self.0.as_raw_fd(),
                    (&one as *const u64).cast(),
                    size_of::<u64>(),
                )
            };
            if result == size_of::<u64>() as isize {
                return Ok(());
            }
            if result >= 0 {
                return Err(io::Error::other("short fetch notification"));
            }
            let error = io::Error::last_os_error();
            match error.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()), // A notification is already pending.
                _ => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests;
