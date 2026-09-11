// Queue execution: retain owned IO until completion and publish against one memory snapshot.

mod recovery;
use cas_daemon::inflight::Entry;
use recovery::Session;

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::Ordering;

use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer};
use vhost::vhost_user::message::VhostUserProtocolFeatures;
use vhost_user_backend::{ShutdownHandle, VhostUserBackendMut, VringMutex, VringState, VringT};
use virtio_bindings::bindings::{
    virtio_blk::{VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_FLUSH, VIRTIO_BLK_F_MQ},
    virtio_config::VIRTIO_F_VERSION_1,
};
use virtio_queue::{Queue, QueueOwnedT, QueueT};
use vm_memory::{
    Bytes, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::event::{EventConsumer, EventFlag, EventNotifier};
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK, EventFd};

use crate::fault::{Fault, Point};
use crate::request::{self, Completion, DEVICE_ID, Request, Segment, Status};
use crate::storage::{CompletionData, Operation, Permit, QueueHead, Storage};
use crate::{BackendKind, local};

const QUEUE_SIZE: usize = 128;
const REQUIRED_FEATURES: u64 =
    (1 << VIRTIO_F_VERSION_1) | (1 << VIRTIO_BLK_F_BLK_SIZE) | (1 << VIRTIO_BLK_F_FLUSH);
const CONCURRENT_QUEUES: usize = 4;
const CONCURRENT_QUEUE_SIZE: usize = 256;

#[derive(Clone, Copy)]
struct GuestCompletion {
    queue: u16,
    target: Completion,
    inflight: Option<Entry>,
    write_number: Option<u64>,
}

struct Admitted {
    queue: u16,
    id: u64,
    request: Request,
    permit: Permit,
    inflight: Option<Entry>,
}

struct PendingRequest {
    completion: GuestCompletion,
    segments: Vec<Segment>,
    error_published: bool,
}

#[derive(Default)]
struct Counters {
    reads: u64,
    writes: u64,
    flushes: u64,
    read_bytes: u64,
    write_bytes: u64,
    guest_payload_copy_bytes: u64,
    errors: u64,
    bounce_requests: u64,
    peak_inflight: usize,
}

pub(super) struct Backend {
    storage: Storage,
    completion_event: EventFd,
    exit: (EventConsumer, EventNotifier),
    // The framework replaces its atomic map before calling update_memory.
    // Keep our accepted map alive independently, including after a rejected update.
    memory: Option<GuestMemoryLoadGuard<GuestMemoryMmap>>,
    capacity_bytes: u64,
    pending: BTreeMap<u64, PendingRequest>,
    next_id: u64,
    counters: Counters,
    shutdown: Option<ShutdownHandle>,
    failure: Option<String>,
    negotiated_features: u64,
    restartable: bool,
    live: Option<Session>,
    concurrent: bool,
    first_queue: usize,
    queue_requests: [u64; CONCURRENT_QUEUES],
    restored_used: Option<u16>,
    restored_pending: u16,
    fault: Fault,
}

/// The queue lock serializes publication with SET_VRING_ENABLE/GET_VRING_BASE.
/// Pass the accepted map explicitly: VringT::add_used loads the mutable shared map.
fn publish(
    mem: &GuestMemoryMmap,
    state: &mut VringState,
    completion: Completion,
    status: Status,
    data: Option<(&[Segment], &[u8])>,
    fault: &mut Fault,
    write_number: Option<u64>,
) -> io::Result<()> {
    if !state.is_enabled() || !state.get_queue().ready() {
        return Err(io::Error::other(
            "queue stopped before completion; pause/resume with pending IO is unsupported",
        ));
    }
    let mut written = 0;
    if let Some((segments, bytes)) = data {
        for segment in segments {
            mem.write_slice(&bytes[written..written + segment.len], segment.addr)
                .map_err(io::Error::other)?;
            written += segment.len;
        }
    }
    mem.write_obj(status as u8, completion.status)
        .map_err(io::Error::other)?;
    fault.hit(Point::AfterStatus, write_number)?;
    state
        .get_queue_mut()
        .add_used(mem, completion.head, written as u32 + 1)
        .map_err(io::Error::other)?;
    state.signal_used_queue()
}

fn publish_tracked(
    mem: &GuestMemoryMmap,
    state: &mut VringState,
    completion: GuestCompletion,
    status: Status,
    data: Option<(&[Segment], &[u8])>,
    fault: &mut Fault,
    health: Option<&mut local::ImageState>,
) -> io::Result<()> {
    fault.set_snapshot(health.as_deref().and_then(local::ImageState::snapshot));
    let used = state.get_queue().next_used();
    let mut publish_used = || {
        publish(
            mem,
            state,
            completion.target,
            status,
            data,
            fault,
            completion.write_number,
        )
    };
    if let Some(entry) = completion.inflight {
        let health = health.ok_or_else(|| io::Error::other("completion lost its image lock"))?;
        let carrier = health
            .carrier
            .as_mut()
            .ok_or_else(|| io::Error::other("completion lost its carrier"))?;
        let result = if health.failure.is_some() {
            if status != Status::IoError {
                return Err(io::Error::other("success after image failure"));
            }
            carrier.complete_error(entry, used, publish_used)
        } else {
            carrier.complete(entry, used, publish_used)
        };
        if let Err(error) = &result {
            health.fail(error.to_string());
        }
        result
    } else {
        publish_used()
    }
}

fn decode_chain(
    mem: &GuestMemoryMmap,
    mut chain: virtio_queue::DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap>>,
    capacity: u64,
) -> io::Result<Request> {
    let head = chain.head_index();
    let mut descriptors = Vec::new();
    let mut has_next = false;
    for descriptor in chain.by_ref().take(CONCURRENT_QUEUE_SIZE) {
        has_next = descriptor.has_next();
        descriptors.push(Segment {
            addr: descriptor.addr(),
            len: descriptor.len() as usize,
            writable: descriptor.is_write_only(),
        });
    }
    if has_next {
        return Err(io::Error::other(
            "unterminated or oversized descriptor chain",
        ));
    }
    let completion = Completion::from_descriptors(mem, head, &descriptors);
    match request::parse(mem, head, descriptors, capacity) {
        Ok(request) => Ok(request),
        Err(_) => completion
            .map(Request::Invalid)
            .ok_or_else(|| io::Error::other("malformed request without writable status")),
    }
}

struct NextChain {
    chain: virtio_queue::DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap>>,
    next_avail: u16,
}

fn peek(
    mem: GuestMemoryLoadGuard<GuestMemoryMmap>,
    state: &mut VringState,
) -> io::Result<Option<NextChain>> {
    if !state.is_enabled() || !state.get_queue().ready() {
        return Ok(None);
    }
    let mut snapshot = Queue::try_from(state.get_queue().state()).map_err(io::Error::other)?;
    let chain = snapshot.iter(mem).map_err(io::Error::other)?.next();
    Ok(chain.map(|chain| NextChain {
        chain,
        next_avail: snapshot.next_avail(),
    }))
}

fn gather(
    mem: &GuestMemoryMmap,
    segments: &[Segment],
    destination: &mut [u8],
    copied: &mut u64,
) -> io::Result<()> {
    let mut offset = 0;
    for segment in segments {
        mem.read_slice(&mut destination[offset..offset + segment.len], segment.addr)
            .map_err(io::Error::other)?;
        *copied += segment.len as u64;
        offset += segment.len;
    }
    Ok(())
}

impl Backend {
    #[cfg(test)]
    pub fn new(path: &Path) -> io::Result<Self> {
        Self::open(path, false, None)
    }
    #[cfg(test)]
    pub fn open(path: &Path, staging: bool, create_bytes: Option<u64>) -> io::Result<Self> {
        Self::open_with_recovery(
            path,
            if staging {
                BackendKind::Staging
            } else {
                BackendKind::Raw
            },
            create_bytes,
            false,
            Fault::default(),
        )
    }
    pub fn open_with_recovery(
        path: &Path,
        kind: BackendKind,
        create_bytes: Option<u64>,
        restartable: bool,
        fault: Fault,
    ) -> io::Result<Self> {
        if restartable && !matches!(kind, BackendKind::Staging | BackendKind::LocalAsync) {
            return Err(io::Error::other(
                "restartable mode requires staging or local-async",
            ));
        }
        let live = restartable && matches!(kind, BackendKind::LocalAsync);
        let completion_event = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC)?;
        let storage = match kind {
            BackendKind::Staging => Storage::staging_with_durability(
                path,
                create_bytes,
                &completion_event,
                QUEUE_SIZE,
                restartable,
            )?,
            BackendKind::Raw => Storage::raw(path, &completion_event, QUEUE_SIZE)?,
            BackendKind::Local => Storage::local(path, create_bytes, &completion_event)?,
            BackendKind::LocalAsync if live => Storage::local_live(path, create_bytes)?,
            BackendKind::LocalAsync => Storage::local_async(path, create_bytes, &completion_event)?,
        };
        let capacity_bytes = storage.image_bytes();
        let exit = vmm_sys_util::event::new_event_consumer_and_notifier(
            EventFlag::NONBLOCK | EventFlag::CLOEXEC,
        )?;
        Ok(Self {
            storage,
            completion_event,
            exit,
            memory: None,
            capacity_bytes,
            pending: BTreeMap::new(),
            next_id: 0,
            counters: Counters::default(),
            shutdown: None,
            failure: None,
            negotiated_features: 0,
            restartable,
            live: live.then(Session::default),
            concurrent: matches!(kind, BackendKind::LocalAsync),
            first_queue: 0,
            queue_requests: [0; CONCURRENT_QUEUES],
            restored_used: None,
            restored_pending: 0,
            fault,
        })
    }
    pub fn set_shutdown_handle(&mut self, handle: ShutdownHandle) {
        self.shutdown = Some(handle);
    }
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }
    fn fail(&mut self, message: String) {
        if let Some(gate) = self.storage.completion_gate() {
            let mut failed = gate.lock().expect("completion gate poisoned");
            failed.fail(message.clone());
        }
        if self.failure.is_none() {
            self.failure = Some(message);
            self.counters.errors += 1;
        }
        if let Some(shutdown) = &self.shutdown {
            shutdown.shutdown();
        }
    }
    pub fn completion_fd(&self) -> RawFd {
        self.completion_event.as_raw_fd()
    }
    pub fn completion_token(&self) -> u16 {
        // Queue tokens precede the framework's exit event.
        (self.num_queues() + 1) as u16
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn report(&self, pending_at_disconnect: usize, connection_ok: bool) -> serde_json::Value {
        let c = &self.counters;
        serde_json::json!({
            "schema_version":1, "fatal_error":self.failure, "backend":self.storage.name(), "connection_ok":connection_ok,
            "inflight":self.live.as_ref().map(Session::report),
            "restartable":self.restartable, "restored_used":self.restored_used, "restored_pending":self.restored_pending,
            "local":self.storage.local_report(),
            "staging":self.storage.status().map(|s| serde_json::json!({
                "image_bytes":s.image_bytes, "appended":s.appended, "durable":s.durable,
                "log_bytes":s.log_bytes, "mapped_blocks":s.mapped_blocks,
                "recovered_tail_bytes":s.recovered_tail_bytes
            })),
            "negotiated_features":self.negotiated_features,
            "flush_negotiated":self.negotiated_features & (1 << VIRTIO_BLK_F_FLUSH) != 0,
            "pending_at_disconnect":pending_at_disconnect, "reads":c.reads, "writes":c.writes,
            "flushes":c.flushes, "read_bytes":c.read_bytes, "write_bytes":c.write_bytes,
            "guest_payload_copy_bytes":c.guest_payload_copy_bytes,
            "errors":c.errors, "bounce_requests":c.bounce_requests, "peak_inflight":c.peak_inflight,
            "queues":self.num_queues(), "queue_requests":&self.queue_requests[..self.num_queues()]
        })
    }
    fn finish(
        &mut self,
        mem: &GuestMemoryMmap,
        state: &mut VringState,
        completion: GuestCompletion,
        status: Status,
        data: Option<(&[Segment], &[u8])>,
        health: Option<&mut local::ImageState>,
    ) -> io::Result<()> {
        let result = publish_tracked(
            mem,
            state,
            completion,
            status,
            data,
            &mut self.fault,
            health,
        );
        if status != Status::Ok {
            self.counters.errors += 1;
        }
        result
    }
    fn enqueue(
        &mut self,
        mem: &GuestMemoryMmap,
        state: &mut VringState,
        admitted: Admitted,
        mut health: Option<&mut local::ImageState>,
    ) -> io::Result<()> {
        let Admitted {
            queue,
            id,
            request,
            permit,
            inflight,
        } = admitted;
        let write_number = matches!(&request, Request::Write(_)).then(|| self.fault.next_write());
        let completion = GuestCompletion {
            queue,
            target: request.completion(),
            inflight,
            write_number,
        };
        match &request {
            Request::GetId { segments, .. } => {
                return self.finish(
                    mem,
                    state,
                    completion,
                    Status::Ok,
                    Some((segments, DEVICE_ID)),
                    health,
                );
            }
            Request::Unsupported(_) => {
                return self.finish(mem, state, completion, Status::Unsupported, None, health);
            }
            Request::Invalid(_) => {
                return self.finish(mem, state, completion, Status::IoError, None, health);
            }
            _ => (),
        }
        // Install retirement before gathering or handing off ownership. A local
        // enqueue returns ownership through completion even if delivery fails.
        self.pending.insert(
            id,
            PendingRequest {
                completion,
                segments: Vec::new(),
                error_published: false,
            },
        );
        self.counters.peak_inflight = self.counters.peak_inflight.max(self.pending.len());
        let has_buffer = !matches!(&request, Request::Flush(_));
        let mut owned = false;
        let result = (|| {
            self.fault
                .set_snapshot(health.as_deref().and_then(local::ImageState::snapshot));
            self.fault.hit(Point::BeforeSubmit, write_number)?;
            let operation = match request {
                Request::Read(data) => {
                    self.pending.get_mut(&id).unwrap().segments = data.segments;
                    Operation::Read {
                        offset: data.offset,
                        buffer: AlignedBuffer::new(data.len),
                    }
                }
                Request::Write(data) if self.storage.is_local() => {
                    self.storage.gather(
                        id,
                        QueueHead {
                            queue,
                            head: data.completion.head,
                        },
                        data.offset,
                        data.len,
                        permit,
                        |destination| {
                            gather(
                                mem,
                                &data.segments,
                                destination,
                                &mut self.counters.guest_payload_copy_bytes,
                            )
                        },
                    )?;
                    owned = true;
                    return Ok(());
                }
                Request::Write(data) => {
                    let mut buffer = AlignedBuffer::new(data.len);
                    gather(
                        mem,
                        &data.segments,
                        buffer.as_mut_slice(),
                        &mut self.counters.guest_payload_copy_bytes,
                    )?;
                    Operation::Write {
                        offset: data.offset,
                        buffer,
                    }
                }
                Request::Flush(_) => Operation::Flush,
                _ => unreachable!("immediate requests returned before storage admission"),
            };
            let result = self.storage.enqueue_owned(id, operation, permit);
            owned = self.storage.is_local() || result.is_ok();
            result
        })();
        if let Err(error) = result {
            if !owned {
                self.pending
                    .remove(&id)
                    .expect("untransferred retirement record");
                if let Some(health) = health.as_deref_mut() {
                    health.fail(error.to_string());
                }
                if let Err(retirement) =
                    self.finish(mem, state, completion, Status::IoError, None, health)
                {
                    return Err(io::Error::other(format!(
                        "{error}; failed retirement: {retirement}"
                    )));
                }
            }
            return Err(error);
        }
        if has_buffer {
            self.counters.bounce_requests += 1;
        }
        Ok(())
    }
    fn complete(&mut self, mem: &GuestMemoryMmap, vrings: &[VringMutex]) -> io::Result<()> {
        loop {
            let Some(completed) = self.storage.try_complete()? else {
                return Ok(());
            };
            let pending = self
                .pending
                .remove(&completed.id)
                .ok_or_else(|| io::Error::other("unknown IO completion"))?;
            let mut queue = vrings[usize::from(pending.completion.queue)].get_mut();
            let state = &mut *queue;
            let gate = self.storage.completion_gate();
            let mut guard = gate
                .as_ref()
                .map(|gate| {
                    gate.lock()
                        .map_err(|_| io::Error::other("completion gate poisoned"))
                })
                .transpose()?;
            let failure = guard
                .as_ref()
                .and_then(|guard| guard.failure.as_ref())
                .map(|error| io::Error::other(error.clone()));
            if let Err(error) = failure.map_or(completed.result, Err) {
                // Completed owns the permit after its data field. Its credit
                // survives the READ buffer, including failed status publication.
                drop(completed.data);
                self.finish(
                    mem,
                    state,
                    pending.completion,
                    Status::IoError,
                    None,
                    guard.as_deref_mut(),
                )?;
                if self.storage.status().is_some() || self.storage.is_local() {
                    return Err(error);
                }
                continue;
            }
            self.fault
                .set_snapshot(guard.as_deref().and_then(local::ImageState::snapshot));
            match completed.data {
                CompletionData::Read(buffer) => {
                    self.finish(
                        mem,
                        state,
                        pending.completion,
                        Status::Ok,
                        Some((&pending.segments, buffer.as_slice())),
                        guard.as_deref_mut(),
                    )?;
                    self.counters.reads += 1;
                    self.counters.read_bytes += buffer.as_slice().len() as u64;
                }
                CompletionData::Write { bytes } => {
                    self.fault
                        .hit(Point::AfterStorage, pending.completion.write_number)?;
                    self.finish(
                        mem,
                        state,
                        pending.completion,
                        Status::Ok,
                        None,
                        guard.as_deref_mut(),
                    )?;
                    self.fault
                        .hit(Point::AfterUsed, pending.completion.write_number)?;
                    self.counters.writes += 1;
                    self.counters.write_bytes += bytes as u64;
                }
                CompletionData::Flush => {
                    self.finish(
                        mem,
                        state,
                        pending.completion,
                        Status::Ok,
                        None,
                        guard.as_deref_mut(),
                    )?;
                    self.counters.flushes += 1;
                }
            }
        }
    }
    fn process(&mut self, vrings: &[VringMutex]) -> io::Result<()> {
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(failure.clone()));
        }
        if let Some(gate) = self.storage.completion_gate()
            && let Some(error) = gate
                .lock()
                .expect("completion gate poisoned")
                .failure
                .as_ref()
        {
            return Err(io::Error::other(error.clone()));
        }
        if self.negotiated_features & REQUIRED_FEATURES != REQUIRED_FEATURES {
            return Err(io::Error::other("IO before required feature negotiation"));
        }
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| io::Error::other("guest memory missing"))?
            .clone();
        if !self.activate_attachment(&mem, vrings)? {
            return Ok(());
        }
        self.complete(&mem, vrings)?;
        let count = vrings.len();
        let first = self.first_queue % count;
        self.first_queue = (first + 1) % count;
        for offset in 0..count {
            let queue = (first + offset) % count;
            self.admit_queue(&mem, queue as u16, &vrings[queue])?;
        }
        self.storage.submit()
    }

    fn admit_queue(
        &mut self,
        mem: &GuestMemoryLoadGuard<GuestMemoryMmap>,
        queue: u16,
        vring: &VringMutex,
    ) -> io::Result<()> {
        let mut state = vring.get_mut();
        if !state.is_enabled() || !state.get_queue().ready() {
            return Ok(());
        }
        if self.restartable && self.live.is_none() && self.restored_used.is_none() {
            if !state.is_enabled() || !state.get_queue().ready() {
                return Ok(());
            }
            let queue = state.get_queue_mut();
            let used = queue
                .used_idx(&**mem, Ordering::Acquire)
                .map_err(io::Error::other)?
                .0;
            let available = queue
                .avail_idx(&**mem, Ordering::Acquire)
                .map_err(io::Error::other)?
                .0;
            let outstanding = available.wrapping_sub(used);
            if outstanding > queue.size() {
                return Err(io::Error::other("invalid restartable queue distance"));
            }
            // Exactly one request may execute before its used entry is published.
            // Thus used.idx is also the consumption cursor, including after wrap.
            // Writes are durable before publication; replay of the unpublished
            // request cannot overwrite a later completed request.
            queue.set_next_used(used);
            queue.set_next_avail(used);
            self.restored_used = Some(used);
            self.restored_pending = outstanding;
        }
        let mut consumed = 0;
        let limit = if self.restartable && self.live.is_none() {
            1
        } else if self.concurrent {
            128 + 8 // Image-wide bulk and separately reserved control requests.
        } else {
            QUEUE_SIZE
        };
        let queue_size = usize::from(state.get_queue().size());
        while self.pending.len() < limit && consumed < queue_size {
            let Some(NextChain { chain, next_avail }) = peek(mem.clone(), &mut state)? else {
                break;
            };
            let request = decode_chain(mem, chain, self.capacity_bytes)?;
            let next_id = self
                .next_id
                .checked_add(1)
                .ok_or_else(|| io::Error::other("request IDs exhausted"))?;
            let gate = self.storage.completion_gate();
            let mut guard = gate
                .as_ref()
                .map(|gate| {
                    gate.lock()
                        .map_err(|_| io::Error::other("completion gate poisoned"))
                })
                .transpose()?;
            if let Some(error) = guard.as_ref().and_then(|guard| guard.failure.as_ref()) {
                return Err(io::Error::other(error.clone()));
            }
            let Some(permit) = self.storage.prepare(request.admission_kind())? else {
                break;
            };
            let inflight = guard
                .as_deref_mut()
                .and_then(|state| state.carrier.as_mut())
                .map(|carrier| carrier.admit(request.inflight(queue, next_avail.wrapping_sub(1))))
                .transpose()?;
            if inflight.is_some_and(|entry| entry.serial != next_id) {
                return Err(io::Error::other(
                    "admission serial differs from retained carrier",
                ));
            }
            let id = self.next_id;
            self.next_id = next_id;
            state.get_queue_mut().set_next_avail(next_avail);
            if matches!(
                &request,
                Request::Read(_) | Request::Write(_) | Request::Flush(_)
            ) {
                self.queue_requests[usize::from(queue)] += 1;
            }
            self.enqueue(
                mem,
                &mut state,
                Admitted {
                    queue,
                    id,
                    request,
                    permit,
                    inflight,
                },
                guard.as_deref_mut(),
            )?;
            consumed += 1;
        }
        if consumed == queue_size {
            // Immediate responses do not fill pending. Yield the mutex anyway,
            // and self-wake so a consumed/coalesced kick cannot strand requests.
            self.completion_event.write(1)?;
        }
        Ok(())
    }
    /// Reap IO after disconnect without touching guest queues or memory.
    pub fn drain(&mut self) -> io::Result<()> {
        // An earlier gather may still own an unsealed batch when admission fails.
        if let Err(error) = self.storage.submit() {
            self.fail(error.to_string());
        }
        while !self.pending.is_empty() {
            let completed = match self.storage.wait_complete() {
                Ok(completed) => completed,
                Err(error) => {
                    self.fail(error.to_string());
                    return Err(error);
                }
            };
            let pending = self
                .pending
                .remove(&completed.id)
                .ok_or_else(|| io::Error::other("unknown IO completion while draining"))?;
            if let Err(error) = completed.result {
                self.fail(error.to_string());
            }
            drop(completed.data);
            drop(pending);
        }
        self.storage.finish()
    }

    fn fail_pending(&mut self, vrings: &[VringMutex]) {
        let Some(gate) = self.storage.completion_gate() else {
            return;
        };
        let mut guard = gate.lock().expect("completion gate poisoned");
        if guard.failure.is_none() {
            return;
        }
        let Some(memory) = &self.memory else {
            return;
        };
        for pending in self.pending.values_mut() {
            let mut state = vrings[usize::from(pending.completion.queue)].get_mut();
            if !pending.error_published
                && publish_tracked(
                    memory,
                    &mut state,
                    pending.completion,
                    Status::IoError,
                    None,
                    &mut self.fault,
                    Some(&mut guard),
                )
                .is_ok()
            {
                pending.error_published = true;
                self.counters.errors += 1;
            }
        }
        // Keep these entries for drain(), while the worker retains its kernel
        // allocations and permits. No later completion writes guest memory.
    }
}

impl VhostUserBackendMut for Backend {
    type Bitmap = ();
    type Vring = VringMutex;
    fn num_queues(&self) -> usize {
        if self.concurrent {
            CONCURRENT_QUEUES
        } else {
            1
        }
    }
    fn max_queue_size(&self) -> usize {
        if self.concurrent {
            CONCURRENT_QUEUE_SIZE
        } else {
            QUEUE_SIZE
        }
    }
    fn features(&self) -> u64 {
        use vhost::vhost_user::message::VhostUserVirtioFeatures;
        use virtio_bindings::bindings::{
            virtio_blk::{VIRTIO_BLK_F_SEG_MAX, VIRTIO_BLK_F_SIZE_MAX},
            virtio_ring::VIRTIO_RING_F_INDIRECT_DESC,
        };
        (1 << VIRTIO_BLK_F_SIZE_MAX)
            | (1 << VIRTIO_BLK_F_SEG_MAX)
            | REQUIRED_FEATURES
            | if self.concurrent {
                1 << VIRTIO_BLK_F_MQ
            } else {
                0
            }
            | (1 << VIRTIO_RING_F_INDIRECT_DESC)
            | VhostUserVirtioFeatures::PROTOCOL_FEATURES.bits()
    }
    fn acked_features(&mut self, features: u64) {
        self.negotiated_features = features;
        if features & REQUIRED_FEATURES != REQUIRED_FEATURES {
            // This research adapter supports a modern Linux guest with 4 KiB
            // blocks and writeback caching. Refuse unsupported negotiation
            // before IO rather than acknowledge unsynchronized writes as stable.
            self.fail("guest must negotiate VERSION_1, BLK_SIZE, and FLUSH".into());
        }
    }
    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        let mut features = VhostUserProtocolFeatures::CONFIG;
        if self.concurrent {
            features |= VhostUserProtocolFeatures::MQ;
        }
        if self.live.is_some() {
            features |=
                VhostUserProtocolFeatures::INFLIGHT_SHMFD | VhostUserProtocolFeatures::REPLY_ACK;
        }
        features
    }
    fn get_inflight_fd(
        &mut self,
        message: &vhost::vhost_user::message::VhostUserInflight,
    ) -> io::Result<(vhost::vhost_user::message::VhostUserInflight, std::fs::File)> {
        self.create_attachment(message)
    }
    fn set_inflight_fd(
        &mut self,
        message: &vhost::vhost_user::message::VhostUserInflight,
        file: std::fs::File,
    ) -> io::Result<()> {
        self.restore_attachment(message, file)
    }
    fn set_event_idx(&mut self, _: bool) {}
    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let mut config = [0; 60];
        config[..8].copy_from_slice(&(self.capacity_bytes / request::SECTOR_BYTES).to_le_bytes());
        config[8..12].copy_from_slice(&(MAX_REQUEST_BYTES as u32).to_le_bytes());
        config[12..16].copy_from_slice(&((self.max_queue_size() - 2) as u32).to_le_bytes());
        config[20..24].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
        config[34..36].copy_from_slice(&(self.num_queues() as u16).to_le_bytes());
        let start = offset as usize;
        let Some(end) = start.checked_add(size as usize) else {
            return Vec::new();
        };
        config.get(start..end).unwrap_or(&[]).to_vec()
    }
    fn update_memory(&mut self, memory: GuestMemoryAtomic<GuestMemoryMmap>) -> io::Result<()> {
        if !self.pending.is_empty() {
            return Err(io::Error::other("memory replacement with IO in flight"));
        }
        self.memory = Some(memory.memory());
        Ok(())
    }
    fn exit_event(&self, _: usize) -> Option<(EventConsumer, EventNotifier)> {
        Some((
            self.exit.0.try_clone().expect("clone exit consumer"),
            self.exit.1.try_clone().expect("clone exit notifier"),
        ))
    }
    fn handle_event(
        &mut self,
        event: u16,
        events: EventSet,
        vrings: &[VringMutex],
        _: usize,
    ) -> io::Result<()> {
        let result = (|| {
            if events != EventSet::IN {
                return Err(io::Error::other("unexpected epoll event"));
            }
            match event {
                queue if usize::from(queue) < self.num_queues() => (),
                completion if completion == self.completion_token() => {
                    match self.completion_event.read() {
                        Ok(_) => (),
                        Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                        Err(error) => return Err(error),
                    }
                }
                _ => return Err(io::Error::other("unknown event token")),
            }
            self.process(vrings)
        })();
        if let Err(error) = &result {
            self.fail(error.to_string());
            self.fail_pending(vrings);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use vm_memory::GuestAddress;

    fn queue() -> (GuestMemoryAtomic<GuestMemoryMmap>, VringMutex) {
        let mem = GuestMemoryAtomic::new(
            GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap(),
        );
        let vring = VringMutex::new(mem.clone(), QUEUE_SIZE as u16).unwrap();
        vring.set_queue_size(QUEUE_SIZE as u16);
        vring.set_queue_info(0x1000, 0x2000, 0x3000).unwrap();
        vring.set_queue_ready(true);
        vring.set_enabled(true);
        (mem, vring)
    }

    #[test]
    fn renegotiation_cannot_resume_a_failed_backend() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("image.raw");
        File::create(&path)
            .unwrap()
            .set_len(BLOCK_SIZE as u64)
            .unwrap();
        let mut backend = Backend::new(&path).unwrap();
        let (memory, vring) = queue();
        let mem = memory.memory();
        mem.write_obj(0x6000u64, vm_memory::GuestAddress(0x1000))
            .unwrap();
        mem.write_obj(16u32, vm_memory::GuestAddress(0x1008))
            .unwrap();
        mem.write_obj(1u16, vm_memory::GuestAddress(0x2002))
            .unwrap();
        backend.update_memory(memory).unwrap();
        backend.acked_features(backend.features() & !(1 << VIRTIO_BLK_F_FLUSH));
        backend.acked_features(backend.features());
        assert!(
            backend
                .handle_event(0, EventSet::IN, std::slice::from_ref(&vring), 0)
                .is_err()
        );
        assert_eq!(
            backend.failure(),
            Some("guest must negotiate VERSION_1, BLK_SIZE, and FLUSH")
        );
        assert_eq!(vring.queue_next_avail(), 0);
        assert_eq!(backend.pending_count(), 0);
    }

    #[test]
    fn completion_uses_accepted_memory_for_payload_status_and_used_ring() {
        let (atomic, vring) = queue();
        let accepted = atomic.memory();
        // This is the upstream ordering: replace the atomic map before the
        // backend callback has accepted it. An old request must stay on map A.
        atomic
            .lock()
            .unwrap()
            .replace(GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap());
        let replacement = atomic.memory();
        let completion = Completion {
            head: 7,
            status: GuestAddress(0x5000),
        };
        let segments = [Segment {
            addr: GuestAddress(0x6000),
            len: BLOCK_SIZE,
            writable: true,
        }];
        accepted.write_obj(0xffu8, completion.status).unwrap();
        publish(
            &accepted,
            &mut vring.get_mut(),
            completion,
            Status::Ok,
            Some((&segments, &[0x5a; BLOCK_SIZE])),
            &mut Fault::default(),
            None,
        )
        .unwrap();
        assert_eq!(accepted.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0x5a);
        assert_eq!(
            accepted.read_obj::<u8>(completion.status).unwrap(),
            Status::Ok as u8
        );
        assert_eq!(accepted.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 1);
        assert_eq!(accepted.read_obj::<u32>(GuestAddress(0x3004)).unwrap(), 7);
        assert_eq!(replacement.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0);
        assert_eq!(
            replacement.read_obj::<u16>(GuestAddress(0x3002)).unwrap(),
            0
        );
    }

    #[test]
    fn inactive_queue_neither_publishes_nor_consumes_available_descriptors() {
        for stopped in [false, true] {
            let (atomic, vring) = queue();
            let mem = atomic.memory();
            // One valid available descriptor. If popped, next_avail changes.
            mem.write_obj(0x6000u64, GuestAddress(0x1000)).unwrap();
            mem.write_obj(16u32, GuestAddress(0x1008)).unwrap();
            mem.write_obj(1u16, GuestAddress(0x2002)).unwrap();
            if stopped {
                vring.set_queue_ready(false);
            } else {
                vring.set_enabled(false);
            }
            let completion = Completion {
                head: 0,
                status: GuestAddress(0x5000),
            };
            mem.write_obj(0xffu8, completion.status).unwrap();
            let segments = [Segment {
                addr: GuestAddress(0x6000),
                len: BLOCK_SIZE,
                writable: true,
            }];
            assert!(
                publish(
                    &mem,
                    &mut vring.get_mut(),
                    completion,
                    Status::Ok,
                    Some((&segments, &[0x5a; BLOCK_SIZE])),
                    &mut Fault::default(),
                    None,
                )
                .is_err()
            );
            assert!(peek(mem.clone(), &mut vring.get_mut()).unwrap().is_none());
            assert_eq!(vring.queue_next_avail(), 0);
            assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 0);
            assert_eq!(mem.read_obj::<u8>(completion.status).unwrap(), 0xff);
            assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0);
            vring.set_queue_ready(true);
            vring.set_enabled(true);
            let next = peek(mem, &mut vring.get_mut()).unwrap().unwrap();
            assert_eq!(vring.queue_next_avail(), 0);
            vring
                .get_mut()
                .get_queue_mut()
                .set_next_avail(next.next_avail);
            assert_eq!(vring.queue_next_avail(), 1);
        }
    }
    #[test]
    fn failed_gather_retires_its_head_and_drains_an_earlier_unsealed_batch() {
        use cas_daemon::inflight::{Carrier, Geometry, Identity};
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut backend = Backend::open_with_recovery(
            &directory.path().join("log"),
            BackendKind::LocalAsync,
            Some(BLOCK_SIZE as u64),
            false,
            Fault::default(),
        )
        .unwrap();
        let (memory, vring) = queue();
        backend.update_memory(memory.clone()).unwrap();
        let mem = memory.memory();
        let gate = backend.storage.completion_gate().unwrap();
        let mut carrier = Carrier::create(
            Geometry::new(1, QUEUE_SIZE as u16).unwrap(),
            Identity {
                store: [1; 16],
                image: [2; 16],
                epoch: 1,
                attachment: 1,
            },
            BLOCK_SIZE as u64,
            0,
        )
        .unwrap();
        carrier.initialize_queue(0, 0, 0).unwrap();
        gate.lock().unwrap().carrier = Some(carrier);
        for (id, address) in [(0, 0x6000), (1, 0x10000)] {
            let request = Request::Write(request::DataRequest {
                completion: Completion {
                    head: id as u16,
                    status: GuestAddress(0x5100 + id),
                },
                offset: 0,
                len: BLOCK_SIZE,
                segments: vec![Segment {
                    addr: GuestAddress(address),
                    len: BLOCK_SIZE,
                    writable: false,
                }],
            });
            mem.write_obj(0xffu8, request.completion().status).unwrap();
            let permit = backend
                .storage
                .prepare(request.admission_kind())
                .unwrap()
                .unwrap();
            let mut health = gate.lock().unwrap();
            let entry = health
                .carrier
                .as_mut()
                .unwrap()
                .admit(request.inflight(0, id as u16))
                .unwrap();
            let mut state = vring.get_mut();
            state.get_queue_mut().set_next_avail(id as u16 + 1);
            let result = backend.enqueue(
                &mem,
                &mut state,
                Admitted {
                    id,
                    queue: 0,
                    request,
                    permit,
                    inflight: Some(entry),
                },
                Some(&mut health),
            );
            assert_eq!(result.is_ok(), id == 0);
            if id == 1 {
                assert!(health.failure.is_some());
                assert_eq!(
                    mem.read_obj::<u8>(GuestAddress(0x5101)).unwrap(),
                    Status::IoError as u8
                );
                assert_eq!(health.carrier.as_ref().unwrap().published(), 0);
            }
        }
        assert_eq!(backend.pending_count(), 1); // Only the batch still owns an operation.
        backend.fail("injected guest gather failure".into());
        backend.fail_pending(std::slice::from_ref(&vring));
        backend.drain().unwrap();
        assert_eq!(backend.pending_count(), 0);
        assert_eq!(
            mem.read_obj::<u8>(GuestAddress(0x5100)).unwrap(),
            Status::IoError as u8
        );
        assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 2);
        let report = backend.storage.local_report().unwrap();
        assert_eq!(report["status"]["published"], 0);
        assert_eq!(report["append"]["current"]["bytes"], 0);
        assert_eq!(report["requests"]["current"]["requests"], 0);
    }
}
