//! WAL quotas are distinct from the filesystem's physical free-space account.
use crate::{
    budget::{Budget, BudgetAllocator, BudgetArc},
    encoding::require,
};
use allocator_api2::vec::Vec;
use std::{
    io,
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Clone, Copy)]
pub struct Image {
    pub allocated: u64,
    pub capacity: u64,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Usage {
    pub allocated: u64,
    pub promised: u64,
    pub capacity: u64,
    pub compaction: bool,
    pub stopped: bool,
}

impl Usage {
    fn new(image: Image) -> io::Result<Self> {
        require(
            image.capacity > 0 && image.allocated <= image.capacity,
            "invalid staging capacity",
        )?;
        let mut usage = Self {
            allocated: image.allocated,
            promised: 0,
            capacity: image.capacity,
            compaction: false,
            stopped: false,
        };
        usage.update();
        Ok(usage)
    }

    fn update(&mut self) {
        let used = u128::from(self.allocated) + u128::from(self.promised);
        let capacity = u128::from(self.capacity);
        if used >= capacity {
            self.stopped = true;
        } else if used * 100 < capacity * 60 {
            self.stopped = false;
        }
        self.compaction = self.stopped || used * 100 >= capacity * 75;
    }

    fn fits(&mut self, bytes: u64) -> bool {
        let used = self.allocated.saturating_add(self.promised);
        if bytes > self.capacity.saturating_sub(used) {
            self.stopped = true;
            self.compaction = true;
            return false;
        }
        !self.stopped
    }
}

struct State {
    host: Usage,
    images: Vec<Usage, BudgetAllocator>,
    failed: bool,
}

pub struct Staging {
    state: Mutex<State>,
}

impl Staging {
    pub fn new(
        capacity: u64,
        images: impl ExactSizeIterator<Item = Image>,
        metadata: &Arc<Budget>,
    ) -> io::Result<BudgetArc<Self>> {
        let mut entries = Vec::new_in(BudgetAllocator::new(Arc::clone(metadata)));
        entries
            .try_reserve_exact(images.len())
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        let mut allocated = 0u64;
        for image in images {
            allocated = allocated
                .checked_add(image.allocated)
                .ok_or_else(|| io::Error::other("staging total overflow"))?;
            entries.push(Usage::new(image)?);
        }
        let host = Usage::new(Image {
            allocated,
            capacity,
        })?;
        BudgetArc::try_new(
            Self {
                state: Mutex::new(State {
                    host,
                    images: entries,
                    failed: false,
                }),
            },
            metadata,
        )
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state.failed = true;
            state
        })
    }

    pub fn failed(&self) -> bool {
        self.lock().failed
    }

    pub fn status(&self, image: usize) -> io::Result<(Usage, Usage)> {
        let state = self.lock();
        let image = *state.images.get(image).ok_or(io::ErrorKind::InvalidInput)?;
        Ok((state.host, image))
    }

    pub fn admits(&self, image: usize) -> bool {
        let state = self.lock();
        !state.failed
            && !state.host.stopped
            && state.images.get(image).is_some_and(|image| !image.stopped)
    }

    pub fn reserve(owner: &BudgetArc<Self>, image: usize, bytes: u64) -> io::Result<Permit> {
        let mut state = owner.lock();
        require(
            !state.failed,
            "staging account failed; explicit recovery required",
        )?;
        let image_capacity = state
            .images
            .get(image)
            .ok_or(io::ErrorKind::InvalidInput)?
            .capacity;
        if bytes == 0 || bytes > image_capacity || bytes > state.host.capacity {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let host_fits = state.host.fits(bytes);
        let image_fits = state.images[image].fits(bytes);
        if !(host_fits && image_fits) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        state.host.promised += bytes;
        state.host.update();
        state.images[image].promised += bytes;
        state.images[image].update();
        Ok(Permit {
            owner: owner.clone(),
            image,
            bytes,
            running: false,
        })
    }

    /// Apply only a completed reclamation after removed Log owners are dropped.
    /// Return a gate reopening observed under the accounting lock, so a caller
    /// can notify waiters without racing a new reservation between snapshots.
    pub fn reclaimed(&self, image: usize, allocated: u64) -> io::Result<bool> {
        let mut state = self.lock();
        require(!state.failed, "staging account failed")?;
        let old = state
            .images
            .get(image)
            .ok_or(io::ErrorKind::InvalidInput)?
            .allocated;
        if allocated > old {
            state.failed = true;
            return Err(io::Error::other(
                "unreserved staging growth during reclamation",
            ));
        }
        let host_stopped = state.host.stopped;
        let image_stopped = state.images[image].stopped;
        state.host.allocated -= old - allocated;
        state.host.update();
        state.images[image].allocated = allocated;
        state.images[image].update();
        Ok(
            (host_stopped && !state.host.stopped)
                || (image_stopped && !state.images[image].stopped),
        )
    }
}

/// A running permit is never refunded merely because its caller disappeared.
pub struct Permit {
    owner: BudgetArc<Staging>,
    image: usize,
    bytes: u64,
    running: bool,
}

impl Permit {
    pub fn start(&mut self) {
        self.running = true;
    }

    pub fn installed(mut self, allocated: u64) -> io::Result<()> {
        let mut state = self.owner.lock();
        let old = state.images[self.image].allocated;
        if !self.running || state.failed || allocated < old {
            state.failed = true;
            self.running = true; // An invalid receipt cannot refund its promise.
            return Err(io::Error::other("invalid staging installation receipt"));
        }
        let growth = allocated.checked_sub(old);
        let valid = growth.is_some_and(|n| n <= self.bytes);
        state.images[self.image].allocated = allocated;
        let total = state
            .host
            .allocated
            .checked_sub(old)
            .and_then(|n| n.checked_add(allocated));
        state.host.allocated = total.unwrap_or(u64::MAX);
        state.host.promised -= self.bytes;
        state.images[self.image].promised -= self.bytes;
        self.bytes = 0;
        state.host.update();
        state.images[self.image].update();
        if !valid
            || total.is_none()
            || state.host.allocated > state.host.capacity
            || allocated > state.images[self.image].capacity
        {
            state.failed = true;
            return Err(io::Error::other("staging receipt exceeds its reservation"));
        }
        Ok(())
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        let mut state = self.owner.lock();
        if self.running {
            state.failed = true;
        } else {
            state.host.promised -= self.bytes;
            state.host.update();
            state.images[self.image].promised -= self.bytes;
            state.images[self.image].update();
        }
    }
}

#[cfg(test)]
mod tests;
