//! Host disk accounting. Callers measure physical allocations and reclaim only
//! after the filesystem operation and its required sync have succeeded.
mod filesystem;
pub use filesystem::{Governor, Observation, Permit};
use std::{
    io,
    sync::{Arc, Mutex},
};

const MIB: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Limits {
    pub capacity: u64,
    pub reserve: u64,
}

impl Limits {
    /// R = 3S + M + 16 MiB. Use the same geometry as the store/compactor.
    pub fn new(capacity: u64, segment: u64, manifest_transaction: u64) -> io::Result<Self> {
        let reserve = segment
            .checked_mul(3)
            .and_then(|n| n.checked_add(manifest_transaction))
            .and_then(|n| n.checked_add(16 * MIB))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "disk reserve overflow"))?;
        if segment == 0 || manifest_transaction == 0 || capacity <= reserve {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "disk capacity cannot preserve the progress reserve",
            ));
        }
        Ok(Self { capacity, reserve })
    }
}

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct Status {
    pub allocated: u64,
    pub promised: u64,
    pub peak_used: u64,
    pub pressured: bool,
    pub background_active: bool,
    pub rejected: u64,
    pub failed: bool,
}

/// One account per host store. Its capacity covers this store's allocated bytes
/// plus usable filesystem free space; the agreed test filesystem excludes other
/// writers. Archives on that filesystem count as allocations too.
#[derive(Debug)]
pub struct Space {
    limits: Limits,
    status: Mutex<Status>,
}

impl Space {
    pub fn new(limits: Limits, allocated: u64) -> io::Result<Arc<Self>> {
        if limits.reserve == 0 || limits.reserve >= limits.capacity || allocated > limits.capacity {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid initial disk accounting",
            ));
        }
        let mut status = Status {
            allocated,
            peak_used: allocated,
            ..Status::default()
        };
        update_pressure(limits, &mut status);
        Ok(Arc::new(Self {
            limits,
            status: Mutex::new(status),
        }))
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }
    pub fn status(&self) -> Status {
        *self.status.lock().expect("space mutex poisoned")
    }

    /// Foreground never consumes R, and remains stopped until the low watermark.
    pub fn foreground(self: &Arc<Self>, bytes: u64) -> io::Result<Reservation> {
        self.reserve(bytes, false)
    }

    /// The single background owner may temporarily borrow the progress reserve.
    /// Ownership remains exclusive even after all promised bytes materialize.
    pub fn background(self: &Arc<Self>, bytes: u64) -> io::Result<Reservation> {
        self.reserve(bytes, true)
    }

    fn reserve(self: &Arc<Self>, bytes: u64, background: bool) -> io::Result<Reservation> {
        let mut status = self.status.lock().expect("space mutex poisoned");
        let used = status.allocated.saturating_add(status.promised);
        let limit = if background {
            self.limits.capacity
        } else {
            self.limits.capacity - self.limits.reserve
        };
        if status.failed {
            return Err(io::Error::other(
                "disk account failed; explicit recovery required",
            ));
        }
        let denied = bytes == 0
            || bytes > limit.saturating_sub(used)
            || (background && (status.background_active || bytes > self.limits.reserve))
            || (!background && status.pressured);
        if denied {
            status.rejected = status.rejected.saturating_add(1);
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "disk admission waits for reclamation or the background owner",
            ));
        }
        status.promised += bytes;
        status.background_active |= background;
        update_pressure(self.limits, &mut status);
        Ok(Reservation {
            space: Arc::clone(self),
            remaining: bytes,
            background,
        })
    }

    /// Release only physically reclaimed bytes; logical dead data still occupies
    /// the store until punching/unlink and the required metadata sync complete.
    pub fn reclaimed(&self, bytes: u64) -> io::Result<()> {
        let mut status = self.status.lock().expect("space mutex poisoned");
        status.allocated = status
            .allocated
            .checked_sub(bytes)
            .ok_or_else(|| io::Error::other("reclamation exceeds accounted allocation"))?;
        update_pressure(self.limits, &mut status);
        Ok(())
    }

    fn fail(&self) {
        self.status.lock().expect("space mutex poisoned").failed = true;
    }

    fn observed(&self, allocated: u64) -> io::Result<()> {
        let mut status = self.status.lock().expect("space mutex poisoned");
        status.allocated = allocated;
        observed(self.limits, &mut status)
    }
}

fn observed(limits: Limits, status: &mut Status) -> io::Result<()> {
    update_pressure(limits, status);
    if u128::from(status.allocated) + u128::from(status.promised) > u128::from(limits.capacity) {
        status.failed = true;
        return Err(io::Error::other(
            "observed filesystem allocation exceeds capacity",
        ));
    }
    Ok(())
}

fn update_pressure(limits: Limits, status: &mut Status) {
    let used = u128::from(status.allocated) + u128::from(status.promised);
    status.peak_used = status.peak_used.max(used.min(u128::from(u64::MAX)) as u64);
    // u128 keeps percentages exact at u64 capacities without multiplication overflow.
    let percent = used * 100;
    let capacity = u128::from(limits.capacity);
    let reserve_intact = used <= u128::from(limits.capacity - limits.reserve);
    if percent >= capacity * 75 || !reserve_intact {
        status.pressured = true;
    } else if percent <= capacity * 60 && reserve_intact {
        status.pressured = false;
    }
}

/// A promise precedes fallocate or output creation. Materialized bytes stay
/// charged across IO errors and orphaning; dropping cancels only unused promise.
#[derive(Debug)]
pub struct Reservation {
    space: Arc<Space>,
    remaining: u64,
    background: bool,
}

impl Reservation {
    pub fn remaining(&self) -> u64 {
        self.remaining
    }

    pub fn materialized(&mut self, bytes: u64) -> io::Result<()> {
        if bytes > self.remaining {
            return Err(io::Error::other(
                "physical allocation exceeds its disk reservation",
            ));
        }
        let mut status = self.space.status.lock().expect("space mutex poisoned");
        status.promised -= bytes;
        status.allocated += bytes;
        self.remaining -= bytes;
        Ok(())
    }

    /// The serialized filesystem owner has completed all IO and observation.
    /// Record excess output before failing; never erase a physical allocation.
    fn finish(&mut self, previous: u64, allocated: u64) -> io::Result<()> {
        let mut status = self.space.status.lock().expect("space mutex poisoned");
        let excess = allocated.saturating_sub(previous) > self.remaining;
        status.allocated = allocated;
        status.promised -= self.remaining;
        self.remaining = 0;
        if self.background {
            status.background_active = false;
            self.background = false;
        }
        observed(self.space.limits, &mut status)?;
        if excess {
            status.failed = true;
            return Err(io::Error::other("observed output exceeds its disk promise"));
        }
        Ok(())
    }

    /// IO returned but its physical outcome cannot be measured. Keep the full
    /// unknown promise in the failed account until explicit filesystem recovery.
    fn unknown(&mut self) {
        self.space.fail();
        self.remaining = 0;
        self.background = false;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let mut status = self.space.status.lock().expect("space mutex poisoned");
        status.promised -= self.remaining;
        if self.background {
            status.background_active = false;
        }
        update_pressure(self.space.limits, &mut status);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn space(allocated: u64) -> Arc<Space> {
        Space::new(
            Limits {
                capacity: 1000,
                reserve: 200,
            },
            allocated,
        )
        .unwrap()
    }

    #[test]
    fn default_reserve_matches_c0_and_arithmetic_is_checked() {
        assert_eq!(
            Limits::new(1024 * MIB, 64 * MIB, 128 * MIB)
                .unwrap()
                .reserve,
            336 * MIB
        );
        assert!(Limits::new(336 * MIB, 64 * MIB, 128 * MIB).is_err());
        assert!(Limits::new(u64::MAX, u64::MAX, 1).is_err());
        assert!(
            Space::new(
                Limits {
                    capacity: 1000,
                    reserve: 200
                },
                1001
            )
            .is_err()
        );
        let s = Space::new(
            Limits {
                capacity: u64::MAX,
                reserve: 1,
            },
            u64::MAX - 1,
        )
        .unwrap();
        assert!(s.status().pressured);
        let mut reservation = s.background(1).unwrap();
        reservation.materialized(1).unwrap();
        assert_eq!(s.status().allocated, u64::MAX);
        assert!(s.background(1).is_err());
    }

    #[test]
    fn promised_and_allocated_space_share_the_cap_and_reserve() {
        let s = space(590);
        let mut first = s.foreground(100).unwrap();
        let second = s.foreground(60).unwrap();
        assert!(s.status().pressured);
        assert!(s.foreground(1).is_err());
        first.materialized(40).unwrap();
        assert_eq!((s.status().allocated, s.status().promised), (630, 120));
        assert!(first.materialized(61).is_err());
        drop((first, second));
        assert_eq!((s.status().allocated, s.status().promised), (630, 0));
        // Dropping promises alone leaves 63% allocated: hysteresis still stops IO.
        assert!(s.foreground(1).is_err());
        s.reclaimed(30).unwrap();
        assert!(!s.status().pressured);
        assert!(s.foreground(1).is_ok());
        assert!(s.reclaimed(601).is_err());
        assert_eq!(s.status().allocated, 600);
    }

    #[test]
    fn background_exclusion_outlives_materialization_and_orphans_remain_charged() {
        let s = space(790);
        assert!(s.foreground(1).is_err());
        let mut background = s.background(200).unwrap();
        assert!(s.background(1).is_err());
        background.materialized(200).unwrap();
        assert_eq!(background.remaining(), 0);
        assert!(s.background(1).is_err());
        drop(background); // A later IO error does not make these extents disappear.
        assert_eq!((s.status().allocated, s.status().promised), (990, 0));
        assert!(!s.status().background_active);
        assert!(s.background(11).is_err());
        assert!(s.background(10).is_ok());
        s.reclaimed(400).unwrap();
        assert!(s.foreground(100).is_ok());
        assert_eq!(s.status().peak_used, 1000);
    }

    #[test]
    fn concurrent_images_cannot_spend_one_anothers_promises() {
        let s = space(0);
        std::thread::scope(|scope| {
            for _ in 0..16 {
                let s = Arc::clone(&s);
                scope.spawn(move || {
                    for _ in 0..1000 {
                        if let Ok(promise) = s.foreground(100) {
                            std::thread::yield_now();
                            assert!(s.status().allocated + s.status().promised <= 800);
                            drop(promise);
                        }
                    }
                });
            }
        });
        assert_eq!(s.status().promised, 0);
        assert!(s.status().peak_used <= 800);
        assert!(!s.status().pressured);
    }
}
