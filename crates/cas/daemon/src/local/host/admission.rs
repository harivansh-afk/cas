//! Guest owners outlive queue publication; GC owns the pause through its IO.
use super::*;

#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct Status {
    pub generation: u64,
    pub active: usize,
    pub paused: bool,
    pub running: bool,
    pub failed: bool,
}

struct Wakes {
    frontend: EventFd,
    reactor: EventFd,
    attached: std::sync::atomic::AtomicBool,
}

pub struct Admission {
    state: Mutex<Status>,
    wakes: BudgetVec<OnceLock<Wakes>, BudgetAllocator>,
}

impl Admission {
    pub fn new(images: usize, metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        let mut wakes = reserved_vec(images, metadata)?;
        wakes.resize_with(images, OnceLock::new);
        BudgetArc::try_new(
            Self {
                state: Mutex::new(Status::default()),
                wakes,
            },
            metadata,
        )
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Status> {
        self.state.lock().unwrap_or_else(|error| {
            let mut state = error.into_inner();
            state.failed = true;
            state.paused = true;
            state
        })
    }

    pub fn status(&self) -> Status {
        *self.lock()
    }

    pub fn bind(&self, index: usize, frontend: EventFd, reactor: EventFd) -> io::Result<()> {
        self.wakes
            .get(index)
            .ok_or(io::ErrorKind::InvalidInput)?
            .set(Wakes {
                frontend,
                reactor,
                attached: std::sync::atomic::AtomicBool::new(true),
            })
            .map_err(|_| io::Error::other("duplicate image wake binding"))?;
        Ok(())
    }

    pub fn attached(&self, index: usize) -> bool {
        self.wakes[index]
            .get()
            .is_some_and(|wake| wake.attached.load(Ordering::Acquire))
    }

    pub fn detach(&self, index: usize) {
        if let Some(wake) = self.wakes[index].get() {
            wake.attached.store(false, Ordering::Release);
        }
    }

    pub(super) fn wake_frontend(&self, index: usize) -> io::Result<()> {
        if let Some(wake) = self.wakes.get(index).and_then(OnceLock::get)
            && wake.attached.load(Ordering::Acquire)
        {
            notify(&wake.frontend)?;
        }
        Ok(())
    }

    fn wake(&self) -> io::Result<()> {
        for wake in self.wakes.iter().filter_map(OnceLock::get) {
            notify(&wake.frontend)?;
            notify(&wake.reactor)?;
        }
        Ok(())
    }

    pub fn enter(owner: &BudgetArc<Self>) -> Option<Entry> {
        let mut state = owner.lock();
        if state.failed || state.paused {
            return None;
        }
        state.active = state.active.checked_add(1)?;
        Some(Entry {
            owner: owner.clone(),
        })
    }

    pub fn pause(owner: &BudgetArc<Self>) -> io::Result<Quiescence> {
        let mut state = owner.lock();
        if state.failed {
            return Err(io::Error::other("host admission failed"));
        }
        if state.paused {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("quiescence generation exhausted"))?;
        state.paused = true;
        let pause = Quiescence {
            owner: owner.clone(),
            generation: state.generation,
            started: false,
            released: false,
        };
        drop(state);
        owner.wake()?;
        Ok(pause)
    }
}

pub struct Entry {
    owner: BudgetArc<Admission>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        self.owner.lock().active -= 1;
    }
}

pub struct Quiescence {
    owner: BudgetArc<Admission>,
    generation: u64,
    started: bool,
    released: bool,
}

impl Quiescence {
    pub(super) fn fail(&mut self) {
        let mut state = self.owner.lock();
        state.failed = true;
        state.paused = true;
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn drained(&self) -> bool {
        self.owner.status().active == 0
    }

    /// Call only after the controller also observes every reactor's drain ack.
    pub fn begin(&mut self) -> io::Result<()> {
        let mut state = self.owner.lock();
        if state.failed || state.generation != self.generation || self.started {
            return Err(io::Error::other("invalid quiescence start"));
        }
        if state.active != 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.started = true;
        state.running = true;
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        let mut state = self.owner.lock();
        if state.failed || state.generation != self.generation || !self.started || state.active != 0
        {
            return Err(io::Error::other("invalid quiescence completion"));
        }
        state.running = false;
        state.paused = false;
        self.released = true;
        drop(state);
        self.owner.wake()
    }
}

impl Drop for Quiescence {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        let mut state = self.owner.lock();
        if self.started {
            state.failed = true;
        }
        if !state.failed {
            state.paused = false;
        }
        drop(state);
        let _ = self.owner.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_wakes_bound_owners_and_cancellation_resumes_without_losing_entries() {
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let gate = Admission::new(1, &metadata).unwrap();
        let front = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
        let reactor = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
        gate.bind(0, front.try_clone().unwrap(), reactor.try_clone().unwrap())
            .unwrap();
        let entry = Admission::enter(&gate).unwrap();
        let mut pause = Admission::pause(&gate).unwrap();
        assert_eq!(pause.generation(), 1);
        assert_eq!(front.read().unwrap(), 1);
        assert_eq!(reactor.read().unwrap(), 1);
        assert!(!pause.drained());
        assert_eq!(pause.begin().unwrap_err().kind(), io::ErrorKind::WouldBlock);
        assert!(Admission::enter(&gate).is_none());
        assert!(Admission::pause(&gate).is_err());
        drop(pause);
        assert_eq!(front.read().unwrap(), 1);
        assert_eq!(reactor.read().unwrap(), 1);
        assert_eq!(gate.status().active, 1);
        let second = Admission::enter(&gate).unwrap();
        drop((entry, second));
        let mut pause = Admission::pause(&gate).unwrap();
        assert_eq!(pause.generation(), 2);
        assert!(pause.drained());
        pause.begin().unwrap();
        pause.finish().unwrap();
        assert!(!gate.status().paused);
        assert!(!gate.status().running);
        assert_eq!(gate.status().active, 0);
        drop(gate);
        assert_eq!(metadata.usage().current, Amount::default());
    }

    #[test]
    fn concurrent_entries_drain_before_begin_and_running_owner_loss_fails_closed() {
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let gate = Admission::new(0, &metadata).unwrap();
        let barrier = std::sync::Barrier::new(17);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let gate = &gate;
                let barrier = &barrier;
                scope.spawn(move || {
                    let entry = Admission::enter(gate).unwrap();
                    barrier.wait();
                    barrier.wait();
                    assert!(Admission::enter(gate).is_none());
                    drop(entry);
                });
            }
            barrier.wait();
            let mut pause = Admission::pause(&gate).unwrap();
            assert_eq!(gate.status().active, 16);
            assert_eq!(pause.begin().unwrap_err().kind(), io::ErrorKind::WouldBlock);
            barrier.wait();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !pause.drained() {
                assert!(Instant::now() < deadline);
                std::thread::yield_now();
            }
            pause.begin().unwrap();
            drop(pause);
        });
        assert!(gate.status().failed);
        assert!(gate.status().paused);
        assert!(Admission::enter(&gate).is_none());
        assert!(Admission::pause(&gate).is_err());
        drop(gate);
        assert_eq!(metadata.usage().current, Amount::default());
    }

    #[test]
    fn denied_metadata_and_final_entry_release_account_actual_gate_storage() {
        let denied = Budget::new(Amount::default());
        assert!(Admission::new(0, &denied).is_err());
        assert!(Admission::new(1, &denied).is_err());
        assert_eq!(denied.usage().current, Amount::default());
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let gate = Admission::new(1, &metadata).unwrap();
        let entry = Admission::enter(&gate).unwrap();
        let allocated = metadata.usage().current;
        drop(gate);
        assert_eq!(metadata.usage().current, allocated);
        drop(entry);
        assert_eq!(metadata.usage().current, Amount::default());
    }
}
