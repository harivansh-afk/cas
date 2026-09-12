//! One budgeted administrative operation keeps its credit through actual IO.
use super::*;
use cas_core::budget::Lease;

pub struct Handle<T> {
    done: mailbox::Receiver<io::Result<T>>,
}

impl<T> Handle<T> {
    /// A timeout leaves the owner running; this handle may be waited on again.
    pub fn wait(&self, timeout: Duration) -> io::Result<T> {
        self.done
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mailbox::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut.into(),
                mailbox::RecvTimeoutError::Disconnected => {
                    io::Error::other("administrative owner exited")
                }
            })?
    }
}

pub(super) struct Control {
    shared: Arc<SharedHost>,
    _credit: Lease,
}

impl Control {
    pub fn claim(shared: &Arc<SharedHost>) -> io::Result<Self> {
        let credit = shared
            .resources
            .pools
            .administrative()
            .ok_or(io::ErrorKind::WouldBlock)?;
        shared
            .administrating
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        Ok(Self {
            shared: Arc::clone(shared),
            _credit: credit,
        })
    }
}
impl Drop for Control {
    fn drop(&mut self) {
        self.shared.administrating.store(false, Ordering::Release);
    }
}

pub(super) struct Request<T> {
    done: mailbox::Sender<io::Result<T>>,
    control: Control,
}
impl<T> Request<T> {
    pub fn new(shared: &Arc<SharedHost>) -> io::Result<(Self, Handle<T>)> {
        let control = Control::claim(shared)?;
        let (done, receiver) = mailbox::bounded(1, &shared.resources.metadata)?;
        Ok((Self { done, control }, Handle { done: receiver }))
    }

    pub fn complete(self, result: io::Result<T>) {
        let Self { done, control } = self;
        drop(control);
        // Canceling the waiter never cancels output owned by the worker.
        let _ = done.try_send(result);
    }
}
