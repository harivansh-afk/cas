//! The stop/join protocol shared by the GET transport worker, the race
//! deadline thread and the local race source: a nonblocking `shutdown` that
//! joins only a finished thread (the embedding host retries `WouldBlock`), and
//! a blocking `join` for `Drop`.
use std::{
    io,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

pub(crate) struct Stoppable {
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl Stoppable {
    pub fn spawn(name: &str, body: impl FnOnce() + Send + 'static) -> io::Result<Self> {
        let handle = thread::Builder::new().name(name.into()).spawn(body)?;
        Ok(Self {
            handle: Mutex::new(Some(handle)),
        })
    }

    /// Request the stop, wake the thread, and join it only if it has exited;
    /// otherwise `WouldBlock` so the caller's shutdown stays nonblocking.
    pub fn shutdown(
        &self,
        stop: &AtomicBool,
        wake: impl FnOnce(),
        what: &'static str,
    ) -> io::Result<()> {
        stop.store(true, Ordering::Release);
        wake();
        let mut handle = self.handle.lock().expect("worker handle poisoned");
        if handle.as_ref().is_some_and(|h| !h.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(handle) = handle.take() {
            handle
                .join()
                .map_err(|_| io::Error::other(format!("{what} panicked")))?;
        }
        Ok(())
    }

    /// Blocking stop for `Drop`: request, wake, join.
    pub fn join(&mut self, stop: &AtomicBool, wake: impl FnOnce()) {
        stop.store(true, Ordering::Release);
        wake();
        if let Some(handle) = self
            .handle
            .get_mut()
            .expect("worker handle poisoned")
            .take()
        {
            let _ = handle.join();
        }
    }
}
