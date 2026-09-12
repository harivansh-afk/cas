use super::*;

pub(super) struct Endpoint {
    pub manifest: Manifest,
    #[cfg(test)]
    pub pause: Arc<Mutex<Option<tests::Pause>>>,
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

    fn compact(&mut self, store: &mut Store) -> io::Result<()> {
        let reclaim = match self.exchange(Event::Select)? {
            Reply::Selected(None) => return Ok(()),
            Reply::Selected(Some(selection)) => {
                #[cfg(test)]
                {
                    let pause = self.pause.lock().unwrap().take();
                    if let Some(pause) = pause {
                        pause.wait();
                    }
                }
                self.healthy()?;
                let input = selection.load()?;
                self.healthy()?;
                let receipt = input.write(store, &mut self.manifest)?;
                match self.exchange(Event::Published(receipt))? {
                    Reply::Reclaim(reclaim) => reclaim,
                    _ => return Err(io::Error::other("expected reclamation after D publication")),
                }
            }
            Reply::Reclaim(reclaim) => reclaim,
            _ => return Err(io::Error::other("expected compaction selection")),
        };
        match self.exchange(Event::Reclaimed(reclaim.run()?))? {
            Reply::Applied => Ok(()),
            _ => Err(io::Error::other("expected reclaimed-space acknowledgment")),
        }
    }
}

pub(super) struct Owner {
    pub store: Store,
    pub endpoints: BudgetVec<Endpoint, BudgetAllocator>,
    pub shared: Arc<SharedHost>,
    pub input: mpsc::Receiver<usize>,
}

impl Owner {
    pub fn run(mut self) {
        let mut exit = UnexpectedExit {
            shared: Arc::clone(&self.shared),
            normal: false,
        };
        while let Ok(index) = self.input.recv() {
            let Some(endpoint) = self.endpoints.get_mut(index) else {
                self.shared
                    .gate
                    .fail("unknown background image slot".into());
                break;
            };
            if let Err(error) = endpoint.compact(&mut self.store) {
                if self.store.status().failed {
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
