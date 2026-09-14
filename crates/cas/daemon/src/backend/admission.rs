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

#[derive(Default, Clone, Copy, serde::Serialize)]
pub(super) struct Statistics {
    started: u64,
    resumed: u64,
    canceled: u64,
    finished_wait_ns: u64,
    max_finished_wait_ns: u64,
}

/// Owns each observed head and its scheduler ticket through retirement.
#[derive(Default)]
pub(super) struct QueueAdmission {
    heads: [Option<Waiting>; CONCURRENT_QUEUES],
    statistics: Statistics,
}

#[derive(serde::Serialize)]
struct Head {
    queue: usize,
    head: u16,
    available: u16,
    reason: Reason,
    attempts: u64,
    wait_ns: u64,
}

struct Heads([Option<Head>; CONCURRENT_QUEUES]);
impl serde::Serialize for Heads {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.iter().flatten().count()))?;
        for head in self.0.iter().flatten() {
            seq.serialize_element(head)?;
        }
        seq.end()
    }
}

#[derive(serde::Serialize)]
pub(super) struct Report {
    statistics: Statistics,
    heads: Heads,
}

impl QueueAdmission {
    pub(super) fn cancel_matching(&mut self, queue: u16, available: u16, head: u16) {
        if self.heads[usize::from(queue)]
            .as_ref()
            .is_some_and(|waiting| waiting.available == available && waiting.head == head)
        {
            self.finish_wait(queue, false);
        }
    }

    pub(super) fn reason_index(&self, queue: u16) -> usize {
        match self.heads[usize::from(queue)]
            .as_ref()
            .and_then(|h| h.reason)
        {
            Some(Reason::Fairness) => 0,
            Some(Reason::Pending) => 1,
            Some(Reason::Storage(reason)) => 2 + reason as usize,
            None => unreachable!("waiting admission must have a reason"),
        }
    }

    pub(super) fn prepare_admission(
        &mut self,
        queue: u16,
        available: u16,
        request: &Request,
        storage: &mut Storage,
        concurrent: bool,
        pending_full: bool,
    ) -> io::Result<Admission> {
        let now = Instant::now();
        let head = request.completion().head;
        if self.heads[usize::from(queue)]
            .as_ref()
            .is_some_and(|old| old.head != head || old.available != available)
        {
            self.finish_wait(queue, false);
        }
        let waiting = &mut self.heads[usize::from(queue)];
        if concurrent && waiting.is_none() {
            *waiting = Some(Waiting {
                head,
                available,
                started: now,
                reason: None,
                attempts: 0,
                ticket: storage.admission_ticket(request.admission_kind())?,
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
        if pending_full {
            drop(turn);
            self.record_wait(queue, Reason::Pending, now);
            return Ok(Admission::Waiting);
        }
        let result = storage.prepare(request.admission_kind())?;
        match result {
            Decision::Ready(mut permit) => {
                if let Some(turn) = turn {
                    let Permit::Local { credits } = &mut permit else {
                        unreachable!("only shared hosts schedule admission");
                    };
                    credits.fair_release = Some(turn.commit());
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
        if let Some(waiting) = &mut self.heads[usize::from(queue)] {
            if waiting.reason.is_none() {
                waiting.started = now;
                self.statistics.started += 1;
            }
            waiting.reason = Some(reason);
        }
    }

    pub(super) fn finish_wait(&mut self, queue: u16, resumed: bool) {
        if let Some(waiting) = self.heads[usize::from(queue)].take()
            && waiting.reason.is_some()
        {
            let elapsed = waiting.started.elapsed().as_nanos().min(u64::MAX as u128) as u64;
            let statistics = &mut self.statistics;
            statistics.resumed += u64::from(resumed);
            statistics.canceled += u64::from(!resumed);
            statistics.finished_wait_ns += elapsed;
            statistics.max_finished_wait_ns = statistics.max_finished_wait_ns.max(elapsed);
        }
    }

    pub(super) fn snapshot(&self) -> Report {
        let now = Instant::now();
        Report {
            statistics: self.statistics,
            heads: Heads(std::array::from_fn(|queue| {
                let head = self.heads[queue].as_ref()?;
                Some(Head {
                    queue,
                    head: head.head,
                    available: head.available,
                    reason: head.reason?,
                    attempts: head.attempts,
                    wait_ns: now
                        .duration_since(head.started)
                        .as_nanos()
                        .min(u64::MAX as u128) as u64,
                })
            })),
        }
    }

    pub(super) fn retry_at(&self) -> Option<Instant> {
        self.heads
            .iter()
            .flatten()
            .any(|head| head.reason.is_some())
            .then(|| Instant::now() + RETRY_INTERVAL)
    }

    pub(super) fn cancel_all(&mut self) {
        for queue in 0..self.heads.len() {
            self.finish_wait(queue as u16, false);
        }
    }

    pub(super) fn changed(&mut self, change: StateChange) {
        match change {
            StateChange::Memory | StateChange::Reset | StateChange::Attachment => self.cancel_all(),
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

impl Backend {
    pub(super) fn prepare_admission(
        &mut self,
        queue: u16,
        available: u16,
        request: &Request,
        limit: usize,
    ) -> io::Result<Admission> {
        self.admission.prepare_admission(
            queue,
            available,
            request,
            &mut self.storage,
            self.concurrent,
            self.pending.len() >= limit,
        )
    }

    #[cfg(test)]
    fn admission_report(&self) -> serde_json::Value {
        serde_json::to_value(self.admission.snapshot()).unwrap()
    }

    pub(super) fn rearm_deadline_timer(&mut self) -> io::Result<()> {
        // Some shared metadata owners release without a frontend notification.
        // Retry the bounded queue heads, never turn elapsed pressure into IOERR.
        let retry = self
            .admission
            .retry_at()
            .into_iter()
            .chain(
                self.frontier
                    .as_ref()
                    .and_then(|frontier| frontier.retry_at()),
            )
            .min();
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
}

#[cfg(test)]
mod tests;
