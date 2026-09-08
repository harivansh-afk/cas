// Queue execution: retain owned IO until completion and publish against one memory snapshot.

use std::collections::BTreeMap;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::path::Path;
use std::sync::atomic::Ordering;

use cas_core::{BLOCK_SIZE, MAX_REQUEST_BYTES, aligned::AlignedBuffer};
use vhost::vhost_user::message::VhostUserProtocolFeatures;
use vhost_user_backend::{ShutdownHandle, VhostUserBackendMut, VringMutex, VringState, VringT};
use virtio_bindings::bindings::{
    virtio_blk::{VIRTIO_BLK_F_BLK_SIZE, VIRTIO_BLK_F_FLUSH},
    virtio_config::VIRTIO_F_VERSION_1,
};
use virtio_queue::QueueT;
use vm_memory::{
    Bytes, GuestAddressSpace, GuestMemoryAtomic, GuestMemoryLoadGuard, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::event::{EventConsumer, EventFlag, EventNotifier};
use vmm_sys_util::eventfd::{EFD_CLOEXEC, EFD_NONBLOCK, EventFd};

use crate::fault::{Fault, Point};
use crate::request::{self, Completion, DEVICE_ID, Request, Segment, Status};
use crate::storage::{Operation, Storage};

const QUEUE_SIZE: usize = 128;
const REQUIRED_FEATURES: u64 =
    (1 << VIRTIO_F_VERSION_1) | (1 << VIRTIO_BLK_F_BLK_SIZE) | (1 << VIRTIO_BLK_F_FLUSH);
pub(super) const COMPLETION_EVENT: u16 = 2; // 0 is the queue; 1 is the framework's exit event.

struct PendingRequest {
    completion: Completion,
    segments: Vec<Segment>,
}

#[derive(Default)]
struct Counters {
    reads: u64,
    writes: u64,
    flushes: u64,
    read_bytes: u64,
    write_bytes: u64,
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
    restored_used: Option<u16>,
    restored_pending: u16,
    fault: Option<Fault>,
}

/// The queue lock serializes publication with SET_VRING_ENABLE/GET_VRING_BASE.
/// Pass the accepted map explicitly: VringT::add_used loads the mutable shared map.
fn publish(
    mem: &GuestMemoryMmap,
    state: &mut VringState,
    completion: Completion,
    status: Status,
    data: Option<(&[Segment], &[u8])>,
    fault: Option<&mut Fault>,
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
    if let Some(fault) = fault {
        fault.hit(Point::AfterStatus)?;
    }
    state
        .get_queue_mut()
        .add_used(mem, completion.head, written as u32 + 1)
        .map_err(io::Error::other)?;
    state.signal_used_queue()
}

fn pop(
    mem: GuestMemoryLoadGuard<GuestMemoryMmap>,
    state: &mut VringState,
) -> Option<virtio_queue::DescriptorChain<GuestMemoryLoadGuard<GuestMemoryMmap>>> {
    if !state.is_enabled() || !state.get_queue().ready() {
        return None;
    }
    state.get_queue_mut().pop_descriptor_chain(mem)
}

impl Backend {
    #[cfg(test)]
    pub fn new(path: &Path) -> io::Result<Self> {
        Self::open(path, false, None)
    }
    #[cfg(test)]
    pub fn open(path: &Path, staging: bool, create_bytes: Option<u64>) -> io::Result<Self> {
        Self::open_with_recovery(path, staging, create_bytes, false, None)
    }
    pub fn open_with_recovery(
        path: &Path,
        staging: bool,
        create_bytes: Option<u64>,
        restartable: bool,
        fault: Option<Fault>,
    ) -> io::Result<Self> {
        if restartable && !staging {
            return Err(io::Error::other("restartable mode requires staging"));
        }
        let completion_event = EventFd::new(EFD_NONBLOCK | EFD_CLOEXEC)?;
        let storage = if staging {
            Storage::staging_with_durability(
                path,
                create_bytes,
                &completion_event,
                QUEUE_SIZE,
                restartable,
            )?
        } else {
            Storage::raw(path, &completion_event, QUEUE_SIZE)?
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
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn report(&self, pending_at_disconnect: usize, connection_ok: bool) -> serde_json::Value {
        let c = &self.counters;
        serde_json::json!({
            "schema_version":1, "fatal_error":self.failure, "backend":self.storage.name(), "connection_ok":connection_ok,
            "restartable":self.restartable, "restored_used":self.restored_used, "restored_pending":self.restored_pending,
            "staging":self.storage.status().map(|s| serde_json::json!({
                "image_bytes":s.image_bytes, "appended":s.appended, "durable":s.durable,
                "log_bytes":s.log_bytes, "mapped_blocks":s.mapped_blocks,
                "recovered_tail_bytes":s.recovered_tail_bytes
            })),
            "negotiated_features":self.negotiated_features,
            "flush_negotiated":self.negotiated_features & (1 << VIRTIO_BLK_F_FLUSH) != 0,
            "pending_at_disconnect":pending_at_disconnect, "reads":c.reads, "writes":c.writes,
            "flushes":c.flushes, "read_bytes":c.read_bytes, "write_bytes":c.write_bytes,
            "errors":c.errors, "bounce_requests":c.bounce_requests, "peak_inflight":c.peak_inflight, "queues":1
        })
    }
    fn finish(
        &mut self,
        mem: &GuestMemoryMmap,
        state: &mut VringState,
        completion: Completion,
        status: Status,
        data: Option<(&[Segment], &[u8])>,
    ) -> io::Result<()> {
        let result = publish(mem, state, completion, status, data, self.fault.as_mut());
        if status != Status::Ok {
            self.counters.errors += 1;
        }
        result
    }
    fn enqueue(
        &mut self,
        mem: &GuestMemoryMmap,
        state: &mut VringState,
        request: Request,
    ) -> io::Result<()> {
        if matches!(&request, Request::Write(_))
            && let Some(fault) = &mut self.fault
        {
            fault.next_write();
            fault.hit(Point::BeforeSubmit)?;
        }
        let (completion, segments, operation) = match request {
            Request::GetId {
                completion,
                segments,
            } => {
                return self.finish(
                    mem,
                    state,
                    completion,
                    Status::Ok,
                    Some((&segments, DEVICE_ID)),
                );
            }
            Request::Unsupported(completion) => {
                return self.finish(mem, state, completion, Status::Unsupported, None);
            }
            Request::Read(data) => (
                data.completion,
                data.segments,
                Operation::Read {
                    offset: data.offset,
                    buffer: AlignedBuffer::new(data.len),
                },
            ),
            Request::Write(data) => {
                let mut buffer = AlignedBuffer::new(data.len);
                let mut offset = 0;
                for segment in &data.segments {
                    mem.read_slice(
                        &mut buffer.as_mut_slice()[offset..offset + segment.len],
                        segment.addr,
                    )
                    .map_err(io::Error::other)?;
                    offset += segment.len;
                }
                (
                    data.completion,
                    Vec::new(),
                    Operation::Write {
                        offset: data.offset,
                        buffer,
                    },
                )
            }
            Request::Flush(completion) => (completion, Vec::new(), Operation::Flush),
        };
        let next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| io::Error::other("request IDs exhausted"))?;
        let has_buffer = !matches!(operation, Operation::Flush);
        self.storage.enqueue(self.next_id, operation)?;
        if has_buffer {
            self.counters.bounce_requests += 1;
        }
        self.pending.insert(
            self.next_id,
            PendingRequest {
                completion,
                segments,
            },
        );
        self.next_id = next_id;
        self.counters.peak_inflight = self.counters.peak_inflight.max(self.pending.len());
        Ok(())
    }
    fn complete(&mut self, mem: &GuestMemoryMmap, state: &mut VringState) -> io::Result<()> {
        loop {
            let Some(completed) = self.storage.try_complete()? else {
                return Ok(());
            };
            let pending = self
                .pending
                .remove(&completed.id)
                .ok_or_else(|| io::Error::other("unknown IO completion"))?;
            let expected = completed.operation.expected_bytes();
            if let Err(error) = completed.result {
                self.finish(mem, state, pending.completion, Status::IoError, None)?;
                if self.storage.status().is_some() {
                    return Err(error);
                }
                continue;
            }
            match completed.operation {
                Operation::Read { buffer, .. } => {
                    self.finish(
                        mem,
                        state,
                        pending.completion,
                        Status::Ok,
                        Some((&pending.segments, buffer.as_slice())),
                    )?;
                    self.counters.reads += 1;
                    self.counters.read_bytes += expected as u64;
                }
                Operation::Write { .. } => {
                    if let Some(fault) = &mut self.fault {
                        fault.hit(Point::AfterStorage)?;
                    }
                    self.finish(mem, state, pending.completion, Status::Ok, None)?;
                    if let Some(fault) = &mut self.fault {
                        fault.hit(Point::AfterUsed)?;
                    }
                    self.counters.writes += 1;
                    self.counters.write_bytes += expected as u64;
                }
                Operation::Flush => {
                    self.finish(mem, state, pending.completion, Status::Ok, None)?;
                    self.counters.flushes += 1;
                }
            }
        }
    }
    fn process(&mut self, vring: &VringMutex) -> io::Result<()> {
        if let Some(failure) = &self.failure {
            return Err(io::Error::other(failure.clone()));
        }
        if self.negotiated_features & REQUIRED_FEATURES != REQUIRED_FEATURES {
            return Err(io::Error::other("IO before required feature negotiation"));
        }
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| io::Error::other("guest memory missing"))?
            .clone();
        let mut state = vring.get_mut();
        if self.restartable && self.restored_used.is_none() {
            if !state.is_enabled() || !state.get_queue().ready() {
                return Ok(());
            }
            let queue = state.get_queue_mut();
            let used = queue
                .used_idx(&*mem, Ordering::Acquire)
                .map_err(io::Error::other)?
                .0;
            let available = queue
                .avail_idx(&*mem, Ordering::Acquire)
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
        self.complete(&mem, &mut state)?;
        let mut consumed = 0;
        let limit = if self.restartable { 1 } else { QUEUE_SIZE };
        while self.pending.len() < limit && consumed < QUEUE_SIZE {
            let Some(mut chain) = pop(mem.clone(), &mut state) else {
                break;
            };
            consumed += 1;
            let head = chain.head_index();
            let mut descriptors = Vec::new();
            let mut has_next = false;
            // Bound indirect tables too, not just the outer queue length.
            for descriptor in chain.by_ref().take(QUEUE_SIZE) {
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
            let completion = Completion::from_descriptors(&mem, head, &descriptors);
            match request::parse(&mem, head, descriptors, self.capacity_bytes) {
                Ok(request) => self.enqueue(&mem, &mut state, request)?,
                Err(_) => match completion {
                    Some(completion) => {
                        self.finish(&mem, &mut state, completion, Status::IoError, None)?
                    }
                    None => {
                        return Err(io::Error::other(
                            "malformed request without writable status",
                        ));
                    }
                },
            }
        }
        self.storage.submit()?;
        if consumed == QUEUE_SIZE {
            // Immediate responses do not fill pending. Yield the mutex anyway,
            // and self-wake so a consumed/coalesced kick cannot strand requests.
            self.completion_event.write(1)?;
        }
        Ok(())
    }
    /// Reap IO after disconnect without touching guest queues or memory.
    pub fn drain(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            let completed = match self.storage.wait_complete() {
                Ok(completed) => completed,
                Err(error) => {
                    self.fail(error.to_string());
                    return Err(error);
                }
            };
            self.pending
                .remove(&completed.id)
                .ok_or_else(|| io::Error::other("unknown IO completion while draining"))?;
            if let Err(error) = completed.result {
                self.fail(error.to_string());
            }
        }
        Ok(())
    }
}

impl VhostUserBackendMut for Backend {
    type Bitmap = ();
    type Vring = VringMutex;
    fn num_queues(&self) -> usize {
        1
    }
    fn max_queue_size(&self) -> usize {
        QUEUE_SIZE
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
        VhostUserProtocolFeatures::CONFIG
    }
    fn set_event_idx(&mut self, _: bool) {}
    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let mut config = [0; 60];
        config[..8].copy_from_slice(&(self.capacity_bytes / request::SECTOR_BYTES).to_le_bytes());
        config[8..12].copy_from_slice(&(MAX_REQUEST_BYTES as u32).to_le_bytes());
        config[12..16].copy_from_slice(&((QUEUE_SIZE - 2) as u32).to_le_bytes());
        config[20..24].copy_from_slice(&(BLOCK_SIZE as u32).to_le_bytes());
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
                0 => (), // The framework already consumed the kick.
                COMPLETION_EVENT => match self.completion_event.read() {
                    Ok(_) => (),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => (),
                    Err(error) => return Err(error),
                },
                _ => return Err(io::Error::other("unknown event token")),
            }
            self.process(&vrings[0])
        })();
        if let Err(error) = &result {
            self.fail(error.to_string());
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
                    None,
                )
                .is_err()
            );
            assert!(pop(mem.clone(), &mut vring.get_mut()).is_none());
            assert_eq!(vring.queue_next_avail(), 0);
            assert_eq!(mem.read_obj::<u16>(GuestAddress(0x3002)).unwrap(), 0);
            assert_eq!(mem.read_obj::<u8>(completion.status).unwrap(), 0xff);
            assert_eq!(mem.read_obj::<u8>(GuestAddress(0x6000)).unwrap(), 0);
            vring.set_queue_ready(true);
            vring.set_enabled(true);
            assert!(pop(mem, &mut vring.get_mut()).is_some());
            assert_eq!(vring.queue_next_avail(), 1);
        }
    }
}
