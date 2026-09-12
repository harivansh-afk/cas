//! Shared image completion state. The caller holds its mutex through both
//! the storage decision and guest status/used publication.
use crate::inflight::Carrier;
use std::{
    io,
    ops::{Deref, DerefMut},
    sync::{Arc, Mutex, MutexGuard},
};

#[derive(Default)]
pub(super) struct HostGate {
    failure: Mutex<Option<String>>,
}

impl HostGate {
    pub fn failure(&self) -> Option<String> {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn fail(&self, message: String) {
        self.failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert(message);
    }
}

pub struct Gate {
    image: Mutex<ImageState>,
    host: Option<Arc<HostGate>>,
}

impl Gate {
    pub(super) fn new(image: ImageState, host: Option<Arc<HostGate>>) -> Arc<Self> {
        Arc::new(Self {
            image: Mutex::new(image),
            host,
        })
    }

    pub fn lock(&self) -> io::Result<Guard<'_>> {
        let host = self
            .host
            .as_ref()
            .map(|host| host.failure.lock())
            .transpose()
            .map_err(|_| io::Error::other("host completion gate poisoned"))?;
        let mut image = self
            .image
            .lock()
            .map_err(|_| io::Error::other("image completion gate poisoned"))?;
        if let Some(message) = host.as_deref().and_then(|state| state.as_ref()) {
            image.fail(message.clone());
        }
        Ok(Guard { image, _host: host })
    }
}

/// Drop image state before releasing the host's failure/publication boundary.
pub struct Guard<'a> {
    image: MutexGuard<'a, ImageState>,
    _host: Option<MutexGuard<'a, Option<String>>>,
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
