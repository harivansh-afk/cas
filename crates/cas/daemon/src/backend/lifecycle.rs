//! Quiesce old-state users across a frontend configuration transaction.
use super::*;
use std::time::Instant;
use vhost_user_backend::StateChange;

impl Backend {
    pub(super) fn begin_change(
        &mut self,
        change: StateChange,
        vrings: &[VringMutex],
    ) -> io::Result<()> {
        if !self.concurrent {
            return Ok(());
        }
        self.paused = true;
        let deadline = Instant::now() + local::IO_DEADLINE;
        self.change_deadline = Some(deadline);
        let result = (|| {
            if self
                .live
                .as_ref()
                .is_some_and(|session| !session.permits_change(change))
            {
                return Err(io::Error::other(
                    "frontend configuration changed during shared recovery",
                ));
            }
            if let Some(deadline) = self.recovery_deadline {
                deadline.check()?;
            }
            let index = match change {
                StateChange::QueueConfiguration(index)
                | StateChange::QueueNotification(index)
                | StateChange::QueueStop(index)
                | StateChange::QueueEnable { index, .. } => Some(index),
                _ => None,
            };
            if index.is_some_and(|index| index >= vrings.len() || index >= CONCURRENT_QUEUES) {
                return Err(io::Error::other("state change has an invalid queue"));
            }
            if let Some(error) = &self.failure {
                return Err(io::Error::other(error.clone()));
            }
            self.storage.submit()?;
            if !self.pending.is_empty() {
                let memory = self
                    .memory
                    .clone()
                    .ok_or_else(|| io::Error::other("pending IO has no accepted memory"))?;
                while !self.pending.is_empty() {
                    self.complete(&memory, vrings)?;
                    if !self.pending.is_empty() {
                        self.wait_for_completion(deadline)?;
                    }
                }
            }
            if let Storage::Local(local) = &mut self.storage {
                local.pause(deadline)?;
            }
            Ok(())
        })()
        .map_err(|error: io::Error| {
            io::Error::new(error.kind(), format!("frontend {change:?}: {error}"))
        });
        if let Err(error) = &result {
            self.fail(error.to_string());
            self.fail_pending(vrings);
        }
        result
    }

    fn wait_for_completion(&self, deadline: Instant) -> io::Result<()> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "guest IO drain deadline expired",
            ));
        }
        let mut event = libc::pollfd {
            fd: self.completion_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: the owned eventfd stays live and the pointer covers one pollfd.
        let result = unsafe { libc::poll(&mut event, 1, remaining.as_millis().min(50) as i32) };
        if result < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
        match self.completion_event.read() {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
            Err(error) => Err(error),
        }
    }

    pub(super) fn end_change(
        &mut self,
        change: StateChange,
        succeeded: bool,
        vrings: &[VringMutex],
    ) -> io::Result<()> {
        if !self.concurrent {
            return Ok(());
        }
        let result = (|| {
            if !succeeded {
                return Err(io::Error::other("frontend state change failed"));
            }
            if let Some(error) = &self.failure {
                return Err(io::Error::other(error.clone()));
            }
            let serving = self
                .live
                .as_ref()
                .map_or(self.next_id != 0, Session::active);
            match change {
                StateChange::QueueConfiguration(index) if serving => {
                    self.blocked_queues[index] = true;
                    self.rebase_queues[index] = true;
                }
                StateChange::QueueStop(index) => {
                    self.blocked_queues[index] = true;
                    self.rebase_queues[index] = serving;
                }
                StateChange::QueueEnable { index, enabled } => {
                    self.rebase_queues[index] |=
                        enabled && self.live.as_ref().is_some_and(Session::fresh);
                    self.blocked_queues[index] = !enabled || self.rebase_queues[index];
                }
                StateChange::Reset => {
                    self.blocked_queues.fill(true);
                    self.rebase_queues.fill(serving);
                    self.negotiated_features = 0;
                }
                _ => (),
            }
            // Enable does not imply that addresses and the kick FD are ready.
            // Any later setup message may finish the pending rebase.
            let configured = match change {
                StateChange::QueueConfiguration(index)
                | StateChange::QueueNotification(index)
                | StateChange::QueueEnable {
                    index,
                    enabled: true,
                } => Some(index),
                _ => None,
            };
            if let Some(index) = configured
                && self.rebase_queues[index]
                && self.configuration_ready(&vrings[index])?
            {
                self.rebase_queue(index, &vrings[index])?;
                self.blocked_queues[index] = false;
                self.rebase_queues[index] = false;
            }
            self.admission.changed(change);
            if let Some(observer) = &mut self.read_trace {
                match change {
                    StateChange::Memory | StateChange::Reset | StateChange::Attachment => {
                        observer.reset(None)
                    }
                    StateChange::QueueConfiguration(index)
                    | StateChange::QueueStop(index)
                    | StateChange::QueueEnable {
                        index,
                        enabled: false,
                    } => observer.reset(Some(index)),
                    _ => (),
                }
            }
            self.rearm_deadline_timer()?;
            self.validate_used_cursors(vrings)?;
            if let Storage::Local(local) = &mut self.storage {
                local.resume()?;
            }
            self.paused = false;
            self.change_deadline = None;
            // A queue kick may have been consumed while admission was paused.
            if self.memory.is_some()
                && self.negotiated_features & REQUIRED_FEATURES == REQUIRED_FEATURES
                && vrings.iter().enumerate().any(|(index, vring)| {
                    let state = vring.get_ref();
                    !self.blocked_queues[index] && state.is_enabled() && state.get_queue().ready()
                })
            {
                local::notify(&self.completion_event)?;
            }
            Ok(())
        })()
        .map_err(|error: io::Error| {
            io::Error::new(error.kind(), format!("frontend {change:?}: {error}"))
        });
        if let Err(error) = &result {
            self.paused = true;
            self.fail(error.to_string());
            self.fail_pending(vrings);
        }
        result
    }

    fn rebase_queue(&mut self, index: usize, vring: &VringMutex) -> io::Result<()> {
        let memory = self
            .memory
            .as_ref()
            .ok_or_else(|| io::Error::other("queue restart has no memory"))?;
        let mut queue = vring.get_mut();
        let used = queue
            .get_queue()
            .used_idx(&**memory, Ordering::Acquire)
            .map_err(|error| {
                io::Error::other(format!(
                    "rebase queue {index}, state {:?}: {error}",
                    queue.get_queue().state()
                ))
            })?
            .0;
        if let Some(gate) = self.storage.completion_gate() {
            let mut health = gate
                .lock()
                .map_err(|_| io::Error::other("completion gate poisoned"))?;
            if let Some(carrier) = &mut health.carrier {
                if queue.get_queue().size() != carrier.geometry().message().queue_size {
                    return Err(io::Error::other("resized queue needs a fresh carrier"));
                }
                if carrier.queue_initialized(index as u16)? {
                    carrier.reset_queue(index as u16, queue.get_queue().next_avail(), used)?;
                }
            }
        }
        queue.get_queue_mut().set_next_used(used);
        Ok(())
    }

    fn configuration_ready(&self, vring: &VringMutex) -> io::Result<bool> {
        let Some(memory) = &self.memory else {
            return Ok(false);
        };
        let state = vring.get_ref();
        let queue = state.get_queue();
        if !state.is_enabled() || !queue.ready() || !queue.is_valid(&**memory) {
            return Ok(false);
        }
        let available = queue
            .avail_idx(&**memory, Ordering::Acquire)
            .map_err(io::Error::other)?
            .0;
        Ok(available.wrapping_sub(queue.next_avail()) <= queue.size())
    }

    fn validate_used_cursors(&self, vrings: &[VringMutex]) -> io::Result<()> {
        let Some(memory) = &self.memory else {
            return Ok(());
        };
        for (index, vring) in vrings.iter().enumerate() {
            let queue = vring.get_ref();
            if self.blocked_queues[index] || !queue.is_enabled() || !queue.get_queue().ready() {
                continue;
            }
            let used = queue
                .get_queue()
                .used_idx(&**memory, Ordering::Acquire)
                .map_err(|error| {
                    io::Error::other(format!(
                        "validate queue {index}, state {:?}: {error}",
                        queue.get_queue().state()
                    ))
                })?
                .0;
            if used != queue.get_queue().next_used() {
                return Err(io::Error::other(
                    "memory replacement changed an active used cursor",
                ));
            }
        }
        Ok(())
    }
}
