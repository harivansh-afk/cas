//! One writer owner; image reactors exchange bounded, sequenced receipts.
#[cfg(test)]
mod tests;
mod worker;

use super::*;
use allocator_api2::vec::Vec as BudgetVec;
use cas_core::{
    budget::BudgetAllocator,
    manifest::file::Manifest,
    store::file::{Reader, Store},
};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Resources {
    pub metadata: Arc<Budget>,
    pub compaction: Arc<Budget>,
    pools: pools::HostPools,
}

impl Default for Resources {
    fn default() -> Self {
        let metadata = || {
            Budget::new(Amount {
                bytes: 128 * MAX_REQUEST_BYTES,
                requests: 0,
            })
        };
        Self {
            metadata: metadata(),
            compaction: metadata(),
            pools: pools::HostPools::new(),
        }
    }
}

impl Resources {
    pub fn read_memory(&self) -> Arc<Budget> {
        Arc::clone(&self.pools.read)
    }
}

struct SharedHost {
    gate: Arc<state::HostGate>,
    reader: Reader,
    resources: Arc<Resources>,
    attached: AtomicUsize,
    #[cfg(test)]
    control: Arc<Mutex<tests::Control>>,
}

struct Attachment {
    image: [u8; 16],
    log: Log,
    port: Port,
}

/// Embedding API for the catalog owner and the same native validation frontend.
/// Inputs must be inspected/stabilized together and allocated from Resources.
pub struct Host {
    shared: Arc<SharedHost>,
    images: BudgetVec<Option<Attachment>, BudgetAllocator>,
    ready: Option<mpsc::SyncSender<Ready>>,
    worker: Option<JoinHandle<()>>,
}

impl Host {
    pub fn new(
        resources: Arc<Resources>,
        store: Store,
        images: Vec<(Log, Manifest)>,
    ) -> io::Result<Self> {
        if images.is_empty() {
            return Err(io::Error::other("host requires an image"));
        }
        let shared = Arc::new(SharedHost {
            gate: Arc::new(state::HostGate::default()),
            reader: store.reader()?,
            resources,
            attached: AtomicUsize::new(0),
            #[cfg(test)]
            control: Arc::new(Mutex::new(tests::Control::default())),
        });
        let mut attachments =
            BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&shared.resources.metadata)));
        let mut endpoints =
            BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&shared.resources.metadata)));
        attachments
            .try_reserve_exact(images.len())
            .map_err(|_| io::Error::other("host image metadata exhausted"))?;
        endpoints
            .try_reserve_exact(images.len())
            .map_err(|_| io::Error::other("host endpoint metadata exhausted"))?;
        let (ready, input) = mpsc::sync_channel(images.len());
        for (index, (log, manifest)) in images.into_iter().enumerate() {
            let window = window::Window::new(&log, &shared.resources.metadata)?;
            let view = manifest.view()?;
            let identity = view.commit();
            if identity.store != store.config().store
                || !log.manifest().is_some_and(|base| base.same(&view))
                || attachments
                    .iter()
                    .flatten()
                    .any(|old: &Attachment| old.image == identity.image)
            {
                return Err(io::Error::other(
                    "host image identity or stabilized root differs",
                ));
            }
            let health = state::Gate::new(
                ImageState {
                    durable: log.status().durable,
                    ..ImageState::default()
                },
                Some(Arc::clone(&shared.gate)),
            );
            let wake = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
            let (output, events) = mpsc::sync_channel(1);
            let (reply, replies) = mpsc::sync_channel(1);
            endpoints.push(worker::Endpoint {
                manifest,
                #[cfg(test)]
                control: Arc::clone(&shared.control),
                health: Arc::clone(&health),
                wake: wake.try_clone()?,
                output,
                replies,
            });
            attachments.push(Some(Attachment {
                image: identity.image,
                log,
                port: Port {
                    index,
                    shared: Arc::clone(&shared),
                    health,
                    ready: ready.clone(),
                    events,
                    reply,
                    wake,
                    active: None,
                    granted: false,
                    oldest: None,
                    retry: false,
                    last_reclaim: Instant::now(),
                    attached: false,
                    rotation: None,
                    window,
                },
            }));
        }
        let owner = worker::Owner {
            store,
            endpoints,
            shared: Arc::clone(&shared),
            input,
        };
        let worker = thread::Builder::new()
            .name("cas-compactor".into())
            .spawn(move || owner.run())?;
        Ok(Self {
            shared,
            images: attachments,
            ready: Some(ready),
            worker: Some(worker),
        })
    }

    pub fn attach(
        &mut self,
        image: [u8; 16],
        fault: crate::fault::Fault,
    ) -> io::Result<crate::backend::Backend> {
        let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
        let local = self.local(image, &event)?;
        crate::backend::Backend::from_storage(
            crate::storage::Storage::Local(Box::new(local)),
            event,
            crate::BackendKind::LocalAsync,
            false,
            false,
            fault,
        )
    }

    fn local(&mut self, image: [u8; 16], event: &EventFd) -> io::Result<Local> {
        let slot = self
            .images
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|a| a.image == image))
            .ok_or_else(|| io::Error::other("unknown or already attached host image"))?;
        let Attachment { log, mut port, .. } = slot.take().unwrap();
        let shared = Shared::with_pools(
            log.status(),
            self.shared.resources.pools.image(),
            Arc::clone(&port.health),
            Arc::clone(&self.shared.resources.metadata),
            Some(port.window.clone()),
        );
        port.attached = true;
        self.shared.attached.fetch_add(1, Ordering::Relaxed);
        Local::from_host(log, event, shared, port)
    }

    pub fn store_status(&self) -> cas_core::store::file::Status {
        self.shared.reader.status()
    }

    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({ "failure": self.shared.gate.failure(), "store": self.store_status(),
            "pools": self.shared.resources.pools.report(), "metadata": self.shared.resources.metadata.usage(),
            "compaction_metadata": self.shared.resources.compaction.usage(),
            "attached": self.shared.attached.load(Ordering::Acquire) })
    }

    /// Nonblocking shutdown. Drop attached backends first; retry WouldBlock
    /// within the caller's shutdown deadline. Live worker IO retains its owners.
    pub fn shutdown(&mut self) -> io::Result<()> {
        if self.shared.attached.load(Ordering::Acquire) != 0 {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "host images still attached",
            ));
        }
        self.images.clear();
        self.ready.take();
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| !worker.is_finished())
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "background owner still draining",
            ));
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("background owner panicked"))?;
        }
        Ok(())
    }
}

enum Event {
    Select,
    Allocate,
    Published(append::Compacted),
    Reclaimed(append::Reclaimed),
    Rotated(append::Rotated),
    Failed(String),
}

enum Reply {
    Selected(Option<append::Selection>),
    Rotation(append::Rotation),
    Reclaim(append::Reclamation),
    Applied,
    Failed(String),
}

#[derive(Clone, Copy)]
enum Turn {
    Compact,
    Rotate,
}

struct Ready {
    index: usize,
    turn: Turn,
}

pub(super) struct Port {
    index: usize,
    shared: Arc<SharedHost>,
    health: Health,
    ready: mpsc::SyncSender<Ready>,
    events: mpsc::Receiver<Event>,
    reply: mpsc::SyncSender<Reply>,
    pub wake: EventFd,
    active: Option<Instant>,
    granted: bool,
    oldest: Option<Instant>,
    retry: bool,
    last_reclaim: Instant,
    attached: bool,
    rotation: Option<append::RotationKind>,
    window: cas_core::budget::BudgetArc<window::Window>,
}

impl Port {
    pub fn abort(&mut self, error: &io::Error) {
        if self.active.is_some() && self.granted {
            let _ = self.respond(Reply::Failed(error.to_string()));
        }
        // The worker still owns its selection/output and will observe failure
        // before another publication. Closing this image must not await its IO.
        self.active = None;
        self.rotation = None;
    }

    pub fn fail_shared(&self, error: &io::Error) {
        self.shared.gate.fail(error.to_string());
    }

    pub fn reader(&self) -> Reader {
        self.shared.reader.clone()
    }
    pub fn metadata(&self) -> Arc<Budget> {
        Arc::clone(&self.shared.resources.metadata)
    }
    pub fn pending(&self) -> bool {
        self.active.is_some() || self.rotation.is_some()
    }

    pub fn rotating(&self) -> bool {
        self.rotation.is_some()
    }

    pub fn rotate(&mut self, kind: append::RotationKind) -> io::Result<()> {
        if self.rotation.is_some_and(|old| old != kind) {
            return Err(io::Error::other("conflicting WAL rotation requests"));
        }
        self.rotation = Some(kind);
        self.window.close();
        if self.active.is_none() {
            self.queue(Turn::Rotate)?;
        }
        Ok(())
    }

    fn queue(&mut self, turn: Turn) -> io::Result<()> {
        self.ready
            .try_send(Ready {
                index: self.index,
                turn,
            })
            .map_err(|_| io::Error::other("background ready queue unavailable"))?;
        self.active = Some(Instant::now());
        self.granted = false;
        Ok(())
    }

    fn respond(&self, reply: Reply) -> io::Result<()> {
        self.reply
            .try_send(reply)
            .map_err(|_| io::Error::other("background reply unavailable"))
    }

    fn reclaim(&self, log: &Log, health: &ImageState) -> io::Result<append::Reclamation> {
        let oldest = health
            .carrier
            .as_ref()
            .map(|carrier| carrier.oldest_live_mutation())
            .transpose()?
            .flatten();
        log.select_reclamation(oldest, Arc::clone(&self.shared.resources.compaction))
            .map_err(io::Error::other)
    }

    pub fn poll(&mut self, log: &mut Log, last_write: Instant, paused: bool) -> io::Result<()> {
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, Event::Select | Event::Allocate) {
                self.active = Some(Instant::now());
                self.granted = true;
            }
            let gate = Arc::clone(&self.health);
            let health = gate.lock()?;
            if let Some(error) = &health.failure {
                let _ = self.respond(Reply::Failed(error.clone()));
                self.active = None;
                return Err(io::Error::other(error.clone()));
            }
            match event {
                Event::Allocate => {
                    self.window.before_rotation(log)?;
                    let kind = self
                        .rotation
                        .ok_or_else(|| io::Error::other("unrequested WAL allocation grant"))?;
                    self.respond(Reply::Rotation(
                        log.prepare_rotation(kind).map_err(io::Error::other)?,
                    ))?;
                }
                Event::Rotated(receipt) => {
                    log.install_rotation(receipt).map_err(io::Error::other)?;
                    self.respond(Reply::Applied)?;
                    self.rotation = None;
                    self.active = None;
                }
                Event::Select if log.status().durable > log.status().compacted => {
                    self.respond(Reply::Selected(
                        log.select_compaction(
                            Arc::clone(&self.shared.resources.compaction),
                            self.shared.resources.read_memory(),
                        )
                        .map_err(io::Error::other)?,
                    ))?;
                }
                Event::Select if self.retry => {
                    self.respond(Reply::Reclaim(self.reclaim(log, &health)?))?
                }
                Event::Select => {
                    self.respond(Reply::Selected(None))?;
                    self.active = None;
                }
                Event::Published(receipt) => {
                    log.publish_compaction(receipt).map_err(io::Error::other)?;
                    self.respond(Reply::Reclaim(self.reclaim(log, &health)?))?;
                }
                Event::Reclaimed(receipt) => {
                    let stats = log.apply_reclamation(receipt).map_err(io::Error::other)?;
                    self.retry = stats.pinned_batches != 0 || log.status().segments > 1;
                    self.last_reclaim = Instant::now();
                    self.respond(Reply::Applied)?;
                    self.active = None;
                }
                Event::Failed(error) => {
                    self.active = None;
                    return Err(io::Error::other(error));
                }
            }
        }
        if self.granted
            && self
                .active
                .is_some_and(|started| started.elapsed() >= IO_DEADLINE)
        {
            self.shared
                .gate
                .fail("background transaction deadline expired".into());
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "background transaction deadline expired",
            ));
        }
        let dirty = log.status().durable > log.status().compacted;
        if dirty {
            self.oldest.get_or_insert_with(Instant::now);
        } else {
            self.oldest = None;
        }
        let settled = last_write.elapsed() >= Duration::from_millis(100);
        let forced = self.window.status().index_pressure
            || self
                .oldest
                .is_some_and(|oldest| oldest.elapsed() >= Duration::from_secs(1));
        let retry = self.retry && self.last_reclaim.elapsed() >= Duration::from_millis(100);
        let turn = if self.rotation.is_some() {
            Some(Turn::Rotate)
        } else if !paused && ((dirty && (settled || forced)) || retry) {
            Some(Turn::Compact)
        } else {
            None
        };
        if let Some(turn) = turn.filter(|_| self.active.is_none()) {
            self.queue(turn)?;
        }
        Ok(())
    }

    pub fn needs_wake(&self, log: &Log) -> bool {
        self.pending() || self.retry || log.status().durable > log.status().compacted
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        if self.attached {
            self.shared.attached.fetch_sub(1, Ordering::Release);
        }
    }
}
