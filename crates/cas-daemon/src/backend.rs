use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use io_uring::{IoUring, opcode, squeue, types};
use vhost::vhost_user::message::VhostUserProtocolFeatures;
use vhost_user_backend::{VhostUserBackendMut, VringMutex, VringT};
use virtio_queue::{DescriptorChain, QueueT};
use vm_memory::{
    Bytes, GuestAddress, GuestAddressSpace, GuestMemory, GuestMemoryAtomic, GuestMemoryMmap,
};
use vmm_sys_util::epoll::EventSet;
use vmm_sys_util::event::{EventConsumer, EventNotifier};
use vmm_sys_util::eventfd::{EFD_NONBLOCK, EventFd};

const BLOCK: usize = 4096;
const MAX_IO: usize = 1024 * 1024;
const QUEUE_SIZE: usize = 128;
const IN: u32 = 0;
const OUT: u32 = 1;
const FLUSH: u32 = 4;
const GET_ID: u32 = 8;
const OK: u8 = 0;
const IOERR: u8 = 1;
const UNSUPP: u8 = 2;

#[repr(C, align(4096))]
#[derive(Clone)]
struct Block([u8; BLOCK]);
const _: () = assert!(size_of::<Block>() == BLOCK && align_of::<Block>() == BLOCK);

struct Buffer(Vec<Block>);
impl Buffer {
    fn new(len: usize) -> Self {
        Self(vec![Block([0; BLOCK]); len.div_ceil(BLOCK)])
    }
    fn bytes(&self, len: usize) -> &[u8] {
        // SAFETY: Block has no padding, is initialized, and Vec owns contiguous
        // blocks. The returned slice borrows the allocation and stays in bounds.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), len) }
    }
    fn bytes_mut(&mut self, len: usize) -> &mut [u8] {
        // SAFETY: same layout as bytes(); &mut self exclusively borrows it.
        unsafe { std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), len) }
    }
}

#[derive(Clone, Copy)]
struct Segment {
    addr: GuestAddress,
    len: usize,
    writable: bool,
}
struct Request {
    head: u16,
    status: GuestAddress,
    kind: u32,
    offset: u64,
    segments: Vec<Segment>,
    len: usize,
}
struct Pending {
    request: Request,
    buffer: Buffer,
}

#[derive(Default)]
struct Counters {
    reads: u64,
    writes: u64,
    flushes: u64,
    read_bytes: u64,
    write_bytes: u64,
    errors: u64,
    bounced: u64,
    peak: usize,
}

pub struct Backend {
    ring: IoUring,
    file: File,
    completion: EventFd,
    exit: (EventConsumer, EventNotifier),
    memory: Option<GuestMemoryAtomic<GuestMemoryMmap>>,
    capacity: u64,
    pending: BTreeMap<u64, Pending>,
    next_id: u64,
    counters: Counters,
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn other(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(error)
}

fn parse(
    mem: &GuestMemoryMmap,
    head: u16,
    descriptors: &[Segment],
    capacity: u64,
) -> io::Result<Request> {
    if descriptors.len() < 2 {
        return Err(invalid("missing request header/status"));
    }
    let first = descriptors[0];
    let last = *descriptors.last().unwrap();
    if first.writable || first.len != 16 || !last.writable || last.len != 1 {
        return Err(invalid("invalid header/status descriptor"));
    }
    for segment in descriptors {
        if segment.len == 0
            || !mem.check_range(
                segment.addr,
                segment.len,
                if segment.writable {
                    vm_memory::Permissions::Write
                } else {
                    vm_memory::Permissions::Read
                },
            )
        {
            return Err(invalid("descriptor outside guest memory"));
        }
    }
    let mut header = [0; 16];
    mem.read_slice(&mut header, first.addr).map_err(other)?;
    let kind = u32::from_le_bytes(header[..4].try_into().unwrap());
    let sector = u64::from_le_bytes(header[8..].try_into().unwrap());
    let offset = sector
        .checked_mul(512)
        .ok_or_else(|| invalid("sector overflow"))?;
    let segments = descriptors[1..descriptors.len() - 1].to_vec();
    let len = segments
        .iter()
        .try_fold(0usize, |n, s| n.checked_add(s.len))
        .ok_or_else(|| invalid("length overflow"))?;
    if len > MAX_IO {
        return Err(invalid("request exceeds size limit"));
    }
    if kind == IN || kind == OUT {
        if len == 0
            || !len.is_multiple_of(BLOCK)
            || !offset.is_multiple_of(BLOCK as u64)
            || offset
                .checked_add(len as u64)
                .is_none_or(|end| end > capacity)
            || segments.iter().any(|s| s.writable != (kind == IN))
        {
            return Err(invalid("invalid data range, alignment, or direction"));
        }
    } else if kind == FLUSH && len != 0 {
        return Err(invalid("FLUSH has data descriptors"));
    } else if kind == GET_ID && (len != 20 || segments.iter().any(|s| !s.writable)) {
        return Err(invalid("invalid GET_ID buffer"));
    }
    Ok(Request {
        head,
        status: last.addr,
        kind,
        offset,
        segments,
        len,
    })
}

impl Backend {
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_DIRECT | libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(path)?;
        let meta = file.metadata()?;
        if !meta.is_file() || meta.len() == 0 || !meta.len().is_multiple_of(BLOCK as u64) {
            return Err(invalid(
                "image must be a nonempty, 4 KiB aligned regular file",
            ));
        }
        file.try_lock().map_err(io::Error::from)?;
        let ring = IoUring::new(QUEUE_SIZE as u32)?;
        let completion = EventFd::new(EFD_NONBLOCK)?;
        ring.submitter().register_eventfd(completion.as_raw_fd())?;
        let exit = vmm_sys_util::event::new_event_consumer_and_notifier(EventFlag::NONBLOCK)?;
        Ok(Self {
            ring,
            file,
            completion,
            exit,
            memory: None,
            capacity: meta.len(),
            pending: BTreeMap::new(),
            next_id: 0,
            counters: Counters::default(),
        })
    }

    pub fn completion_fd(&self) -> RawFd {
        self.completion.as_raw_fd()
    }
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
    pub fn report(&self, pending_at_disconnect: usize, connection_ok: bool) -> serde_json::Value {
        let c = &self.counters;
        serde_json::json!({"schema_version":1, "backend":"raw_io_uring", "connection_ok":connection_ok,
            "pending_at_disconnect":pending_at_disconnect, "reads":c.reads, "writes":c.writes,
            "flushes":c.flushes,"read_bytes":c.read_bytes,"write_bytes":c.write_bytes,
            "errors":c.errors,"bounce_requests":c.bounced,"peak_inflight":c.peak,"queues":1})
    }

    fn finish(
        &mut self,
        mem: &GuestMemoryMmap,
        vring: &VringMutex,
        request: &Request,
        status: u8,
        written: u32,
    ) -> io::Result<()> {
        if status != OK {
            self.counters.errors += 1;
        }
        mem.write_obj(status, request.status).map_err(other)?;
        vring.add_used(request.head, written + 1).map_err(other)?;
        vring.signal_used_queue()
    }

    fn copy_to_guest(mem: &GuestMemoryMmap, request: &Request, data: &[u8]) -> io::Result<()> {
        let mut offset = 0;
        for s in &request.segments {
            mem.write_slice(&data[offset..offset + s.len], s.addr)
                .map_err(other)?;
            offset += s.len;
        }
        Ok(())
    }

    fn enqueue(
        &mut self,
        mem: &GuestMemoryMmap,
        vring: &VringMutex,
        request: Request,
    ) -> io::Result<()> {
        if request.kind == GET_ID {
            let mut id = [0; 20];
            id[..14].copy_from_slice(b"cas-experiment");
            Self::copy_to_guest(mem, &request, &id)?;
            return self.finish(mem, vring, &request, OK, 20);
        }
        if !matches!(request.kind, IN | OUT | FLUSH) {
            return self.finish(mem, vring, &request, UNSUPP, 0);
        }
        let mut buffer = Buffer::new(request.len);
        let fd = types::Fd(self.file.as_raw_fd());
        let entry = match request.kind {
            IN => opcode::Read::new(
                fd,
                buffer.bytes_mut(request.len).as_mut_ptr(),
                request.len as u32,
            )
            .offset(request.offset)
            .build(),
            OUT => {
                let mut offset = 0;
                for s in &request.segments {
                    mem.read_slice(
                        &mut buffer.bytes_mut(request.len)[offset..offset + s.len],
                        s.addr,
                    )
                    .map_err(other)?;
                    offset += s.len;
                }
                opcode::Write::new(fd, buffer.bytes(request.len).as_ptr(), request.len as u32)
                    .offset(request.offset)
                    .build()
            }
            FLUSH => opcode::Fsync::new(fd)
                .flags(types::FsyncFlags::DATASYNC)
                .build()
                .flags(squeue::Flags::IO_DRAIN),
            _ => unreachable!(),
        }
        .user_data(self.next_id);
        // SAFETY: the file and owned aligned buffer stay alive in self.pending
        // until the CQE. Errors/shutdown drain the ring before freeing buffers.
        unsafe { self.ring.submission().push(&entry) }.map_err(other)?;
        if request.kind != FLUSH {
            self.counters.bounced += 1;
        }
        self.pending
            .insert(self.next_id, Pending { request, buffer });
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or_else(|| invalid("request ID overflow"))?;
        self.counters.peak = self.counters.peak.max(self.pending.len());
        Ok(())
    }

    fn complete(&mut self, mem: &GuestMemoryMmap, vring: &VringMutex) -> io::Result<()> {
        loop {
            let completion = self
                .ring
                .completion()
                .next()
                .map(|c| (c.user_data(), c.result()));
            let Some((id, result)) = completion else {
                break;
            };
            let pending = self
                .pending
                .remove(&id)
                .ok_or_else(|| invalid("unknown IO completion"))?;
            let r = &pending.request;
            if result != r.len as i32 {
                self.finish(mem, vring, r, IOERR, 0)?;
                continue;
            }
            let written = match r.kind {
                IN => {
                    Self::copy_to_guest(mem, r, pending.buffer.bytes(r.len))?;
                    self.counters.reads += 1;
                    self.counters.read_bytes += r.len as u64;
                    r.len as u32
                }
                OUT => {
                    self.counters.writes += 1;
                    self.counters.write_bytes += r.len as u64;
                    0
                }
                FLUSH => {
                    self.counters.flushes += 1;
                    0
                }
                _ => unreachable!(),
            };
            self.finish(mem, vring, r, OK, written)?;
        }
        Ok(())
    }

    fn process(&mut self, vring: &VringMutex) -> io::Result<()> {
        let mem = self
            .memory
            .as_ref()
            .ok_or_else(|| invalid("guest memory missing"))?
            .memory();
        self.complete(&mem, vring)?;
        while self.pending.len() < QUEUE_SIZE {
            let chain: Option<DescriptorChain<_>> = vring
                .get_mut()
                .get_queue_mut()
                .pop_descriptor_chain(mem.clone());
            let Some(mut chain) = chain else {
                break;
            };
            let head = chain.head_index();
            let raw_descriptors: Vec<_> = chain.by_ref().collect();
            if raw_descriptors.last().is_some_and(|d| d.has_next()) {
                self.counters.errors += 1;
                return Err(invalid("unterminated descriptor chain"));
            }
            let descriptors: Vec<_> = raw_descriptors
                .iter()
                .map(|d| Segment {
                    addr: d.addr(),
                    len: d.len() as usize,
                    writable: d.is_write_only(),
                })
                .collect();
            match parse(&mem, head, &descriptors, self.capacity) {
                Ok(request) => self.enqueue(&mem, vring, request)?,
                Err(_) => {
                    // Report invalid IO when the status byte is safe to write.
                    // Otherwise stop this device and let the host watchdog fail.
                    if let Some(status) = descriptors.last().filter(|s| {
                        s.writable
                            && s.len == 1
                            && mem.check_range(s.addr, 1, vm_memory::Permissions::Write)
                    }) {
                        let request = Request {
                            head,
                            status: status.addr,
                            kind: 0,
                            offset: 0,
                            segments: Vec::new(),
                            len: 0,
                        };
                        self.finish(&mem, vring, &request, IOERR, 0)?;
                    } else {
                        self.counters.errors += 1;
                        return Err(invalid("malformed block request without status"));
                    }
                }
            }
        }
        self.ring.submit()?;
        Ok(())
    }

    /// After disconnect, reap IO without touching guest queues or memory.
    pub fn drain(&mut self) -> io::Result<()> {
        while !self.pending.is_empty() {
            match self.ring.submit_and_wait(1) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => {
                    result?;
                }
            }
            for completion in self.ring.completion() {
                self.pending.remove(&completion.user_data());
            }
        }
        Ok(())
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if self.drain().is_err() {
            // An unrecoverable ring error must not free buffers the kernel may
            // still reference. Leak them until process exit in this failure case.
            std::mem::forget(std::mem::take(&mut self.pending));
        }
    }
}

use vmm_sys_util::event::EventFlag;
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
        // SIZE_MAX, SEG_MAX, BLK_SIZE, FLUSH, INDIRECT_DESC,
        // VHOST_USER_F_PROTOCOL_FEATURES, VERSION_1. No MQ, EVENT_IDX or replay.
        (1 << 1) | (1 << 2) | (1 << 6) | (1 << 9) | (1 << 28) | (1 << 30) | (1 << 32)
    }
    fn protocol_features(&self) -> VhostUserProtocolFeatures {
        VhostUserProtocolFeatures::CONFIG
    }
    fn set_event_idx(&mut self, _: bool) {}
    fn get_config(&self, offset: u32, size: u32) -> Vec<u8> {
        let mut config = [0; 60];
        config[..8].copy_from_slice(&(self.capacity / 512).to_le_bytes());
        config[8..12].copy_from_slice(&(MAX_IO as u32).to_le_bytes());
        config[12..16].copy_from_slice(&((QUEUE_SIZE - 2) as u32).to_le_bytes());
        config[20..24].copy_from_slice(&(BLOCK as u32).to_le_bytes());
        let start = offset as usize;
        let Some(end) = start.checked_add(size as usize) else {
            return Vec::new();
        };
        config.get(start..end).unwrap_or(&[]).to_vec()
    }
    fn update_memory(&mut self, memory: GuestMemoryAtomic<GuestMemoryMmap>) -> io::Result<()> {
        if !self.pending.is_empty() {
            return Err(invalid("memory replacement with IO in flight"));
        }
        self.memory = Some(memory);
        Ok(())
    }
    fn exit_event(&self, _: usize) -> Option<(EventConsumer, EventNotifier)> {
        Some((
            self.exit.0.try_clone().unwrap(),
            self.exit.1.try_clone().unwrap(),
        ))
    }
    fn handle_event(
        &mut self,
        event: u16,
        events: EventSet,
        vrings: &[VringMutex],
        _: usize,
    ) -> io::Result<()> {
        if events != EventSet::IN {
            return Err(invalid("unexpected epoll event"));
        }
        match event {
            0 => (), // The framework already consumed the kick.
            2 => match self.completion.read() {
                Ok(_) => (),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => (),
                Err(e) => return Err(e),
            },
            _ => return Err(invalid("unknown event token")),
        }
        self.process(&vrings[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(kind: u32, sector: u64) -> (GuestMemoryMmap, Vec<Segment>) {
        let mem = GuestMemoryMmap::from_ranges(&[(GuestAddress(0), 0x10000)]).unwrap();
        let mut header = [0; 16];
        header[..4].copy_from_slice(&kind.to_le_bytes());
        header[8..].copy_from_slice(&sector.to_le_bytes());
        mem.write_slice(&header, GuestAddress(0)).unwrap();
        let mut descriptors = vec![Segment {
            addr: GuestAddress(0),
            len: 16,
            writable: false,
        }];
        if kind != FLUSH {
            descriptors.push(Segment {
                addr: GuestAddress(0x1000),
                len: BLOCK,
                writable: kind == IN,
            });
        }
        descriptors.push(Segment {
            addr: GuestAddress(0xf000),
            len: 1,
            writable: true,
        });
        (mem, descriptors)
    }

    #[test]
    fn sector_units_remain_512_bytes() {
        let (mem, descriptors) = fixture(OUT, 8);
        let r = parse(&mem, 7, &descriptors, 8192).unwrap();
        assert_eq!(r.offset, 4096);
        assert_eq!(r.len, 4096);
        assert_eq!(r.head, 7);
    }

    #[test]
    fn rejects_misalignment_overflow_and_out_of_bounds() {
        for sector in [1, 16, u64::MAX] {
            let (mem, descriptors) = fixture(IN, sector);
            assert!(parse(&mem, 0, &descriptors, 8192).is_err());
        }
        let (mem, mut descriptors) = fixture(IN, 0);
        descriptors[1].len = BLOCK - 1;
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
        descriptors[1].len = BLOCK;
        descriptors[1].addr = GuestAddress(0xffff);
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
    }

    #[test]
    fn scatter_gather_checks_direction_and_total_length() {
        let (mem, mut descriptors) = fixture(IN, 0);
        descriptors[1].len = 2048;
        descriptors.insert(
            2,
            Segment {
                addr: GuestAddress(0x3000),
                len: 2048,
                writable: true,
            },
        );
        assert_eq!(parse(&mem, 0, &descriptors, 8192).unwrap().len, BLOCK);
        descriptors[2].writable = false;
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
    }

    #[test]
    fn requires_header_and_writable_status() {
        let (mem, mut descriptors) = fixture(OUT, 0);
        assert!(parse(&mem, 0, &descriptors[..1], 8192).is_err());
        descriptors[0].writable = true;
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
        descriptors[0].writable = false;
        descriptors[2].writable = false;
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
    }

    #[test]
    fn flush_has_no_data_payload() {
        let (mem, mut descriptors) = fixture(FLUSH, 0);
        assert_eq!(parse(&mem, 0, &descriptors, 8192).unwrap().len, 0);
        descriptors.insert(
            1,
            Segment {
                addr: GuestAddress(0x1000),
                len: BLOCK,
                writable: false,
            },
        );
        assert!(parse(&mem, 0, &descriptors, 8192).is_err());
    }
}
