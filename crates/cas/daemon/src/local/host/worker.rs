use super::*;

enum Grant {
    Ready(Option<cas_core::space::Permit>),
    Deferred,
}

pub(super) struct Endpoint {
    pub statistics: BudgetArc<Mutex<statistics::Totals>>,
    pub manifest: Manifest,
    pub resources: Arc<Resources>,
    pub quiescent: Option<u64>,
    #[cfg(test)]
    pub control: Arc<Mutex<tests::Control>>,
    pub health: Health,
    pub wake: EventFd,
    pub output: mailbox::Sender<Event>,
    pub replies: mailbox::Receiver<Reply>,
}

impl Endpoint {
    pub(super) fn healthy(&self) -> io::Result<()> {
        if let Some(error) = &self.health.lock()?.failure {
            return Err(io::Error::other(error.clone()));
        }
        Ok(())
    }

    pub(super) fn exchange(&self, event: Event) -> io::Result<Reply> {
        self.healthy()?;
        self.output
            .try_send(event)
            .map_err(|_| io::Error::other("image compaction receiver unavailable"))?;
        notify(&self.wake)?;
        let reply = self.replies.recv_timeout(IO_DEADLINE).map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "image compaction acknowledgment unavailable",
            )
        })?;
        if let Reply::Failed(error) = reply {
            return Err(io::Error::other(error));
        }
        Ok(reply)
    }

    pub(super) fn compact(
        &mut self,
        store: &mut Store,
        physical: Option<&Arc<cas_core::space::Governor>>,
        generation: Option<u64>,
    ) -> io::Result<()> {
        let event = generation.map_or(Event::Select, Event::CompactQuiescent);
        match self.exchange(event)? {
            Reply::Selected(None) => Ok(()),
            Reply::Selected(Some(selection)) => {
                let mut attempt = statistics::Attempt::new(&self.statistics);
                self.healthy()?;
                let input = selection.load()?;
                attempt.advance(statistics::Phase::Prepare);
                let input_bytes = input.payload_bytes() as u64;
                let prepared = input.prepare(&self.manifest)?;
                attempt.advance(statistics::Phase::Reserve);
                let output_bytes = (prepared.chunk_count() * BLOCK_SIZE) as u64;
                let bytes = capacity::compaction_bytes(&prepared, store.config().segment_bytes);
                let permit = match self.background(physical, bytes)? {
                    Grant::Ready(permit) => permit,
                    Grant::Deferred => {
                        attempt.deferred();
                        return Ok(());
                    }
                };
                attempt.advance(statistics::Phase::Chunks);
                #[cfg(test)]
                {
                    let pause = self.control.lock().unwrap().compaction.take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                self.healthy()?;
                let through = prepared.through();
                let chunks = Some(prepared.chunk_count());
                let image = self.manifest.current().image;
                let resources = Arc::clone(&self.resources);
                let health = self.health.clone();
                let output = || {
                    let receipt =
                        prepared.write_with(store, &mut self.manifest, |stage, durable| {
                            use append::Publication;
                            match stage {
                                Publication::AfterChunks => {
                                    attempt.advance(statistics::Phase::Manifest)
                                }
                                Publication::AfterManifest => {
                                    attempt.advance(statistics::Phase::Reclaim)
                                }
                                Publication::BeforeChunks => (),
                            }
                            let point = match stage {
                                Publication::BeforeChunks => fault::Point::BeforeChunks,
                                Publication::AfterChunks => fault::Point::AfterChunks,
                                Publication::AfterManifest => fault::Point::AfterManifest,
                            };
                            resources.hit_compaction(
                                point,
                                fault::Observation {
                                    image,
                                    through,
                                    manifest_durable: durable,
                                    chunks,
                                    operation: None,
                                },
                                &health,
                            )
                        })?;
                    let Reply::Reclaim(reclaim) = self.exchange(Event::Published(receipt))? else {
                        return Err(io::Error::other("expected reclamation after D publication"));
                    };
                    self.resources.hit_compaction(
                        fault::Point::AfterD,
                        fault::Observation {
                            image,
                            through,
                            manifest_durable: self.manifest.current().durable,
                            chunks,
                            operation: None,
                        },
                        &self.health,
                    )?;
                    self.reclaim(reclaim)
                };
                match permit {
                    Some(permit) => permit.run(output),
                    None => output(),
                }?;
                attempt.completed(input_bytes, output_bytes);
                Ok(())
            }
            Reply::Reclaim(reclaim) => {
                let permit = match self.background(physical, capacity::METADATA_MARGIN)? {
                    Grant::Ready(permit) => permit,
                    Grant::Deferred => return Ok(()),
                };
                let output = || self.reclaim(reclaim);
                match permit {
                    Some(permit) => permit.run(output),
                    None => output(),
                }
            }
            _ => Err(io::Error::other("expected compaction selection")),
        }
    }

    fn background(
        &self,
        physical: Option<&Arc<cas_core::space::Governor>>,
        bytes: u64,
    ) -> io::Result<Grant> {
        match physical
            .map(|physical| physical.background(bytes))
            .transpose()
        {
            Ok(permit) => Ok(Grant::Ready(permit)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                self.defer(None)?;
                Ok(Grant::Deferred)
            }
            Err(error) => Err(error),
        }
    }

    fn reclaim(&self, reclaim: append::Reclamation) -> io::Result<()> {
        let durable = self.manifest.current().durable;
        let reclaimed = reclaim.run_with(|operation| {
            let point = match operation {
                append::ReclaimOperation::Punch { .. } => fault::Point::AfterPunch,
                append::ReclaimOperation::Unlink { .. } => fault::Point::AfterUnlink,
            };
            self.resources.hit_compaction(
                point,
                fault::Observation {
                    image: self.manifest.current().image,
                    through: durable,
                    manifest_durable: durable,
                    chunks: None,
                    operation: Some(operation),
                },
                &self.health,
            )
        })?;
        match self.exchange(Event::Reclaimed(reclaimed))? {
            Reply::Applied => Ok(()),
            _ => Err(io::Error::other("expected reclaimed-space acknowledgment")),
        }
    }

    fn rotate(&mut self, physical: Option<&Arc<cas_core::space::Governor>>) -> io::Result<()> {
        let (prepared, mut staging) = match self.exchange(Event::Allocate)? {
            Reply::Rotation(prepared, staging) => (prepared, staging),
            Reply::Deferred => return Ok(()),
            _ => return Err(io::Error::other("expected WAL rotation preparation")),
        };
        let bytes = prepared.segment_bytes() + capacity::METADATA_MARGIN;
        let permit = match physical
            .map(|physical| physical.foreground(bytes))
            .transpose()
        {
            Ok(permit) => permit,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                drop(staging);
                return self.defer(Some(prepared));
            }
            Err(error) => return Err(error),
        };
        #[cfg(test)]
        {
            let pause = self.control.lock().unwrap().rotation.take();
            if let Some(pause) = pause {
                pause.wait();
            }
        }
        self.healthy()?;
        let output = || {
            staging.start();
            let created = prepared.create()?;
            match self.exchange(Event::Rotated(created, staging))? {
                Reply::Applied => Ok(()),
                _ => Err(io::Error::other("expected WAL installation acknowledgment")),
            }
        };
        match permit {
            Some(permit) => permit.run(output),
            None => output(),
        }
    }

    pub(super) fn defer(&self, rotation: Option<append::Rotation>) -> io::Result<()> {
        match self.exchange(Event::Deferred(rotation))? {
            Reply::Applied => Ok(()),
            _ => Err(io::Error::other(
                "expected allocation deferral acknowledgment",
            )),
        }
    }
}

pub(super) struct Owner {
    pub catalog: Option<cas_core::catalog::Catalog>,
    pub store: Store,
    pub snapshots: BudgetVec<Snapshot, BudgetAllocator>,
    pub endpoints: BudgetVec<Endpoint, BudgetAllocator>,
    pub shared: BudgetArc<SharedHost>,
    pub input: mailbox::Receiver<Ready>,
}

impl Owner {
    pub fn run(mut self) {
        let _io_scope = cas_core::scheduler::Scheduler::background(&self.shared.io_scheduler);
        let mut exit = UnexpectedExit {
            shared: self.shared.clone(),
            normal: false,
        };
        let mut next_collection = Instant::now();
        loop {
            if self.shared.collection_required()
                && Instant::now() >= next_collection
                && let Ok(control) = administration::Control::claim(&self.shared)
            {
                let result = self.collect();
                self.shared.collected(&result);
                drop(control);
                next_collection = Instant::now() + Duration::from_secs(1);
            }
            let ready = match self.input.recv_timeout(Duration::from_millis(50)) {
                Ok(ready) => ready,
                Err(mailbox::RecvTimeoutError::Timeout) => continue,
                Err(mailbox::RecvTimeoutError::Disconnected) => break,
            };
            let (index, turn) = match ready {
                Ready::Image { index, turn } => (index, turn),
                Ready::Collect(request) => {
                    let result = self.collect();
                    self.shared.collected(&result);
                    request.complete(result);
                    next_collection = Instant::now() + Duration::from_secs(1);
                    continue;
                }
                Ready::Snapshot(request) => {
                    let result = self.snapshot(request.image, request.snapshot);
                    request.done.complete(result);
                    continue;
                }
            };
            let Some(endpoint) = self.endpoints.get_mut(index) else {
                self.shared
                    .gate
                    .fail("unknown background image slot".into());
                break;
            };
            let result = match turn {
                Turn::Compact => {
                    endpoint.compact(&mut self.store, self.shared.physical.as_ref(), None)
                }
                Turn::Rotate => endpoint.rotate(self.shared.physical.as_ref()),
            };
            if let Err(error) = result {
                if self.store.status().failed || self.shared.account_failed() {
                    self.shared.gate.fail(error.to_string());
                }
                if let Ok(mut health) = endpoint.health.lock() {
                    health.fail(error.to_string());
                }
                let _ = endpoint.output.try_send(Event::Failed(error.to_string()));
                let _ = notify(&endpoint.wake);
            }
        }
        exit.normal = true;
    }
}

struct UnexpectedExit {
    shared: BudgetArc<SharedHost>,
    normal: bool,
}

impl Drop for UnexpectedExit {
    fn drop(&mut self) {
        if !self.normal {
            self.shared.gate.fail("background owner panicked".into());
        }
    }
}
