use super::{
    TIMEOUT,
    metrics::{Metrics, Phases},
    wire::*,
};
use allocator_api2::vec::Vec;
use cas_core::{
    BLOCK_SIZE,
    budget::{Budget, BudgetAllocator, BudgetArc, Queue},
};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    os::fd::{AsRawFd, RawFd},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Instant,
};
use vmm_sys_util::eventfd::EventFd;

// At most one remote chunk per admitted host read; slots outlive cancelled polls.
const SLOTS: usize = crate::local::pools::HOST_REQUESTS;
/// Slot completion eventfds plus the client's wake eventfd and TCP socket.
pub(crate) const CLIENT_DESCRIPTORS: u64 = SLOTS as u64 + 2;
/// Headroom for the rest of the host: stores, WALs, rings, listeners, guests.
const HOST_DESCRIPTORS: u64 = 1024;

/// Each GET client preallocates one eventfd per host request slot, so even a
/// single client exceeds the common 1024-descriptor soft limit. Raise the soft
/// limit to the hard limit before allocating (the daemon uses epoll/io_uring,
/// never select), and fail clearly when the hard limit cannot cover `clients`.
pub(crate) fn reserve_descriptors(clients: usize) -> io::Result<()> {
    let required = (clients as u64)
        .saturating_mul(CLIENT_DESCRIPTORS)
        .saturating_add(HOST_DESCRIPTORS);
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one initialized rlimit through a valid pointer.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    if limit.rlim_cur == libc::RLIM_INFINITY || limit.rlim_cur >= required {
        return Ok(());
    }
    if limit.rlim_max != libc::RLIM_INFINITY && limit.rlim_max < required {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{clients} peer GET client(s) need {required} descriptors; \
                 hard RLIMIT_NOFILE is {}",
                limit.rlim_max
            ),
        ));
    }
    limit.rlim_cur = if limit.rlim_max == libc::RLIM_INFINITY {
        required
    } else {
        limit.rlim_max
    };
    // SAFETY: setrlimit reads one initialized rlimit; raising only the soft
    // limit up to the hard limit needs no privilege.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
struct Slot {
    event: EventFd,
    owned: bool,
    done: bool,
    hash: [u8; 32],
    bytes: [u8; BLOCK_SIZE],
    phases: Phases,
    error: Option<io::ErrorKind>,
    queued: Instant,
    /// Image slot credited with the verified payload; `UNATTRIBUTED` otherwise.
    image: usize,
}
struct Pool {
    slots: Vec<Slot, BudgetAllocator>,
    free: Vec<usize, BudgetAllocator>,
    queued: Queue<usize>,
    pending: Queue<usize>,
    stream: Option<Arc<TcpStream>>,
    sending: Option<usize>,
    request: [u8; HEADER + 32],
    sent: usize,
    response: [u8; FRAME],
    received: usize,
    expected: usize,
    first: u64,
    last_receive: u64,
}
struct Shared {
    pool: Mutex<Pool>,
    wake: EventFd,
    stop: AtomicBool,
    metrics: BudgetArc<Metrics>,
    inline: std::time::Duration,
}
pub struct Client {
    shared: BudgetArc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
pub struct Ticket {
    shared: BudgetArc<Shared>,
    index: usize,
}
impl Client {
    pub fn new(
        address: SocketAddr,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
    ) -> io::Result<Self> {
        Self::with_inline(address, budget, metrics, 0)
    }
    pub fn with_inline(
        address: SocketAddr,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
        inline_us: u32,
    ) -> io::Result<Self> {
        reserve_descriptors(1)?;
        let mut slots = crate::local::reserved_vec(SLOTS, budget)?;
        let mut free = crate::local::reserved_vec(SLOTS, budget)?;
        for index in 0..SLOTS {
            slots.push(Slot {
                event: EventFd::new(libc::EFD_NONBLOCK | libc::EFD_CLOEXEC)?,
                owned: false,
                done: false,
                hash: [0; 32],
                bytes: [0; BLOCK_SIZE],
                phases: Phases::default(),
                error: None,
                queued: Instant::now(),
                image: super::metrics::UNATTRIBUTED,
            });
            free.push(index);
        }
        let shared = BudgetArc::try_new(
            Shared {
                pool: Mutex::new(Pool {
                    slots,
                    free,
                    queued: Queue::with_capacity(SLOTS, budget)?,
                    pending: Queue::with_capacity(SLOTS, budget)?,
                    stream: None,
                    sending: None,
                    request: [0; HEADER + 32],
                    sent: 0,
                    response: [0; FRAME],
                    received: 0,
                    expected: HEADER,
                    first: 0,
                    last_receive: 0,
                }),
                wake: EventFd::new(libc::EFD_NONBLOCK | libc::EFD_CLOEXEC)?,
                stop: AtomicBool::new(false),
                metrics,
                inline: std::time::Duration::from_micros(u64::from(inline_us)),
            },
            budget,
        )?;
        let owner = shared.clone();
        let worker = thread::Builder::new()
            .name("cas-peer-get".into())
            .spawn(move || run(owner, address))?;
        Ok(Self {
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn shutdown(&self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake.write(1)?;
        let mut worker = self.worker.lock().expect("peer worker poisoned");
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("peer worker panicked"))?;
        }
        Ok(())
    }

    pub fn fetch(&self, hash: [u8; 32]) -> io::Result<Ticket> {
        self.fetch_for(hash, super::metrics::UNATTRIBUTED)
    }
    pub(crate) fn fetch_for(&self, hash: [u8; 32], image: usize) -> io::Result<Ticket> {
        if self.shared.stop.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let mut pool = self.shared.pool.lock().expect("peer pool poisoned");
        let index = pool.free.pop().ok_or(io::ErrorKind::WouldBlock)?;
        let slot = &mut pool.slots[index];
        let _ = slot.event.read();
        slot.owned = true;
        slot.done = false;
        slot.hash = hash;
        slot.phases = Phases::default();
        slot.error = None;
        slot.queued = Instant::now();
        slot.image = image;
        pool.queued.push_back(index);
        self.shared.metrics.count(|c| c.gets += 1);
        // The image reactor may send and consume a fast reply itself. Bound
        // polling so a slow peer cannot monopolize mutation/FLUSH processing.
        if pool.stream.is_some() {
            self.shared.metrics.count(|c| c.inline_attempts += 1);
            let until = Instant::now() + self.shared.inline;
            loop {
                pool.drive(&self.shared.metrics);
                if pool.slots[index].done || Instant::now() >= until {
                    break;
                }
                std::hint::spin_loop();
            }
            if pool.slots[index].done {
                self.shared.metrics.count(|c| c.inline_completed += 1);
            }
        }
        // A completed inline GET needs no worker handoff. Slow replies and
        // connection failures refresh the worker's socket/deadline snapshot.
        if pool.active() || pool.stream.is_none() {
            self.shared.wake.write(1)?;
        }
        Ok(Ticket {
            shared: self.shared.clone(),
            index,
        })
    }
}
impl Ticket {
    pub fn ready(&self) -> bool {
        self.shared.pool.lock().expect("peer pool poisoned").slots[self.index].done
    }
    pub fn notification(&self) -> RawFd {
        self.shared.pool.lock().expect("peer pool poisoned").slots[self.index]
            .event
            .as_raw_fd()
    }
    pub fn finish(&self, destination: &mut [u8]) -> (Phases, io::Result<()>) {
        let pool = self.shared.pool.lock().expect("peer pool poisoned");
        let slot = &pool.slots[self.index];
        let result = if !slot.done {
            Err(io::ErrorKind::WouldBlock.into())
        } else if let Some(error) = slot.error {
            Err(error.into())
        } else if destination.len() != BLOCK_SIZE {
            Err(io::ErrorKind::InvalidInput.into())
        } else {
            destination.copy_from_slice(&slot.bytes);
            Ok(())
        };
        (slot.phases, result)
    }
}
impl Drop for Ticket {
    fn drop(&mut self) {
        let mut pool = self.shared.pool.lock().expect("peer pool poisoned");
        let slot = &mut pool.slots[self.index];
        slot.owned = false;
        if slot.done {
            pool.free.push(self.index);
        }
        // Pending abandoned slots are recycled only after transport completion.
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let _ = self.shared.wake.write(1);
        if let Some(worker) = self.worker.get_mut().expect("peer worker poisoned").take() {
            let _ = worker.join();
        }
    }
}

impl Pool {
    fn finish(
        &mut self,
        metrics: &Metrics,
        index: usize,
        error: Option<io::ErrorKind>,
        first: u64,
    ) {
        let slot = &mut self.slots[index];
        slot.phases.first_byte_ns = first;
        let bytes = &self.response[HEADER..self.expected.max(HEADER)];
        let error = error.or_else(|| verify(&slot.hash, bytes).err().map(|e| e.kind()));
        if error.is_none() {
            slot.bytes.copy_from_slice(bytes);
            slot.phases.verified_ns = metrics.now();
        }
        slot.error = error;
        slot.done = true;
        metrics.transport_finished(slot.image, error.is_none());
        if slot.owned {
            let _ = slot.event.write(1);
        } else {
            self.free.push(index);
        }
    }
    fn next(&mut self) {
        self.sending = self.queued.pop_front();
        if let Some(index) = self.sending {
            self.sent = 0;
            Header {
                op: GET,
                status: OK,
                id: index as u64 + 1,
                count: 1,
                bytes: self.request.len(),
            }
            .encode(&mut self.request);
            self.request[HEADER..].copy_from_slice(&self.slots[index].hash);
        }
    }
    fn active(&self) -> bool {
        !self.queued.is_empty() || !self.pending.is_empty() || self.sending.is_some()
    }
    fn failed(&mut self, metrics: &Metrics, error: io::ErrorKind) {
        self.stream = None;
        if let Some(index) = self.sending.take() {
            self.pending.push_back(index);
        }
        while let Some(index) = self.queued.pop_front() {
            self.pending.push_back(index);
        }
        while let Some(index) = self.pending.pop_front() {
            self.finish(metrics, index, Some(error), self.first);
            self.first = 0;
        }
        self.received = 0;
        self.expected = HEADER;
    }
    fn drive(&mut self, metrics: &Metrics) {
        if let Err(error) = self.progress(metrics) {
            self.failed(metrics, error.kind());
        }
    }
    fn progress(&mut self, metrics: &Metrics) -> io::Result<()> {
        let Some(socket) = self.stream.clone() else {
            return Ok(());
        };
        let mut socket = &*socket;
        if self.sending.is_none() {
            self.next();
        }
        // Bound a visit as well as the inline time budget. Never wait for IO
        // while holding this lock; connection establishment stays on the worker.
        for _ in 0..64 {
            let Some(index) = self.sending else {
                break;
            };
            if self.sent == 0 && self.slots[index].phases.send_ns == 0 {
                self.slots[index].phases.send_ns = metrics.now();
            }
            match socket.write(&self.request[self.sent..]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => {
                    self.sent += n;
                    metrics.wire(true, n);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
            if self.sent == self.request.len() {
                self.pending.push_back(index);
                metrics.count(|c| c.max_inflight = c.max_inflight.max(self.pending.len()));
                self.next();
            }
        }
        for _ in 0..64 {
            let Some(&index) = self.pending.front() else {
                break;
            };
            if self.received < self.expected {
                match socket.read(&mut self.response[self.received..]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => {
                        metrics.wire(false, n);
                        self.last_receive = metrics.now();
                        if self.received == 0 {
                            self.first = self.last_receive;
                        }
                        self.received += n;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
            }
            if self.received < HEADER {
                continue;
            }
            let h = Header::decode(&self.response, true)?;
            if h.op != GET || h.id != index as u64 + 1 {
                return Err(invalid());
            }
            self.expected = h.bytes;
            if self.received < self.expected {
                continue;
            }
            let error = match h.status {
                OK => None,
                MISSING => Some(io::ErrorKind::NotFound),
                _ => Some(io::ErrorKind::Other),
            };
            self.finish(metrics, index, error, self.first);
            self.pending.pop_front();
            self.response.copy_within(self.expected..self.received, 0);
            self.received -= self.expected;
            self.expected = HEADER;
            self.first = if self.received == 0 {
                0
            } else {
                self.last_receive
            };
        }
        if let Some(index) = self.pending.front().copied().or(self.sending)
            && self.slots[index].queued.elapsed() >= TIMEOUT
        {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(())
    }
}

fn run(shared: BudgetArc<Shared>, address: SocketAddr) {
    while !shared.stop.load(Ordering::Acquire) {
        let connect = {
            let pool = shared.pool.lock().expect("peer pool poisoned");
            pool.active() && pool.stream.is_none()
        };
        if connect {
            let connected = (|| {
                let socket = TcpStream::connect_timeout(&address, TIMEOUT)?;
                socket.set_nodelay(true)?;
                socket.set_nonblocking(true)?;
                Ok::<_, io::Error>(socket)
            })();
            let mut pool = shared.pool.lock().expect("peer pool poisoned");
            match connected {
                Ok(socket) => {
                    pool.stream = Some(Arc::new(socket));
                    shared.metrics.count(|c| c.connections += 1);
                }
                Err(e) => pool.failed(&shared.metrics, e.kind()),
            }
        }
        let (socket, active, writing) = {
            let mut pool = shared.pool.lock().expect("peer pool poisoned");
            pool.drive(&shared.metrics);
            (pool.stream.clone(), pool.active(), pool.sending.is_some())
        };
        let mut fds = [
            libc::pollfd {
                fd: shared.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                // Observe EOF on idle connections as well. Otherwise a peer
                // restart is discovered only by failing the next guest read.
                fd: socket.as_ref().map_or(-1, |s| s.as_raw_fd()),
                events: libc::POLLIN | if writing { libc::POLLOUT } else { 0 },
                revents: 0,
            },
        ];
        // SAFETY: the Arc snapshot pins the socket even if an inline request
        // discards the connection while this worker is asleep.
        unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                if active { 50 } else { -1 },
            );
        }
        let _ = shared.wake.read();
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            let mut pool = shared.pool.lock().expect("peer pool poisoned");
            if !pool.active()
                && pool
                    .stream
                    .as_ref()
                    .zip(socket.as_ref())
                    .is_some_and(|(a, b)| Arc::ptr_eq(a, b))
            {
                pool.stream = None;
            }
        }
    }
    shared
        .pool
        .lock()
        .expect("peer pool poisoned")
        .failed(&shared.metrics, io::ErrorKind::Interrupted);
}
