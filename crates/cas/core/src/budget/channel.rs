//! Fixed storage and shared control state are charged before endpoints escape.
use super::{Budget, BudgetArc, Queue as Ring};
use std::{
    io,
    sync::{Arc, Condvar, Mutex, MutexGuard},
    time::{Duration, Instant},
};

pub use std::sync::mpsc::{RecvError, RecvTimeoutError, TryRecvError, TrySendError};

pub struct Sender<T>(BudgetArc<State<T>>);
pub struct Receiver<T>(BudgetArc<State<T>>);

struct State<T> {
    queue: Mutex<Queue<T>>,
    ready: Condvar,
}

struct Queue<T> {
    messages: Ring<T>,
    senders: usize,
    closed: bool,
    failed: bool,
}

pub fn bounded<T>(capacity: usize, budget: &Arc<Budget>) -> io::Result<(Sender<T>, Receiver<T>)> {
    let messages = Ring::with_capacity(capacity, budget)?;
    let state = BudgetArc::try_new(
        State {
            queue: Mutex::new(Queue {
                messages,
                senders: 1,
                closed: false,
                failed: false,
            }),
            ready: Condvar::new(),
        },
        budget,
    )?;
    Ok((Sender(state.clone()), Receiver(state)))
}

impl<T> State<T> {
    fn lock(&self) -> MutexGuard<'_, Queue<T>> {
        self.queue.lock().unwrap_or_else(|error| {
            let mut queue = error.into_inner();
            self.fail(&mut queue);
            queue
        })
    }

    fn fail(&self, queue: &mut Queue<T>) {
        queue.failed = true;
        queue.closed = true;
        self.ready.notify_all();
    }
}

impl<T> Queue<T> {
    fn receive(&mut self) -> Result<T, TryRecvError> {
        if self.failed {
            return Err(TryRecvError::Disconnected);
        }
        if let Some(value) = self.messages.pop_front() {
            return Ok(value);
        }
        Err(if self.closed || self.senders == 0 {
            TryRecvError::Disconnected
        } else {
            TryRecvError::Empty
        })
    }
}

impl<T> Sender<T> {
    pub fn try_send(&self, value: T) -> Result<(), TrySendError<T>> {
        let mut queue = self.0.lock();
        if queue.closed {
            return Err(TrySendError::Disconnected(value));
        }
        queue
            .messages
            .try_push_back(value)
            .map_err(TrySendError::Full)?;
        drop(queue);
        self.0.ready.notify_one();
        Ok(())
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut queue = self.0.lock();
        queue.senders = queue.senders.checked_add(1).expect("sender count overflow");
        Self(self.0.clone())
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut queue = self.0.lock();
        queue.senders -= 1;
        let last = queue.senders == 0;
        drop(queue);
        if last {
            self.0.ready.notify_all();
        }
    }
}

impl<T> Receiver<T> {
    /// Reject new publication, retaining already accepted messages for draining.
    pub fn close(&self) {
        self.0.lock().closed = true;
        self.0.ready.notify_all();
    }

    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        self.0.lock().receive()
    }

    pub fn recv(&self) -> Result<T, RecvError> {
        self.receive(None).map_err(|_| RecvError)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<T, RecvTimeoutError> {
        // A duration beyond Instant's range cannot expire during this process.
        self.receive(Instant::now().checked_add(timeout))
    }

    fn receive(&self, deadline: Option<Instant>) -> Result<T, RecvTimeoutError> {
        let mut queue = self.0.lock();
        loop {
            match queue.receive() {
                Ok(value) => return Ok(value),
                Err(TryRecvError::Disconnected) => return Err(RecvTimeoutError::Disconnected),
                Err(TryRecvError::Empty) => {}
            }
            queue = if let Some(deadline) = deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(RecvTimeoutError::Timeout);
                }
                self.0
                    .ready
                    .wait_timeout(queue, remaining)
                    .unwrap_or_else(|error| {
                        let (mut queue, timeout) = error.into_inner();
                        self.0.fail(&mut queue);
                        (queue, timeout)
                    })
                    .0
            } else {
                self.0.ready.wait(queue).unwrap_or_else(|error| {
                    let mut queue = error.into_inner();
                    self.0.fail(&mut queue);
                    queue
                })
            };
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        self.close();
        loop {
            let value = self.0.lock().messages.pop_front();
            match value {
                Some(value) => drop(value), // Message owners never drop under the queue lock.
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests;
