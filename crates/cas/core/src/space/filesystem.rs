//! Absolute filesystem observation with one serialized allocation owner.
use super::{Limits, Reservation, Space, Status};
use crate::{encoding::require, segments::Tickets};
use std::{
    fmt,
    fs::File,
    io,
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
struct Domain {
    device: u64,
    filesystem: u64,
    capacity: u64,
    unit: u64,
}

#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Observation {
    domain: Domain,
    pub allocated: u64,
}

impl Observation {
    pub fn inspect(tickets: &Tickets) -> io::Result<Self> {
        Self::read(tickets.root_file())
    }

    pub fn capacity(self) -> u64 {
        self.domain.capacity
    }

    fn read(root: &File) -> io::Result<Self> {
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: root remains open; stat points to writable storage of the
        // exact libc result type. It is read only after a successful syscall.
        if unsafe { libc::fstatvfs(root.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatvfs initialized all result fields.
        let stat = unsafe { stat.assume_init() };
        let unit = stat.f_frsize;
        require(
            unit != 0 && stat.f_bavail <= stat.f_blocks,
            "invalid filesystem geometry",
        )?;
        let capacity = stat
            .f_blocks
            .checked_mul(unit)
            .ok_or_else(|| io::Error::other("filesystem capacity overflow"))?;
        let allocated = (stat.f_blocks - stat.f_bavail)
            .checked_mul(unit)
            .ok_or_else(|| io::Error::other("filesystem allocation overflow"))?;
        Ok(Self {
            domain: Domain {
                device: root.metadata()?.dev(),
                filesystem: stat.f_fsid,
                capacity,
                unit,
            },
            allocated,
        })
    }
}

/// The caller establishes a dedicated, fixed filesystem allocation domain.
/// This account retains its actual root lock and charges every observed byte.
pub struct Governor {
    tickets: Arc<Tickets>,
    domain: Domain,
    space: Arc<Space>,
    owner: Mutex<()>,
    #[cfg(test)]
    samples: Mutex<std::collections::VecDeque<io::Result<Observation>>>,
}

impl Governor {
    pub fn open(tickets: Arc<Tickets>, limits: Limits) -> io::Result<Arc<Self>> {
        let initial = Observation::read(tickets.root_file())?;
        require(
            limits.capacity <= initial.capacity(),
            "admission capacity exceeds filesystem",
        )?;
        Ok(Arc::new(Self {
            tickets,
            domain: initial.domain,
            space: Space::new(limits, initial.allocated)?,
            owner: Mutex::new(()),
            #[cfg(test)]
            samples: Mutex::new(Default::default()),
        }))
    }

    pub fn limits(&self) -> Limits {
        self.space.limits()
    }
    pub fn status(&self) -> Status {
        self.space.status()
    }
    pub fn capacity(&self) -> u64 {
        self.domain.capacity
    }

    pub fn uses_tickets(&self, tickets: &Arc<Tickets>) -> bool {
        Arc::ptr_eq(&self.tickets, tickets)
    }

    pub fn validate_file(&self, file: &File) -> io::Result<()> {
        require(
            Observation::read(file)?.domain == self.domain,
            "file is outside the allocation domain",
        )
    }

    pub fn foreground(self: &Arc<Self>, bytes: u64) -> io::Result<Permit> {
        Ok(self.permit(self.space.foreground(bytes)?))
    }

    pub fn background(self: &Arc<Self>, bytes: u64) -> io::Result<Permit> {
        Ok(self.permit(self.space.background(bytes)?))
    }

    fn permit(self: &Arc<Self>, reservation: Reservation) -> Permit {
        Permit {
            governor: Arc::clone(self),
            reservation: Some(reservation),
        }
    }

    fn lock(&self) -> io::Result<MutexGuard<'_, ()>> {
        let owner = self
            .owner
            .lock()
            .map_err(|_| io::Error::other("filesystem owner panicked"))?;
        require(
            !self.status().failed,
            "filesystem account failed; explicit recovery required",
        )?;
        Ok(owner)
    }

    fn sample(&self) -> io::Result<Observation> {
        #[cfg(test)]
        if let Some(sample) = self.samples.lock().unwrap().pop_front() {
            return sample;
        }
        Observation::inspect(&self.tickets)
    }

    fn observe(&self) -> io::Result<Observation> {
        let sample = self.sample()?;
        require(
            sample.domain == self.domain,
            "filesystem allocation domain changed",
        )?;
        Ok(sample)
    }

    /// Recheck delayed frees without retiring any queued operation's promise.
    /// Like run(), this performs filesystem IO and belongs on the owner worker.
    pub fn refresh(&self) -> io::Result<Observation> {
        let _owner = self.lock()?;
        let result = self.observe().and_then(|sample| {
            self.space.observed(sample.allocated)?;
            Ok(sample)
        });
        if result.is_err() {
            self.space.fail();
        }
        result
    }
}

/// An unused permit can cancel. A running permit stays with its worker until
/// all IO, required sync and final observation return, even after caller timeout.
pub struct Permit {
    governor: Arc<Governor>,
    reservation: Option<Reservation>,
}

impl Permit {
    /// Serialize a complete allocation transaction. The closure must not return
    /// while kernel IO still owns its output or recursively acquire this owner.
    pub fn run<T>(self, operation: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        self.run_with(operation)
    }

    /// `run` for operations whose own error type also carries IO failures.
    pub fn run_with<T, E: From<io::Error> + fmt::Display>(
        mut self,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, E> {
        let _owner = self.governor.lock()?;
        let mut attempt = Attempt {
            governor: &self.governor,
            previous: self.governor.status().allocated,
            reservation: self.reservation.take(),
        };
        let result = operation();
        let observed = attempt.finish();
        match (result, observed) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) => Err(error),
            (Ok(_), Err(error)) => Err(error.into()),
            (Err(operation), Err(observation)) => Err(io::Error::other(format!(
                "allocation operation: {operation}; physical observation: {observation}"
            ))
            .into()),
        }
    }
}

struct Attempt<'a> {
    governor: &'a Governor,
    previous: u64,
    reservation: Option<Reservation>,
}

impl Attempt<'_> {
    fn finish(&mut self) -> io::Result<()> {
        let mut reservation = self
            .reservation
            .take()
            .expect("one observation per attempt");
        match self.governor.observe() {
            Ok(sample) => reservation.finish(self.previous, sample.allocated),
            Err(error) => {
                reservation.unknown();
                Err(error)
            }
        }
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if self.reservation.is_some() {
            let _ = self.finish();
            self.governor.space.fail();
        }
    }
}

#[cfg(test)]
mod tests;
