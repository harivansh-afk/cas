//! Conservative physical framing reservations before guest mutation admission.
use super::*;
use cas_core::budget::BudgetArc;
use std::sync::MutexGuard;

pub(super) struct Window {
    state: Mutex<State>,
    wake: OnceLock<EventFd>,
    admission: Option<host::capacity::Admission>,
}

#[derive(Clone, Copy, serde::Serialize)]
pub(super) struct State {
    point: append::Position,
    reserved_bytes: u64,
    index_capacity: usize,
    pub unsubmitted: usize,
    pub rotation_wanted: bool,
    pub index_pressure: bool,
    failed: bool,
}

impl State {
    fn used(&self) -> Option<u64> {
        let fences = self
            .point
            .issued
            .checked_sub(self.point.fenced)?
            .checked_add(self.unsubmitted as u64)?
            .checked_add(u64::from(self.point.initial_fence))?;
        self.point
            .end
            .checked_add(self.reserved_bytes)?
            .checked_add(fences.checked_mul(BLOCK_SIZE as u64)?)
    }

    fn require(&mut self, valid: bool, message: &'static str) -> io::Result<()> {
        if self.failed || !valid {
            self.failed = true;
            Err(io::Error::other(message))
        } else {
            Ok(())
        }
    }

    fn at(&mut self, log: &Log) -> io::Result<()> {
        self.require(
            self.point == log.position(),
            "WAL admission position differs",
        )
    }

    fn bounded(&mut self) -> io::Result<()> {
        self.require(
            self.used().is_some_and(|used| used <= self.point.capacity)
                && self.unsubmitted <= self.index_capacity,
            "WAL admission exceeded reserved capacity",
        )
    }
}

impl Window {
    pub fn new(
        log: &Log,
        admission: Option<host::capacity::Admission>,
        metadata: &Arc<Budget>,
    ) -> io::Result<BudgetArc<Self>> {
        let point = log.position();
        if point.capacity < (MAX_REQUEST_BYTES + 4 * BLOCK_SIZE) as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "host WAL segment cannot reserve a maximum request and initial fence",
            ));
        }
        let mut state = State {
            point,
            reserved_bytes: 0,
            index_capacity: log.append_capacity(),
            unsubmitted: 0,
            rotation_wanted: false,
            index_pressure: false,
            failed: false,
        };
        state.bounded()?;
        BudgetArc::try_new(
            Self {
                state: Mutex::new(state),
                wake: OnceLock::new(),
                admission,
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

    pub fn bind(&self, wake: EventFd) -> io::Result<()> {
        self.wake
            .set(wake)
            .map_err(|_| io::Error::other("WAL window already bound"))
    }

    fn notify(&self) {
        if let Some(wake) = self.wake.get() {
            let _ = notify(wake);
        }
    }

    pub fn status(&self) -> State {
        *self.lock()
    }

    pub fn host_pressure(&self) -> bool {
        self.admission
            .as_ref()
            .is_some_and(host::capacity::Admission::pressure)
    }

    pub fn reserve(owner: &BudgetArc<Self>, bytes: usize) -> Option<Slot> {
        if bytes > MAX_REQUEST_BYTES || !bytes.is_multiple_of(BLOCK_SIZE) {
            return None;
        }
        if owner
            .admission
            .as_ref()
            .is_some_and(|admission| !admission.admits())
        {
            owner.notify();
            return None;
        }
        let mut state = owner.lock();
        if state.failed || state.rotation_wanted {
            return None;
        }
        if state.unsubmitted >= state.index_capacity {
            state.index_pressure = true;
            drop(state);
            owner.notify();
            return None;
        }
        let bytes = (bytes + BLOCK_SIZE) as u32;
        let mut candidate = *state;
        candidate.reserved_bytes += u64::from(bytes);
        candidate.unsubmitted += 1;
        if candidate
            .used()
            .is_none_or(|used| used > candidate.point.capacity)
        {
            state.rotation_wanted = true;
            drop(state);
            owner.notify();
            return None;
        }
        *state = candidate;
        Some(Slot {
            owner: owner.clone(),
            bytes,
        })
    }

    pub fn append(
        owner: &BudgetArc<Self>,
        log: &mut Log,
        builder: Builder,
        writes: &mut [Write],
    ) -> io::Result<append::Submission> {
        let mut state = owner.lock();
        state.at(log)?;
        state.require(
            writes.len() == builder.len()
                && writes.iter().all(|write| {
                    write.permit.window.as_ref().is_some_and(|slot| {
                        slot.owner.ptr_eq(owner) && slot.matches(write.data.payload_bytes())
                    })
                }),
            "WAL batch lacks matching admission tokens",
        )?;
        let submission = log.prepare_append(builder).map_err(io::Error::other)?;
        for write in writes {
            let mut slot = write.permit.window.take().expect("checked admission token");
            state.reserved_bytes -= u64::from(slot.bytes);
            state.unsubmitted -= 1;
            slot.bytes = 0; // Consumed; Drop must not refund prepared physical IO.
        }
        state.point = log.position();
        state.index_capacity = log.append_capacity();
        state.bounded()?;
        Ok(submission)
    }

    pub fn fence(&self, log: &mut Log) -> append::Result<append::Submission> {
        let mut state = self.lock();
        state.at(log)?;
        let submission = log.prepare_fence()?;
        state.point = log.position();
        state.bounded()?;
        Ok(submission)
    }

    pub fn close(&self) {
        self.lock().rotation_wanted = true;
    }

    /// Pure sequencer refresh; an old index sample remains conservative while
    /// append publication or compaction releases capacity in the core.
    pub fn refresh(&self, log: &Log) -> io::Result<bool> {
        let mut state = self.lock();
        state.at(log)?;
        state.index_capacity = log.append_capacity();
        state.bounded()?;
        let reopened = state.index_pressure && state.unsubmitted < state.index_capacity;
        if reopened {
            state.index_pressure = false;
        }
        Ok(reopened)
    }

    pub fn before_rotation(&self, log: &Log) -> io::Result<()> {
        let mut state = self.lock();
        state.at(log)?;
        let drained = state.rotation_wanted && state.unsubmitted == 0;
        state.require(drained, "WAL rotation has unsubmitted admission tokens")
    }

    /// Called after receipt installation; sync completion otherwise leaves the
    /// tracked physical point unchanged, including the prepared fence prefix.
    pub fn installed(&self, log: &Log) -> io::Result<bool> {
        let point = log.position();
        let mut state = self.lock();
        if point.segment == state.point.segment {
            state.at(log)?;
            return Ok(false);
        }
        let valid = state.rotation_wanted
            && state.unsubmitted == 0
            && point.segment > state.point.segment
            && point.capacity == state.point.capacity
            && point.end == BLOCK_SIZE as u64
            && point.issued == state.point.issued
            && point.fenced == point.issued
            && point.initial_fence;
        state.require(valid, "WAL successor differs from admission boundary")?;
        state.point = point;
        state.index_capacity = log.append_capacity();
        state.rotation_wanted = false;
        state.bounded()?;
        Ok(true)
    }
}

/// The actual shared owner survives unused reservations; no per-request heap.
pub(super) struct Slot {
    owner: BudgetArc<Window>,
    bytes: u32,
}

impl Slot {
    pub fn matches(&self, payload: usize) -> bool {
        payload.checked_add(BLOCK_SIZE) == Some(self.bytes as usize)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        if self.bytes == 0 {
            return;
        }
        let mut state = self.owner.lock();
        state.reserved_bytes -= u64::from(self.bytes);
        state.unsubmitted -= 1;
        drop(state);
        self.owner.notify();
    }
}

#[cfg(test)]
mod tests;
