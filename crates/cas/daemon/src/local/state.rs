//! Shared image completion state. The caller holds its mutex through both
//! the storage decision and guest status/used publication.
use cas_daemon::inflight::Carrier;
use std::io;

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
