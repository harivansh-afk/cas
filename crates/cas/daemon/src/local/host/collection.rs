//! Admission and reply lifetimes do not cancel the worker's physical IO.
use super::*;
use cas_core::{budget::Lease, manifest::file::ReclaimedPages, store::file::Collected};

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct CollectionReport {
    pub pause_micros: u64,
    pub rounds: usize,
    pub compactions: usize,
    pub chunks: Collected,
    pub manifests: ReclaimedPages,
    pub allocated_before: Option<u64>,
    pub allocated_after: Option<u64>,
    pub capacity_exhausted: bool,
}

#[derive(Default, serde::Serialize)]
pub(super) struct Status {
    pub attempts: u64,
    pub completed: u64,
    pub last: Option<CollectionReport>,
    pub error: Option<String>,
}

impl SharedHost {
    pub fn collected(&self, result: &io::Result<CollectionReport>) {
        let mut status = self.collection.lock().expect("collection status poisoned");
        status.attempts = status.attempts.saturating_add(1);
        match result {
            Ok(report) => {
                status.completed = status.completed.saturating_add(1);
                status.last = Some(*report);
                status.error = None;
            }
            Err(error) => status.error = Some(error.to_string()),
        }
    }
}

pub struct CollectionHandle {
    done: mailbox::Receiver<io::Result<CollectionReport>>,
}

impl CollectionHandle {
    /// Timeout leaves the worker running. This handle may be waited on again.
    pub fn wait(&self, timeout: Duration) -> io::Result<super::CollectionReport> {
        self.done
            .recv_timeout(timeout)
            .map_err(|error| match error {
                mailbox::RecvTimeoutError::Timeout => io::ErrorKind::TimedOut.into(),
                mailbox::RecvTimeoutError::Disconnected => {
                    io::Error::other("collection owner exited")
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
            .collecting
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
        self.shared.collecting.store(false, Ordering::Release);
    }
}

pub(super) struct Request {
    done: mailbox::Sender<io::Result<CollectionReport>>,
    control: Control,
}

impl Request {
    pub fn complete(self, result: io::Result<CollectionReport>) {
        let Self { done, control } = self;
        drop(control);
        // The caller may have gone away; its cancellation cannot undo completed IO.
        let _ = done.try_send(result);
    }
}

impl Host {
    pub fn collect(&self) -> io::Result<super::CollectionHandle> {
        let control = Control::claim(&self.shared)?;
        let (done, receiver) = mailbox::bounded(1, &self.shared.resources.metadata)?;
        self.ready
            .as_ref()
            .ok_or_else(|| io::Error::other("host is shutting down"))?
            .try_send(Ready::Collect(Request { done, control }))
            .map_err(|_| io::Error::other("collection queue unavailable"))?;
        Ok(CollectionHandle { done: receiver })
    }
}

impl worker::Owner {
    pub fn collect(&mut self) -> io::Result<CollectionReport> {
        let started = Instant::now();
        let mut pause = admission::Admission::pause(&self.shared.admission)?;
        let generation = pause.generation();
        let mut uncertain = false;
        let result = (|| {
            self.drain_guests(&pause, started)?;
            for (index, endpoint) in self.endpoints.iter_mut().enumerate() {
                endpoint.healthy()?;
                if !self.shared.admission.attached(index) {
                    continue;
                }
                uncertain = true;
                match endpoint.exchange(Event::Quiesce(generation)) {
                    Ok(Reply::Quiesced(actual)) if actual == generation => {
                        uncertain = false;
                        endpoint.quiescent = Some(generation);
                    }
                    Ok(Reply::Deferred) => {
                        uncertain = false;
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                    Err(_) if !self.shared.admission.attached(index) => {
                        endpoint.healthy()?;
                        uncertain = false;
                    }
                    Err(error) => return Err(error),
                    _ => return Err(io::Error::other("collection drain acknowledgment differs")),
                }
            }
            self.drain_notifications()?;
            pause.begin()?;
            #[cfg(test)]
            {
                let blocked = self.shared.control.lock().unwrap().collection.take();
                if let Some(blocked) = blocked {
                    blocked.wait();
                }
            }
            let mut report = CollectionReport {
                allocated_before: self.shared.physical.as_ref().map(|p| p.status().allocated),
                ..CollectionReport::default()
            };
            loop {
                if started.elapsed() >= IO_DEADLINE {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "collection deadline expired",
                    ));
                }
                self.sweep(&mut report)?;
                report.rounds += 1;
                if !self.shared.collection_required() {
                    break;
                }
                let mut advanced = false;
                for endpoint in &mut self.endpoints {
                    if endpoint.quiescent != Some(generation) {
                        continue;
                    }
                    let before = endpoint.manifest.current();
                    endpoint.compact(
                        &mut self.store,
                        self.shared.physical.as_ref(),
                        Some(generation),
                    )?;
                    if endpoint.manifest.current() != before {
                        report.compactions += 1;
                        advanced = true;
                        break; // One output, then sweep before another output.
                    }
                }
                if !advanced {
                    report.capacity_exhausted = true;
                    break;
                }
            }
            report.allocated_after = self.shared.physical.as_ref().map(|p| p.status().allocated);
            Ok(report)
        })();
        match result {
            Ok(mut report) => {
                if let Err(error) = self.resume_collection(generation) {
                    self.shared.gate.fail(error.to_string());
                    pause.fail();
                    return Err(error);
                }
                pause.finish()?;
                report.pause_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
                Ok(report)
            }
            Err(error) => {
                if uncertain || self.shared.admission.status().running {
                    // Publish failure before any parked lifecycle work can resume.
                    self.shared.gate.fail(error.to_string());
                    pause.fail();
                } else if let Err(resume) = self
                    .drain_notifications()
                    .and_then(|()| self.resume_collection(generation))
                {
                    self.shared.gate.fail(resume.to_string());
                    pause.fail();
                }
                Err(error)
            }
        }
    }

    fn drain_guests(&mut self, pause: &Quiescence, started: Instant) -> io::Result<()> {
        while !pause.drained() {
            if started.elapsed() >= IO_DEADLINE {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "accepted guest owners did not drain",
                ));
            }
            match self.input.recv_timeout(Duration::from_millis(10)) {
                Ok(Ready::Image { index, .. }) => self.cancel_ready(index)?,
                Ok(Ready::Collect(request)) => {
                    request.complete(Err(io::ErrorKind::WouldBlock.into()))
                }
                Err(mailbox::RecvTimeoutError::Timeout) => (),
                Err(mailbox::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("host closed during collection"));
                }
            }
        }
        self.drain_notifications()
    }

    fn cancel_ready(&self, index: usize) -> io::Result<()> {
        let endpoint = &self.endpoints[index];
        if !self.shared.admission.attached(index) {
            return endpoint.healthy();
        }
        match endpoint.defer(None) {
            Err(_) if !self.shared.admission.attached(index) => endpoint.healthy(),
            result => result,
        }
    }

    fn drain_notifications(&mut self) -> io::Result<()> {
        while let Ok(ready) = self.input.try_recv() {
            match ready {
                Ready::Image { index, .. } if self.endpoints[index].quiescent.is_none() => {
                    self.cancel_ready(index)?;
                }
                Ready::Image { .. } => (), // Ack cancelled this unstarted turn.
                Ready::Collect(request) => request.complete(Err(io::ErrorKind::WouldBlock.into())),
            }
        }
        Ok(())
    }

    fn resume_collection(&mut self, generation: u64) -> io::Result<()> {
        for (index, endpoint) in self.endpoints.iter_mut().enumerate() {
            if endpoint.quiescent.take() != Some(generation) {
                continue;
            }
            match endpoint.exchange(Event::Resume(generation)) {
                Ok(Reply::Applied) => (),
                Err(_) if !self.shared.admission.attached(index) => endpoint.healthy()?,
                Err(error) => return Err(error),
                _ => return Err(io::Error::other("collection resume acknowledgment differs")),
            }
        }
        Ok(())
    }

    fn sweep(&mut self, report: &mut CollectionReport) -> io::Result<()> {
        let metadata = &self.shared.resources.compaction;
        let mut collection = self.store.begin_collection()?;
        for endpoint in &mut self.endpoints {
            endpoint.healthy()?;
            endpoint
                .manifest
                .pinned_roots(Arc::clone(metadata))?
                .walk(|extent| extent.hash.map_or(Ok(()), |hash| collection.mark(&hash)))?;
        }
        for snapshot in &mut self.snapshots {
            snapshot
                .pinned_roots(Arc::clone(metadata))?
                .walk(|extent| extent.hash.map_or(Ok(()), |hash| collection.mark(&hash)))?;
        }
        let mut sweep = collection.finish_marking()?;
        while let Some(victim) = sweep.next_victim() {
            physical(
                &self.shared,
                victim.destination_bytes + capacity::METADATA_MARGIN,
                || sweep.clean_next().map(|_| ()),
            )?;
        }
        let chunks = sweep.finish()?;
        report.chunks.segments_removed += chunks.segments_removed;
        report.chunks.headers_retained += chunks.headers_retained;
        report.chunks.chunks_copied += chunks.chunks_copied;
        report.chunks.encoded_bytes_copied += chunks.encoded_bytes_copied;
        for endpoint in &mut self.endpoints {
            let pages = physical(&self.shared, capacity::METADATA_MARGIN, || {
                endpoint.manifest.reclaim_pages(Arc::clone(metadata))
            })?;
            add_pages(&mut report.manifests, pages);
        }
        for snapshot in &mut self.snapshots {
            let pages = physical(&self.shared, capacity::METADATA_MARGIN, || {
                snapshot.reclaim_pages(Arc::clone(metadata))
            })?;
            add_pages(&mut report.manifests, pages);
        }
        Ok(())
    }
}

fn physical<T>(
    shared: &SharedHost,
    bytes: u64,
    operation: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    match &shared.physical {
        Some(physical) => physical.background(bytes)?.run(operation),
        None => operation(),
    }
}

fn add_pages(total: &mut ReclaimedPages, pages: ReclaimedPages) {
    total.roots += pages.roots;
    total.file_bytes += pages.file_bytes;
    total.windows += pages.windows;
    total.tree_page_reads += pages.tree_page_reads;
    total.retained_pages += pages.retained_pages;
    total.punch_calls += pages.punch_calls;
    total.punched_logical_bytes += pages.punched_logical_bytes;
    total.removed_tail_mapping_bytes += pages.removed_tail_mapping_bytes;
}
