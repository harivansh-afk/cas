//! Shared image completion state. The caller holds its mutex through both
//! the storage decision and guest status/used publication.
use crate::inflight::Carrier;
use cas_core::budget::{Budget, BudgetArc};
use std::{
    io,
    ops::{Deref, DerefMut},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Default)]
pub(super) struct HostGate {
    failure: Mutex<Option<String>>,
    /// A failure recorded by a thread that may hold `failure` higher in its
    /// call chain (the frontend's IO thread inside the fast path); applied by
    /// the next `failure()` or `Gate::lock()`.
    pending: Mutex<Option<String>>,
}

fn take_pending(pending: &Mutex<Option<String>>) -> Option<String> {
    pending.lock().unwrap_or_else(|e| e.into_inner()).take()
}

impl HostGate {
    pub(super) fn new(metadata: &Arc<Budget>) -> io::Result<BudgetArc<Self>> {
        BudgetArc::try_new(Self::default(), metadata)
    }

    pub fn failure(&self) -> Option<String> {
        let mut failure = self.failure.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(message) = take_pending(&self.pending) {
            failure.get_or_insert(message);
        }
        failure.clone()
    }

    pub fn fail(&self, message: String) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(message);
    }

    /// Fail without taking the gate: for the frontend thread, which holds the
    /// completion gate across admission and publication and would deadlock
    /// on `fail`. The failure lands at the next `failure()` or `Gate::lock()`.
    pub fn fail_pending(&self, message: String) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(message);
    }
}

pub struct Gate {
    writer_fenced: AtomicBool,
    image: Mutex<ImageState>,
    host: Option<BudgetArc<HostGate>>,
    /// Mirror of `image.failure.is_some()`, written when a guard drops, so a
    /// thread already holding the gate can read the state without locking.
    failed: AtomicBool,
    pending: Mutex<Option<String>>,
}

impl Gate {
    pub(super) fn new(
        image: ImageState,
        host: Option<BudgetArc<HostGate>>,
        metadata: &Arc<Budget>,
    ) -> io::Result<BudgetArc<Self>> {
        let image_failed = image.failure.is_some();
        BudgetArc::try_new(
            Self {
                writer_fenced: AtomicBool::new(false),
                image: Mutex::new(image),
                host,
                failed: AtomicBool::new(image_failed),
                pending: Mutex::new(None),
            },
            metadata,
        )
    }

    pub fn lock(&self) -> io::Result<Guard<'_>> {
        let mut host = self
            .host
            .as_ref()
            .map(|host| host.failure.lock())
            .transpose()
            .map_err(|_| io::Error::other("host completion gate poisoned"))?;
        if let (Some(host), Some(state)) = (&self.host, host.as_deref_mut())
            && let Some(message) = take_pending(&host.pending)
        {
            state.get_or_insert(message);
        }
        let mut image = self
            .image
            .lock()
            .map_err(|_| io::Error::other("image completion gate poisoned"))?;
        if let Some(message) = take_pending(&self.pending) {
            image.fail(message);
        }
        if let Some(message) = host.as_deref().and_then(|state| state.as_ref()) {
            image.fail(message.clone());
        }
        Ok(Guard {
            image,
            _host: host,
            failed: &self.failed,
        })
    }

    /// Whether the image has recorded a failure, without taking the gate.
    /// The frontend's IO thread holds the gate across admission, so the fast
    /// path reads this instead of locking. A failure recorded under a guard
    /// becomes visible when that guard drops; a pending one at once.
    pub fn is_failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
            || self
                .pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    /// Fail without taking the gate (see `is_failed`); applied by the next
    /// `lock()`.
    pub fn fail_pending(&self, message: String) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(message);
    }

    // Admission may already hold the completion gate. This terminal flag is
    // published under quiescence and must never recursively acquire that gate.
    pub fn writer_fenced(&self) -> bool {
        self.writer_fenced.load(Ordering::Acquire)
    }
    pub fn fence_writer(&self) {
        self.writer_fenced.store(true, Ordering::Release);
    }

    /// Lock, then refuse further work once the image has recorded a failure.
    pub fn lock_checked(&self) -> io::Result<Guard<'_>> {
        let guard = self.lock()?;
        if let Some(error) = &guard.failure {
            return Err(io::Error::other(error.clone()));
        }
        Ok(guard)
    }
}

/// Drop image state before releasing the host's failure/publication boundary.
pub struct Guard<'a> {
    image: MutexGuard<'a, ImageState>,
    _host: Option<MutexGuard<'a, Option<String>>>,
    failed: &'a AtomicBool,
}

impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.failed
            .store(self.image.failure.is_some(), Ordering::Release);
    }
}

impl Guard<'_> {
    /// Publish shared failure while already holding the host completion gate.
    pub(super) fn fail_host(&mut self, message: String) {
        if let Some(host) = &mut self._host {
            host.get_or_insert_with(|| message.clone());
        }
        self.image.fail(message);
    }
}

impl Deref for Guard<'_> {
    type Target = ImageState;
    fn deref(&self) -> &Self::Target {
        &self.image
    }
}
impl DerefMut for Guard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.image
    }
}

#[derive(Default)]
pub struct ImageState {
    pub failure: Option<String>,
    pub carrier: Option<Carrier>,
    pub durable: u64,
}

impl ImageState {
    pub fn snapshot(&self) -> Option<crate::fault::Snapshot> {
        self.carrier.as_ref().map(|carrier| crate::fault::Snapshot {
            published: carrier.published(),
            durable: self.durable,
        })
    }

    pub fn fail(&mut self, message: String) {
        if let Some(carrier) = &mut self.carrier {
            carrier.fail();
        }
        self.failure.get_or_insert(message);
    }

    pub fn publish(&mut self, prefix: u64) -> io::Result<()> {
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(failure.clone()));
        }
        if let Some(carrier) = &mut self.carrier {
            carrier.publish(prefix)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cas_core::budget::Amount;

    #[test]
    fn gate_allocation_refusal_preserves_existing_host_failure_ownership() {
        let metadata = Budget::new(Amount {
            bytes: 4096,
            requests: 0,
        });
        let host = HostGate::new(&metadata).unwrap();
        let baseline = metadata.usage().current.bytes;
        let held = metadata
            .reserve(Amount {
                bytes: 4096 - baseline,
                requests: 0,
            })
            .unwrap();
        assert_eq!(
            HostGate::new(&metadata).err().unwrap().kind(),
            io::ErrorKind::OutOfMemory
        );
        assert_eq!(
            Gate::new(ImageState::default(), Some(host.clone()), &metadata)
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::OutOfMemory
        );
        assert_eq!(metadata.usage().current.bytes, 4096);
        drop(held);
        let image = Gate::new(ImageState::default(), Some(host.clone()), &metadata).unwrap();
        host.fail("retained host failed".into());
        drop(host);
        assert_eq!(
            image.lock().unwrap().failure.as_deref(),
            Some("retained host failed")
        );
        assert!(metadata.usage().current.bytes > baseline);
        drop(image);
        assert_eq!(metadata.usage().current, Amount::default());
    }
}
