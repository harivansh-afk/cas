//! Monotonic recovery bounds without releasing a blocked task's owned IO.
use std::io;
use std::os::fd::{FromRawFd, RawFd};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use vmm_sys_util::timerfd::TimerFd;

pub const RECOVERY_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug)]
pub struct Deadline(Instant);

impl Deadline {
    pub fn after(duration: Duration) -> Self {
        Self(Instant::now() + duration)
    }

    pub fn remaining(self) -> io::Result<Duration> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "recovery deadline expired"))
    }

    pub fn check(self) -> io::Result<()> {
        self.remaining().map(|_| ())
    }

    pub fn run<T: Send + 'static>(
        self,
        work: impl FnOnce() -> io::Result<T> + Send + 'static,
    ) -> io::Result<T> {
        self.check()?;
        let (done, result) = mpsc::sync_channel(1);
        // Detach on timeout: the task owns every file/buffer/map it may still
        // access. Its eventual result is dropped if the waiter has gone away.
        thread::Builder::new()
            .name("cas-recovery".into())
            .spawn(move || {
                let value = work();
                let _ = done.send(self.check().and(value));
            })?;
        result.recv_timeout(self.remaining()?).map_err(|error| {
            io::Error::new(
                match error {
                    mpsc::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut,
                    mpsc::RecvTimeoutError::Disconnected => io::ErrorKind::BrokenPipe,
                },
                format!("recovery task did not complete: {error}"),
            )
        })?
    }

    pub fn wait_readable(self, fd: RawFd) -> io::Result<()> {
        loop {
            let remaining = self.remaining()?;
            let mut event = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: the caller owns fd and the pointer covers one pollfd.
            let ready = unsafe {
                libc::poll(
                    &mut event,
                    1,
                    remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
                )
            };
            if ready < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if ready > 0 {
                if event.revents & libc::POLLIN != 0 {
                    return self.check();
                }
                return Err(io::Error::other(
                    "recovery listener closed before connection",
                ));
            }
        }
    }

    pub fn instant(self) -> Instant {
        self.0
    }

    pub fn timer(self) -> io::Result<TimerFd> {
        let mut timer = timer()?;
        timer
            .reset(self.remaining()?, None)
            .map_err(io::Error::from)?;
        Ok(timer)
    }
}

pub fn timer() -> io::Result<TimerFd> {
    // SAFETY: creates a new owned descriptor; no pointers are passed.
    let fd = unsafe {
        libc::timerfd_create(
            libc::CLOCK_MONOTONIC,
            libc::TFD_CLOEXEC | libc::TFD_NONBLOCK,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is newly created and its ownership moves into TimerFd.
    Ok(unsafe { TimerFd::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::sync::{Arc, Weak};

    #[test]
    fn timeout_keeps_a_tasks_actual_file_lock_and_memory_alive() {
        let file = tempfile::tempfile().unwrap();
        file.try_lock().unwrap();
        let path = format!("/proc/self/fd/{}", file.as_raw_fd());
        let contender = File::open(path).unwrap();
        let memory = Arc::new([0x55u8; 4096]);
        let weak: Weak<[u8; 4096]> = Arc::downgrade(&memory);
        let (release, resume) = mpsc::channel();
        let (exited, joined) = mpsc::channel();
        let result = Deadline::after(Duration::from_millis(30)).run(move || {
            resume.recv().unwrap();
            assert_eq!(memory[0], 0x55);
            drop((file, memory));
            exited.send(()).unwrap();
            Ok(())
        });
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(weak.upgrade().is_some());
        assert!(matches!(
            contender.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        release.send(()).unwrap();
        joined.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(weak.upgrade().is_none());
        contender.try_lock().unwrap();
    }

    #[test]
    fn timer_and_listener_wait_share_the_same_deadline() {
        let deadline = Deadline::after(Duration::from_millis(30));
        let mut timer = deadline.timer().unwrap();
        assert_eq!(
            io::Error::from(timer.wait().unwrap_err()).kind(),
            io::ErrorKind::WouldBlock
        );
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(
            deadline
                .wait_readable(reader.as_raw_fd())
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(timer.wait().unwrap(), 1);
        assert!(deadline.run(|| Ok(())).is_err());
    }
}
