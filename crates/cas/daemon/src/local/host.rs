//! One writer owner; image reactors exchange bounded, sequenced receipts.
pub(super) mod admission;
pub(super) mod capacity;
pub(crate) mod fair;
pub use admission::Quiescence;
mod administration;
mod collection;
pub mod fault;
mod snapshots;
mod statistics;
pub use snapshots::{SnapshotHandle, SnapshotReport};
pub mod initialize;
pub mod recovery;
#[cfg(test)]
pub(crate) mod tests;
mod worker;
pub use collection::{CollectionHandle, CollectionReport};

use super::*;
use allocator_api2::vec::Vec as BudgetVec;
use cas_core::{
    budget::BudgetAllocator,
    manifest::file::{Manifest, Snapshot},
    store::file::{Reader, Store},
};
use std::os::fd::{FromRawFd, IntoRawFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

pub struct Resources {
    pub cache_bytes: usize,
    pub metadata_cache_bytes: usize,
    pub metadata: Arc<Budget>,
    pub compaction: Arc<Budget>,
    pools: pools::HostPools,
    fault: Option<Mutex<Option<fault::Pause>>>,
}

impl Default for Resources {
    fn default() -> Self {
        Self {
            cache_bytes: Self::DEFAULT_CACHE_BYTES,
            metadata_cache_bytes: 16 * MAX_REQUEST_BYTES,
            metadata: metadata_budget(),
            compaction: metadata_budget(),
            pools: pools::HostPools::new(),
            fault: None,
        }
    }
}

impl Resources {
    pub const DEFAULT_CACHE_BYTES: usize = 256 * MAX_REQUEST_BYTES;

    pub fn read_memory(&self) -> Arc<Budget> {
        Arc::clone(&self.pools.read)
    }
}

struct SharedHost {
    admission: BudgetArc<admission::Admission>,
    fair: BudgetArc<fair::Fair>,
    io_scheduler: BudgetArc<cas_core::scheduler::Scheduler>,
    gate: BudgetArc<state::HostGate>,
    reader: Reader,
    cache: BudgetArc<cas_core::cache::Cache>,
    pages: BudgetArc<cas_core::manifest::file::PageCache>,
    fetches: Fetches,
    resources: Arc<Resources>,
    attached: AtomicUsize,
    administrating: AtomicBool,
    collection: Mutex<collection::Status>,
    compaction: BudgetVec<BudgetArc<Mutex<statistics::Totals>>, BudgetAllocator>,
    staging: BudgetArc<cas_core::space::Staging>,
    physical: Option<Arc<cas_core::space::Governor>>,
    #[cfg(test)]
    control: Arc<Mutex<tests::Control>>,
}

struct Attachment {
    mode: Option<recovery::Mode>,
    image: [u8; 16],
    log: Log,
    port: Port,
}

#[derive(Default)]
struct Context {
    catalog: Option<cas_core::catalog::Catalog>,
    mode: Option<recovery::Mode>,
    gates: Option<recovery::frontend::Gates>,
}

/// Complete retained membership, stabilized together before starting the host.
pub struct Roots<I = Vec<(Log, Manifest)>, S = Vec<Snapshot>> {
    pub images: I,
    pub snapshots: S,
}

/// Embedding API for the catalog owner and the same native validation frontend.
/// Inputs must be inspected/stabilized together and allocated from Resources.
pub struct Host {
    shared: BudgetArc<SharedHost>,
    images: BudgetVec<Option<Attachment>, BudgetAllocator>,
    ready: Option<mailbox::Sender<Ready>>,
    worker: Option<JoinHandle<()>>,
}

impl Host {
    pub(crate) fn failure(&self) -> Option<String> {
        self.shared.gate.failure()
    }

    pub(crate) fn fail_all(&self, reason: &str) {
        self.shared.gate.fail(reason.to_owned());
    }
    pub fn new(
        resources: Arc<Resources>,
        store: Store,
        images: Vec<(Log, Manifest)>,
    ) -> io::Result<Self> {
        Self::build(
            resources,
            store,
            Roots {
                images,
                snapshots: Vec::new(),
            },
            1024 * MAX_REQUEST_BYTES as u64,
            None,
        )
    }

    pub fn governed(
        resources: Arc<Resources>,
        store: Store,
        images: Vec<(Log, Manifest)>,
        physical: Arc<cas_core::space::Governor>,
        staging_bytes: u64,
    ) -> io::Result<Self> {
        Self::build(
            resources,
            store,
            Roots {
                images,
                snapshots: Vec::new(),
            },
            staging_bytes,
            Some(physical),
        )
    }

    pub fn from_roots(
        resources: Arc<Resources>,
        store: Store,
        roots: Roots,
        staging_bytes: u64,
        physical: Option<Arc<cas_core::space::Governor>>,
    ) -> io::Result<Self> {
        Self::build(resources, store, roots, staging_bytes, physical)
    }

    fn build(
        resources: Arc<Resources>,
        store: Store,
        roots: Roots,
        staging_bytes: u64,
        physical: Option<Arc<cas_core::space::Governor>>,
    ) -> io::Result<Self> {
        Self::build_owned(
            resources,
            store,
            roots,
            staging_bytes,
            physical,
            Context::default(),
        )
    }

    fn build_owned<I, S>(
        resources: Arc<Resources>,
        store: Store,
        roots: Roots<I, S>,
        staging_bytes: u64,
        physical: Option<Arc<cas_core::space::Governor>>,
        context: Context,
    ) -> io::Result<Self>
    where
        I: AsRef<[(Log, Manifest)]> + IntoIterator<Item = (Log, Manifest)>,
        S: AsRef<[Snapshot]> + IntoIterator<Item = Snapshot>,
    {
        let Roots { images, snapshots } = roots;
        if let Some(physical) = &physical {
            capacity::validate(&store, images.as_ref(), physical)?;
        }
        for snapshot in snapshots.as_ref() {
            if snapshot.key().commit.store != store.config().store {
                return Err(io::Error::other("snapshot belongs to another store"));
            }
            if let Some(physical) = &physical {
                physical.validate_file(snapshot.view()?.file())?;
            }
        }
        if images.as_ref().is_empty() {
            return Err(io::Error::other("host requires an image"));
        }
        capacity::staging_geometry(images.as_ref(), staging_bytes)?;
        let staging = cas_core::space::Staging::new(
            staging_bytes,
            images
                .as_ref()
                .iter()
                .map(|(log, _)| cas_core::space::StagingImage {
                    allocated: log.status().allocated_bytes,
                    capacity: log.limits().staging_bytes,
                }),
            &resources.metadata,
        )?;
        let image_count = images.as_ref().len();
        if let Some(gates) = &context.gates {
            gates.validate(images.as_ref())?;
        }
        let admission = admission::Admission::new(image_count, &resources.metadata)?;
        let metadata = Arc::clone(&resources.metadata);
        let fair = fair::Fair::new(image_count, admission.clone(), &metadata)?;
        let mut compaction = reserved_vec(image_count, &metadata)?;
        for _ in 0..image_count {
            compaction.push(BudgetArc::try_new(
                Mutex::new(statistics::Totals::default()),
                &metadata,
            )?);
        }
        let gate = match &context.gates {
            Some(gates) => gates.host.clone(),
            None => state::HostGate::new(&metadata)?,
        };
        let shared = BudgetArc::try_new(
            SharedHost {
                admission,
                fair,
                io_scheduler: cas_core::scheduler::Scheduler::new(image_count, &metadata)?,
                gate,
                reader: store.reader()?,
                cache: cas_core::cache::Cache::new(resources.cache_bytes, &metadata)?,
                pages: cas_core::manifest::file::PageCache::new(
                    resources.metadata_cache_bytes,
                    &metadata,
                )?,
                fetches: cas_core::cache::fills::Registry::new(
                    pools::HOST_REQUESTS,
                    pools::HOST_REQUESTS,
                    &metadata,
                )?,
                resources,
                attached: AtomicUsize::new(0),
                administrating: AtomicBool::new(false),
                collection: Mutex::new(collection::Status::default()),
                compaction,
                staging,
                physical,
                #[cfg(test)]
                control: Arc::new(Mutex::new(tests::Control::default())),
            },
            &metadata,
        )?;
        let mut attachments =
            BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&shared.resources.metadata)));
        let mut endpoints =
            BudgetVec::new_in(BudgetAllocator::new(Arc::clone(&shared.resources.metadata)));
        attachments
            .try_reserve_exact(image_count)
            .map_err(|_| io::Error::other("host image metadata exhausted"))?;
        endpoints
            .try_reserve_exact(image_count)
            .map_err(|_| io::Error::other("host endpoint metadata exhausted"))?;
        let (ready, input) = mailbox::bounded(image_count + 1, &shared.resources.metadata)?;
        for (index, (log, manifest)) in images.into_iter().enumerate() {
            let admission = capacity::Admission {
                staging: shared.staging.clone(),
                physical: shared.physical.clone(),
                image: index,
            };
            let window = window::Window::new(&log, Some(admission), &shared.resources.metadata)?;
            let view = manifest.view()?;
            let identity = view.commit();
            if identity.store != store.config().store
                || !log.uses_tickets(store.tickets())
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
            let health = match &context.gates {
                Some(gates) => gates.images[index].1.clone(),
                None => state::Gate::new(
                    ImageState {
                        durable: log.status().durable,
                        ..ImageState::default()
                    },
                    Some(shared.gate.clone()),
                    &metadata,
                )?,
            };
            let wake = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
            let (output, events) = mailbox::bounded(1, &shared.resources.metadata)?;
            let (reply, replies) = mailbox::bounded(1, &shared.resources.metadata)?;
            endpoints.push(worker::Endpoint {
                statistics: shared.compaction[index].clone(),
                resources: Arc::clone(&shared.resources),
                manifest,
                quiescent: None,
                #[cfg(test)]
                control: Arc::clone(&shared.control),
                health: health.clone(),
                wake: wake.try_clone()?,
                output,
                replies,
            });
            attachments.push(Some(Attachment {
                mode: context.mode,
                image: identity.image,
                log,
                port: Port {
                    index,
                    shared: shared.clone(),
                    health,
                    ready: ready.clone(),
                    events,
                    reply,
                    wake,
                    active: None,
                    granted: false,
                    quiescence: None,
                    oldest: None,
                    retry: false,
                    last_reclaim: Instant::now(),
                    attached: false,
                    rotation: None,
                    rotation_retry: Instant::now(),
                    window,
                },
            }));
        }
        let mut retained = reserved_vec(snapshots.as_ref().len(), &shared.resources.metadata)?;
        retained.extend(snapshots);
        let owner = worker::Owner {
            catalog: context.catalog,
            snapshots: retained,
            store,
            endpoints,
            shared: shared.clone(),
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
            crate::storage::Storage::from_local(local)?,
            event,
            crate::BackendKind::LocalAsync,
            false,
            false,
            fault,
        )
    }

    /// Export cold recovery's synced epoch; retained startup uses its coordinator.
    pub fn attach_cold(
        &mut self,
        image: [u8; 16],
        timeout: Duration,
        fault: crate::fault::Fault,
    ) -> io::Result<crate::backend::Backend> {
        let deadline = crate::deadline::Deadline::after(timeout);
        deadline.check()?;
        let attachment = self
            .images
            .iter()
            .flatten()
            .find(|a| a.image == image)
            .ok_or_else(|| io::Error::other("unknown or already attached host image"))?;
        let status = attachment.log.status();
        if attachment.mode != Some(recovery::Mode::Cold)
            || !attachment.log.covers_flush(status.published)
        {
            return Err(io::Error::other(
                "attachment has no completed cold recovery",
            ));
        }
        let config = attachment.log.config();
        let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
        let local = self.local(image, &event)?;
        let mut backend = crate::backend::Backend::from_storage(
            crate::storage::Storage::from_local(local)?,
            event,
            crate::BackendKind::LocalAsync,
            true,
            true,
            fault,
        )?;
        backend.cold_attachment(config, status, deadline)?;
        Ok(backend)
    }

    fn local(&mut self, image: [u8; 16], event: &EventFd) -> io::Result<Local> {
        let _attachment =
            admission::Admission::enter(&self.shared.admission).ok_or(io::ErrorKind::WouldBlock)?;
        let slot = self
            .images
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|a| a.image == image))
            .ok_or_else(|| io::Error::other("unknown or already attached host image"))?;
        // Allocate and clone before taking the attachment: a denied metadata
        // charge or an exhausted descriptor table leaves the image attachable.
        let attachment = slot.as_ref().expect("matched attachment");
        let frontend = event.try_clone()?;
        let reactor = attachment.port.wake.try_clone()?;
        let wake = attachment.port.wake.try_clone()?.into_raw_fd();
        // SAFETY: the freshly cloned eventfd transfers its unique FD owner here.
        let wake = unsafe { std::os::fd::OwnedFd::from_raw_fd(wake) };
        let shared = Shared::with_pools(
            attachment.log.status(),
            self.shared.resources.pools.image(),
            attachment.port.health.clone(),
            Arc::clone(&self.shared.resources.metadata),
            Some(attachment.port.window.clone()),
            Some(self.shared.admission.clone()),
        )?;
        self.shared
            .admission
            .bind(attachment.port.index, frontend, reactor)?;
        let Attachment { log, mut port, .. } = slot.take().expect("matched attachment");
        // From here `Port::drop` detaches the bound wake if a later step fails.
        port.attached = true;
        self.shared.attached.fetch_add(1, Ordering::Relaxed);
        self.shared.io_scheduler.bind(port.index, wake)?;
        Local::from_host(log, event, shared, port)
    }

    pub fn pause_admission(&self) -> io::Result<Quiescence> {
        admission::Admission::pause(&self.shared.admission)
    }

    pub fn store_status(&self) -> cas_core::store::file::Status {
        self.shared.reader.status()
    }

    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({ "failure": self.shared.gate.failure(), "store": self.store_status(),
            "admission": self.shared.admission.status(),
            "cache": self.shared.cache.status(), "metadata_cache": self.shared.pages.status(),
            "fetches": self.shared.fetches.status(), "admission_scheduler":self.shared.fair.report(), "io_scheduler":self.shared.io_scheduler.status(),
            "collection": *self.shared.collection.lock().expect("collection status poisoned"),
            "compaction": self.shared.compaction.iter().map(|totals| *totals.lock().expect("compaction statistics poisoned")).collect::<Vec<_>>(),
            "pools": self.shared.resources.pools.report(), "metadata": self.shared.resources.metadata.usage(),
            "compaction_metadata": self.shared.resources.compaction.usage(),
            "attached": self.shared.attached.load(Ordering::Acquire),
            "staging": self.shared.staging.status(0).expect("host staging account").0,
            "space": self.shared.physical.as_ref().map(|physical| physical.status()),
            "collection_required": self.shared.collection_required() })
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
    Quiesce(u64),
    Resume(u64),
    CompactQuiescent(u64),
    Select,
    Allocate,
    Published(append::Compacted),
    Reclaimed(append::Reclaimed),
    Rotated(append::Rotated, cas_core::space::StagingPermit),
    Deferred(Option<append::Rotation>),
    Failed(String),
}

enum Reply {
    Quiesced(u64),
    Selected(Option<append::Selection>),
    Rotation(append::Rotation, cas_core::space::StagingPermit),
    Deferred,
    Reclaim(append::Reclamation),
    Applied,
    Failed(String),
}

#[derive(Clone, Copy)]
enum Turn {
    Compact,
    Rotate,
}

enum Ready {
    Image { index: usize, turn: Turn },
    Collect(collection::Request),
    Snapshot(snapshots::Request),
}

pub(super) struct Port {
    index: usize,
    shared: BudgetArc<SharedHost>,
    health: Health,
    ready: mailbox::Sender<Ready>,
    events: mailbox::Receiver<Event>,
    reply: mailbox::Sender<Reply>,
    pub wake: EventFd,
    active: Option<Instant>,
    granted: bool,
    quiescence: Option<(u64, bool)>,
    oldest: Option<Instant>,
    retry: bool,
    last_reclaim: Instant,
    attached: bool,
    rotation: Option<append::RotationKind>,
    rotation_retry: Instant,
    window: BudgetArc<window::Window>,
}

impl Port {
    pub fn abort(&mut self, error: &io::Error) {
        if (self.active.is_some() && self.granted) || self.quiescence.is_some_and(|(_, ack)| !ack) {
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

    pub(super) fn fair(&self) -> fair::Port {
        fair::Port {
            owner: self.shared.fair.clone(),
            image: self.index,
        }
    }

    pub(super) fn io_scheduler(&self) -> io::Result<cas_core::scheduler::Port> {
        cas_core::scheduler::Scheduler::port(&self.shared.io_scheduler, self.index)
    }

    pub fn reader(&self) -> Reader {
        self.shared.reader.clone()
    }
    pub fn cache(&self) -> BudgetArc<cas_core::cache::Cache> {
        self.shared.cache.clone()
    }
    pub fn page_cache(&self) -> BudgetArc<cas_core::manifest::file::PageCache> {
        self.shared.pages.clone()
    }
    pub fn fetches(&self) -> Fetches {
        self.shared.fetches.clone()
    }
    #[cfg(test)]
    pub fn read_control(&self) -> Arc<Mutex<tests::Control>> {
        Arc::clone(&self.shared.control)
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

    pub fn cancel_future_rollover(&mut self, log: &Log) {
        if self.rotation == Some(append::RotationKind::Rollover) && !log.status().rotating {
            self.rotation = None;
        }
    }

    pub fn rotate(&mut self, kind: append::RotationKind) -> io::Result<()> {
        if self.rotation.is_some_and(|old| old != kind) {
            return Err(io::Error::other("conflicting WAL rotation requests"));
        }
        self.rotation = Some(kind);
        self.window.close();
        if self.active.is_none() && !self.shared.admission.status().paused {
            self.queue(Turn::Rotate)?;
        }
        Ok(())
    }

    fn queue(&mut self, turn: Turn) -> io::Result<()> {
        self.ready
            .try_send(Ready::Image {
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
        if self.shared.account_failed() {
            self.shared
                .gate
                .fail("shared allocation account failed".into());
            return Err(io::Error::other("shared allocation account failed"));
        }
        while let Ok(event) = self.events.try_recv() {
            if matches!(event, Event::Select | Event::Allocate) {
                self.active = Some(Instant::now());
                self.granted = true;
            }
            let gate = self.health.clone();
            let mut health = gate.lock()?;
            if let Some(error) = &health.failure {
                let _ = self.respond(Reply::Failed(error.clone()));
                self.active = None;
                return Err(io::Error::other(error.clone()));
            }
            match event {
                Event::Quiesce(generation) => {
                    if self.rotation == Some(append::RotationKind::FreshAttachment) {
                        self.respond(Reply::Deferred)?;
                        continue;
                    }
                    self.cancel_future_rollover(log);
                    self.active = None;
                    self.granted = false;
                    self.quiescence = Some((generation, false));
                    #[cfg(test)]
                    if self.shared.control.lock().unwrap().quiescence_error {
                        return Err(io::Error::other(
                            "injected quiescence acknowledgment failure",
                        ));
                    }
                }
                Event::Resume(generation) => {
                    if self.quiescence != Some((generation, true)) {
                        return Err(io::Error::other("collection resume generation differs"));
                    }
                    self.quiescence = None;
                    self.respond(Reply::Applied)?;
                }
                Event::CompactQuiescent(generation) => {
                    if self.quiescence != Some((generation, true)) {
                        return Err(io::Error::other("collection compaction generation differs"));
                    }
                    self.active = Some(Instant::now());
                    self.granted = true;
                    let selection = log
                        .select_compaction(
                            Arc::clone(&self.shared.resources.compaction),
                            self.shared.resources.read_memory(),
                        )
                        .map_err(io::Error::other)?;
                    if selection.is_none() {
                        self.active = None;
                        self.granted = false;
                    }
                    self.respond(Reply::Selected(selection))?;
                }
                Event::Allocate => {
                    let Some(kind) = self.rotation else {
                        // A pause cancelled this future-only queued turn.
                        self.defer();
                        self.respond(Reply::Deferred)?;
                        continue;
                    };
                    self.window.before_rotation(log)?;
                    let permit = match cas_core::space::Staging::reserve(
                        &self.shared.staging,
                        self.index,
                        log.config().segment_bytes,
                    ) {
                        Ok(permit) => permit,
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                            self.defer();
                            self.respond(Reply::Deferred)?;
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                    match log.prepare_rotation(kind) {
                        Ok(rotation) => self.respond(Reply::Rotation(rotation, permit))?,
                        Err(append::Error::Capacity) => {
                            drop(permit);
                            self.defer();
                            self.respond(Reply::Deferred)?;
                        }
                        Err(error) => return Err(io::Error::other(error)),
                    }
                }
                Event::Rotated(receipt, permit) => {
                    let result = log
                        .install_rotation(receipt)
                        .map_err(io::Error::other)
                        .and_then(|()| permit.installed(log.status().allocated_bytes));
                    if let Err(error) = result {
                        health.fail_host(error.to_string());
                        return Err(error);
                    }
                    self.shared.fair.resources_released()?;
                    self.respond(Reply::Applied)?;
                    self.rotation = None;
                    self.active = None;
                }
                Event::Deferred(rotation) => {
                    if let Some(rotation) = rotation {
                        log.cancel_rotation(rotation).map_err(io::Error::other)?;
                    }
                    self.defer();
                    self.shared.fair.resources_released()?;
                    self.respond(Reply::Applied)?;
                }
                Event::Select
                    if log.status().durable > log.status().compacted
                        && !self.shared.collection_required() =>
                {
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
                    let before = self.shared.staging.status(self.index)?;
                    let stats = log.apply_reclamation(receipt).map_err(io::Error::other)?;
                    let reopened = match self
                        .shared
                        .staging
                        .reclaimed(self.index, log.status().allocated_bytes)
                    {
                        Ok(reopened) => reopened,
                        Err(error) => {
                            health.fail_host(error.to_string());
                            return Err(error);
                        }
                    };
                    let after = self.shared.staging.status(self.index)?;
                    {
                        let mut totals = self.shared.compaction[self.index]
                            .lock()
                            .expect("compaction statistics poisoned");
                        totals.staging_reopens += u64::from(reopened);
                        totals.reclaimed_bytes += before.1.allocated - after.1.allocated;
                    }
                    if reopened {
                        self.shared.fair.resources_released()?;
                    }
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
            || self.window.host_pressure()
            || self
                .oldest
                .is_some_and(|oldest| oldest.elapsed() >= Duration::from_secs(1));
        let retry = self.retry && self.last_reclaim.elapsed() >= Duration::from_millis(100);
        let turn = if self.rotation.is_some() && Instant::now() >= self.rotation_retry {
            Some(Turn::Rotate)
        } else if !paused
            && ((dirty && (settled || forced) && !self.shared.collection_required()) || retry)
        {
            Some(Turn::Compact)
        } else {
            None
        };
        if let Some(turn) = turn.filter(|_| {
            self.active.is_none()
                && self.quiescence.is_none()
                && !self.shared.admission.status().paused
        }) {
            self.queue(turn)?;
        }
        Ok(())
    }

    pub fn needs_wake(&self, log: &Log) -> bool {
        self.pending() || self.retry || log.status().durable > log.status().compacted
    }

    fn defer(&mut self) {
        self.active = None;
        self.granted = false;
        self.rotation_retry = Instant::now() + Duration::from_millis(100);
    }
}

impl Port {
    pub fn quiescing(&self) -> bool {
        self.quiescence.is_some()
    }

    pub fn acknowledge_quiescence(&mut self) -> io::Result<()> {
        if let Some((generation, false)) = self.quiescence {
            self.respond(Reply::Quiesced(generation))?;
            self.quiescence = Some((generation, true));
        }
        Ok(())
    }
}

impl Drop for Port {
    fn drop(&mut self) {
        if self.attached {
            self.shared.admission.detach(self.index);
            self.shared.attached.fetch_sub(1, Ordering::Release);
        }
    }
}
