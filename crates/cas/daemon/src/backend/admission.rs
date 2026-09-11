//! Bound waiting before a descriptor consumes a serial or mutation.
use super::*;
use std::time::{Duration, Instant};
use vhost_user_backend::StateChange;

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy)]
pub(super) struct Waiting {
    head: u16,
    available: u16,
    deadline: Instant,
}

pub(super) enum Admission {
    Waiting,
    Rejected,
    Accepted(Permit),
}

impl Backend {
    pub(super) fn prepare_admission(
        &mut self,
        queue: u16,
        available: u16,
        request: &Request,
        limit: usize,
    ) -> io::Result<Admission> {
        let now = Instant::now();
        let head = request.completion().head;
        let waiting = &mut self.waiting[usize::from(queue)];
        if waiting.is_some_and(|old| old.head != head || old.available != available) {
            *waiting = None;
        }
        // Expiration wins even if credits became available after the deadline.
        if waiting.is_some_and(|old| now >= old.deadline) {
            return Ok(Admission::Rejected);
        }
        if self.pending.len() < limit
            && let Some(permit) = self.storage.prepare(request.admission_kind())?
        {
            return Ok(Admission::Accepted(permit));
        }
        if self.concurrent {
            waiting.get_or_insert(Waiting {
                head,
                available,
                deadline: now + ADMISSION_TIMEOUT,
            });
        }
        Ok(Admission::Waiting)
    }

    pub(super) fn rearm_deadline_timer(&mut self) -> io::Result<()> {
        let next = self
            .recovery_deadline
            .map(Deadline::instant)
            .into_iter()
            .chain(
                self.waiting
                    .iter()
                    .flatten()
                    .map(|waiting| waiting.deadline),
            )
            .min();
        let Some(timer) = &mut self.deadline_timer else {
            return Ok(());
        };
        match next {
            // Zero disarms timerfd; an expired wait instead needs an immediate wake.
            Some(next) => timer.reset(
                next.saturating_duration_since(Instant::now())
                    .max(Duration::from_nanos(1)),
                None,
            ),
            None => timer.clear(),
        }
        .map_err(io::Error::from)
    }

    pub(super) fn clear_changed_waits(&mut self, change: StateChange) {
        match change {
            StateChange::Memory | StateChange::Reset | StateChange::Attachment => {
                self.waiting.fill(None)
            }
            StateChange::QueueConfiguration(index)
            | StateChange::QueueStop(index)
            | StateChange::QueueEnable {
                index,
                enabled: false,
            } => self.waiting[index] = None,
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests;
