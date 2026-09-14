//! Shared bulk submission opportunities; owners and IO completion stay in callers.
use crate::budget::{Budget, BudgetAllocator, BudgetArc};
use allocator_api2::vec::Vec;
use std::{
    cell::RefCell,
    io,
    marker::PhantomData,
    os::fd::{AsFd, OwnedFd},
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Selected {
    Demand(usize),
    Background,
}

struct Image {
    ready: bool,
    wake: Option<OwnedFd>,
}

#[derive(Default, Clone, Copy, serde::Serialize)]
pub struct Counters {
    pub demand: u64,
    pub background: u64,
    pub borrowed_demand: u64,
    pub borrowed_background: u64,
    pub demand_requested_bytes: u64,
    pub background_requested_bytes: u64,
}

#[derive(Clone, Copy, serde::Serialize)]
pub struct Status {
    pub counters: Counters,
    pub ready_images: usize,
    pub background_ready: bool,
}

struct State {
    images: Vec<Image, BudgetAllocator>,
    next_image: usize,
    opportunity: u8,
    background_ready: bool,
    counters: Counters,
}

pub struct Scheduler {
    state: Mutex<State>,
    changed: Condvar,
}

#[derive(Clone)]
pub struct Port {
    owner: BudgetArc<Scheduler>,
    image: usize,
}

thread_local! {
    static BACKGROUND: RefCell<Option<BudgetArc<Scheduler>>> = const { RefCell::new(None) };
}

pub struct Background {
    previous: Option<BudgetArc<Scheduler>>,
    _thread: PhantomData<Rc<()>>,
}

impl Drop for Background {
    fn drop(&mut self) {
        BACKGROUND.with(|current| *current.borrow_mut() = self.previous.take());
    }
}

impl State {
    fn selected(&self) -> Option<Selected> {
        let demand = (0..self.images.len())
            .map(|offset| (self.next_image + offset) % self.images.len())
            .find(|&index| self.images[index].ready)
            .map(Selected::Demand);
        if self.background_ready && (self.opportunity == 0 || demand.is_none()) {
            Some(Selected::Background)
        } else {
            demand
        }
    }

    fn grant(&mut self, selected: Selected, bytes: usize) {
        match selected {
            Selected::Demand(image) => {
                self.images[image].ready = false;
                self.next_image = (image + 1) % self.images.len();
                self.counters.demand += 1;
                self.counters.demand_requested_bytes += bytes as u64;
                self.counters.borrowed_demand += u64::from(self.opportunity == 0);
            }
            Selected::Background => {
                self.background_ready = false;
                self.counters.background += 1;
                self.counters.background_requested_bytes += bytes as u64;
                self.counters.borrowed_background += u64::from(self.opportunity != 0);
            }
        }
        self.opportunity = (self.opportunity + 1) % 4;
    }
}

impl Scheduler {
    pub fn new(images: usize, metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        if images == 0 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let mut entries = Vec::new_in(BudgetAllocator::new(Arc::clone(metadata)));
        entries
            .try_reserve_exact(images)
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        entries.resize_with(images, || Image {
            ready: false,
            wake: None,
        });
        BudgetArc::try_new(
            Self {
                state: Mutex::new(State {
                    images: entries,
                    next_image: 0,
                    opportunity: 0,
                    background_ready: false,
                    counters: Counters::default(),
                }),
                changed: Condvar::new(),
            },
            metadata,
        )
    }

    /// The caller transfers a nonblocking eventfd already bound to its reactor.
    pub fn bind(&self, image: usize, wake: OwnedFd) -> io::Result<()> {
        let mut state = self.state.lock().expect("IO scheduler poisoned");
        let entry = state
            .images
            .get_mut(image)
            .ok_or(io::ErrorKind::InvalidInput)?;
        if entry.wake.is_some() {
            return Err(io::Error::other("duplicate IO scheduler binding"));
        }
        entry.wake = Some(wake);
        Ok(())
    }

    pub fn port(owner: &BudgetArc<Self>, image: usize) -> io::Result<Port> {
        if image
            >= owner
                .state
                .lock()
                .expect("IO scheduler poisoned")
                .images
                .len()
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Port {
            owner: owner.clone(),
            image,
        })
    }

    pub fn background(owner: &BudgetArc<Self>) -> Background {
        let previous = BACKGROUND.with(|current| current.replace(Some(owner.clone())));
        Background {
            previous,
            _thread: PhantomData,
        }
    }

    pub fn counters(&self) -> Counters {
        self.state.lock().expect("IO scheduler poisoned").counters
    }

    pub fn status(&self) -> Status {
        let state = self.state.lock().expect("IO scheduler poisoned");
        Status {
            counters: state.counters,
            ready_images: state.images.iter().filter(|image| image.ready).count(),
            background_ready: state.background_ready,
        }
    }

    fn notify(&self, state: &State) -> io::Result<()> {
        match state.selected() {
            Some(Selected::Background) => self.changed.notify_all(),
            Some(Selected::Demand(image)) => {
                if let Some(wake) = &state.images[image].wake {
                    crate::eventfd::notify(wake.as_fd())?;
                }
            }
            None => (),
        }
        Ok(())
    }

    fn wait_background(&self, bytes: usize) -> io::Result<()> {
        let _measurement = crate::io_metrics::measure(bytes as u64, |c| &mut c.scheduler_wait);
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut state = self.state.lock().expect("IO scheduler poisoned");
        if state.background_ready {
            return Err(io::Error::other("more than one compaction IO owner"));
        }
        state.background_ready = true;
        loop {
            if state.selected() == Some(Selected::Background) {
                state.grant(Selected::Background, bytes);
                self.notify(&state)?;
                return Ok(());
            }
            if Instant::now() >= deadline {
                state.background_ready = false;
                self.notify(&state)?;
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "background IO submission deadline expired",
                ));
            }
            if let Err(error) = self.notify(&state) {
                state.background_ready = false;
                return Err(error);
            }
            state = self
                .changed
                .wait_timeout(state, deadline.saturating_duration_since(Instant::now()))
                .expect("IO scheduler poisoned")
                .0;
        }
    }
}

impl Port {
    pub fn ready(&self, ready: bool) -> io::Result<()> {
        let mut state = self.owner.state.lock().expect("IO scheduler poisoned");
        state.images[self.image].ready = ready;
        self.owner.notify(&state)
    }

    pub fn take(&self, bytes: usize, more_ready: bool) -> io::Result<bool> {
        let mut state = self.owner.state.lock().expect("IO scheduler poisoned");
        let selected = Selected::Demand(self.image);
        if state.selected() != Some(selected) {
            return Ok(false);
        }
        state.grant(selected, bytes);
        state.images[self.image].ready = more_ready;
        self.owner.notify(&state)?;
        Ok(true)
    }
}

pub(crate) fn before_background_io(bytes: usize) -> io::Result<()> {
    let owner = BACKGROUND.with(|current| current.borrow().clone());
    owner.map_or(Ok(()), |owner| owner.wait_background(bytes))
}

#[cfg(test)]
mod tests;
