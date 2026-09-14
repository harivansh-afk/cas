//! Keep bounded queue heads pending before consuming a serial or mutation.
use super::*;
use std::time::{Duration, Instant};
use vhost_user_backend::StateChange;

const RETRY_INTERVAL: Duration = Duration::from_millis(100);

pub(super) struct Waiting {
    head: u16,
    available: u16,
    started: Instant,
    reason: Option<Reason>,
    attempts: u64,
    ticket: Option<local::host::fair::Ticket>,
}

pub(super) enum Admission {
    Waiting,
    Accepted(Permit),
}

#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    Fairness,
    Pending,
    Storage(local::pressure::Reason),
}

#[derive(Default, serde::Serialize)]
pub(super) struct Statistics {
    started: u64,
    resumed: u64,
    canceled: u64,
    finished_wait_ns: u64,
    max_finished_wait_ns: u64,
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
        if self.waiting[usize::from(queue)]
            .as_ref()
            .is_some_and(|old| old.head != head || old.available != available)
        {
            self.finish_wait(queue, false);
        }
        let waiting = &mut self.waiting[usize::from(queue)];
        if self.concurrent && waiting.is_none() {
            *waiting = Some(Waiting {
                head,
                available,
                started: now,
                reason: None,
                attempts: 0,
                ticket: self.storage.admission_ticket(request.admission_kind())?,
            });
        }
        if let Some(waiting) = waiting {
            waiting.attempts += 1;
        }
        let turn =
            if let Some(ticket) = waiting.as_ref().and_then(|waiting| waiting.ticket.as_ref()) {
                let Some(turn) = ticket.turn()? else {
                    self.record_wait(queue, Reason::Fairness, now);
                    return Ok(Admission::Waiting);
                };
                Some(turn)
            } else {
                None
            };
        use local::pressure::Decision;
        if self.pending.len() >= limit {
            drop(turn);
            self.record_wait(queue, Reason::Pending, now);
            return Ok(Admission::Waiting);
        }
        let result = self.storage.prepare(request.admission_kind())?;
        match result {
            Decision::Ready(mut permit) => {
                if let Some(turn) = turn {
                    let Permit::Local { _credits } = &mut permit else {
                        unreachable!("only shared hosts schedule admission");
                    };
                    _credits.fair_release = Some(turn.commit());
                }
                Ok(Admission::Accepted(permit))
            }
            Decision::Waiting(reason) => {
                drop(turn);
                self.record_wait(queue, Reason::Storage(reason), now);
                Ok(Admission::Waiting)
            }
        }
    }

    fn record_wait(&mut self, queue: u16, reason: Reason, now: Instant) {
        if let Some(waiting) = &mut self.waiting[usize::from(queue)] {
            if waiting.reason.is_none() {
                waiting.started = now;
                self.admission_statistics.started += 1;
            }
            waiting.reason = Some(reason);
        }
    }

    pub(super) fn finish_wait(&mut self, queue: u16, resumed: bool) {
        if let Some(waiting) = self.waiting[usize::from(queue)].take()
            && waiting.reason.is_some()
        {
            let elapsed = waiting.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            let statistics = &mut self.admission_statistics;
            statistics.resumed += u64::from(resumed);
            statistics.canceled += u64::from(!resumed);
            statistics.finished_wait_ns += elapsed;
            statistics.max_finished_wait_ns = statistics.max_finished_wait_ns.max(elapsed);
        }
    }

    pub(super) fn admission_report(&self) -> serde_json::Value {
        let now = Instant::now();
        let heads: Vec<_> = self.waiting.iter().enumerate().filter_map(|(queue, head)| {
            let head = head.as_ref()?;
            let reason = head.reason?;
            Some(serde_json::json!({
                "queue": queue, "head": head.head, "available": head.available,
                "reason": reason, "attempts": head.attempts,
                "wait_ns": now.duration_since(head.started).as_nanos().min(u64::MAX as u128) as u64,
            }))
        }).collect();
        serde_json::json!({"statistics": self.admission_statistics, "heads": heads})
    }

    pub(super) fn rearm_deadline_timer(&mut self) -> io::Result<()> {
        // Some shared metadata owners release without a frontend notification.
        // Retry the bounded queue heads, never turn elapsed pressure into IOERR.
        let retry = self
            .waiting
            .iter()
            .flatten()
            .any(|head| head.reason.is_some())
            .then(|| Instant::now() + RETRY_INTERVAL);
        let next = self
            .recovery_deadline
            .map(Deadline::instant)
            .into_iter()
            .chain(retry)
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
                for queue in 0..self.waiting.len() {
                    self.finish_wait(queue as u16, false);
                }
            }
            StateChange::QueueConfiguration(index)
            | StateChange::QueueStop(index)
            | StateChange::QueueEnable {
                index,
                enabled: false,
            } => self.finish_wait(index as u16, false),
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests;
