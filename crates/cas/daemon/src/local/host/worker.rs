use super::*;

enum Grant {
    Ready(Option<cas_core::space::Permit>),
    Deferred,
}

pub(super) struct Endpoint {
    pub manifest: Manifest,
    #[cfg(test)]
    pub control: Arc<Mutex<tests::Control>>,
    pub health: Health,
    pub wake: EventFd,
    pub output: mpsc::SyncSender<Event>,
    pub replies: mpsc::Receiver<Reply>,
}

impl Endpoint {
    fn healthy(&self) -> io::Result<()> {
        if let Some(error) = &self.health.lock()?.failure {
            return Err(io::Error::other(error.clone()));
        }
        Ok(())
    }

    fn exchange(&self, event: Event) -> io::Result<Reply> {
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

    fn compact(
        &mut self,
        store: &mut Store,
        physical: Option<&Arc<cas_core::space::Governor>>,
    ) -> io::Result<()> {
        match self.exchange(Event::Select)? {
            Reply::Selected(None) => Ok(()),
            Reply::Selected(Some(selection)) => {
                self.healthy()?;
                let input = selection.load()?;
                let prepared = input.prepare(&self.manifest)?;
                let bytes = capacity::compaction_bytes(&prepared, store.config().segment_bytes);
                let permit = match self.background(physical, bytes)? {
                    Grant::Ready(permit) => permit,
                    Grant::Deferred => return Ok(()),
                };
                #[cfg(test)]
                {
                    let pause = self.control.lock().unwrap().compaction.take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                self.healthy()?;
                let output = || {
                    let receipt = prepared.write(store, &mut self.manifest)?;
                    let Reply::Reclaim(reclaim) = self.exchange(Event::Published(receipt))? else {
                        return Err(io::Error::other("expected reclamation after D publication"));
                    };
                    self.reclaim(reclaim)
                };
                match permit {
                    Some(permit) => permit.run(output),
                    None => output(),
                }
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
        match self.exchange(Event::Reclaimed(reclaim.run()?))? {
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

    fn defer(&self, rotation: Option<append::Rotation>) -> io::Result<()> {
        match self.exchange(Event::Deferred(rotation))? {
            Reply::Applied => Ok(()),
            _ => Err(io::Error::other(
                "expected allocation deferral acknowledgment",
            )),
        }
    }
}

pub(super) struct Owner {
    pub store: Store,
    pub endpoints: BudgetVec<Endpoint, BudgetAllocator>,
    pub shared: Arc<SharedHost>,
    pub input: mpsc::Receiver<Ready>,
}

impl Owner {
    pub fn run(mut self) {
        let mut exit = UnexpectedExit {
            shared: Arc::clone(&self.shared),
            normal: false,
        };
        while let Ok(Ready { index, turn }) = self.input.recv() {
            let Some(endpoint) = self.endpoints.get_mut(index) else {
                self.shared
                    .gate
                    .fail("unknown background image slot".into());
                break;
            };
            let result = match turn {
                Turn::Compact => endpoint.compact(&mut self.store, self.shared.physical.as_ref()),
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
    shared: Arc<SharedHost>,
    normal: bool,
}

impl Drop for UnexpectedExit {
    fn drop(&mut self) {
        if !self.normal {
            self.shared.gate.fail("background owner panicked".into());
        }
    }
}
