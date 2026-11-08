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
struct Slot {
    event: EventFd,
    owned: bool,
    done: bool,
    hash: [u8; 32],
    bytes: [u8; BLOCK_SIZE],
    phases: Phases,
    error: Option<io::ErrorKind>,
    queued: Instant,
}
struct Pool {
    slots: Vec<Slot, BudgetAllocator>,
    free: Vec<usize, BudgetAllocator>,
    queued: Queue<usize>,
}
struct Shared {
    pool: Mutex<Pool>,
    wake: EventFd,
    stop: AtomicBool,
    metrics: BudgetArc<Metrics>,
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
            });
            free.push(index);
        }
        let shared = BudgetArc::try_new(
            Shared {
                pool: Mutex::new(Pool {
                    slots,
                    free,
                    queued: Queue::with_capacity(SLOTS, budget)?,
                }),
                wake: EventFd::new(libc::EFD_NONBLOCK | libc::EFD_CLOEXEC)?,
                stop: AtomicBool::new(false),
                metrics,
            },
            budget,
        )?;
        let pending = Queue::with_capacity(SLOTS, budget)?;
        let owner = shared.clone();
        let worker = thread::Builder::new()
            .name("cas-peer-get".into())
            .spawn(move || run(owner, address, pending))?;
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
        pool.queued.push_back(index);
        self.shared.metrics.count(|c| c.gets += 1);
        // The live shared event cannot be closed while the worker owns it.
        self.shared.wake.write(1)?;
        Ok(Ticket {
            shared: self.shared.clone(),
            index,
        })
    }
}
impl Ticket {
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

fn finish(shared: &Shared, index: usize, bytes: &[u8], error: Option<io::ErrorKind>, first: u64) {
    let mut pool = shared.pool.lock().expect("peer pool poisoned");
    let slot = &mut pool.slots[index];
    slot.phases.first_byte_ns = first;
    let error = error.or_else(|| verify(&slot.hash, bytes).err().map(|e| e.kind()));
    if error.is_none() {
        slot.bytes.copy_from_slice(bytes);
        slot.phases.verified_ns = shared.metrics.now();
    }
    slot.error = error;
    slot.done = true;
    shared.metrics.count(|c| {
        c.verified += u64::from(error.is_none());
        c.errors += u64::from(error.is_some());
    });
    if slot.owned {
        let _ = slot.event.write(1);
    } else {
        pool.free.push(index);
    }
}

fn run(shared: BudgetArc<Shared>, address: SocketAddr, mut pending: Queue<usize>) {
    let mut stream: Option<TcpStream> = None;
    let mut sending: Option<usize> = None;
    let mut request = [0; HEADER + 32];
    let mut sent = 0;
    let mut response = [0; FRAME];
    let mut received = 0;
    let mut expected = HEADER;
    let mut first = 0;
    while !shared.stop.load(Ordering::Acquire) {
        let outcome = (|| -> io::Result<()> {
            if sending.is_none() {
                sending = shared
                    .pool
                    .lock()
                    .expect("peer pool poisoned")
                    .queued
                    .pop_front();
                if let Some(index) = sending {
                    sent = 0;
                    let pool = shared.pool.lock().expect("peer pool poisoned");
                    Header {
                        op: GET,
                        status: OK,
                        id: index as u64 + 1,
                        count: 1,
                        bytes: request.len(),
                    }
                    .encode(&mut request);
                    request[HEADER..].copy_from_slice(&pool.slots[index].hash);
                }
            }
            if stream.is_none() && sending.is_some() {
                let socket = TcpStream::connect_timeout(&address, TIMEOUT)?;
                socket.set_nodelay(true)?;
                socket.set_nonblocking(true)?;
                stream = Some(socket);
                shared.metrics.count(|c| c.connections += 1);
            }
            // Drain ready requests before blocking for a reply: true pipelining.
            while let (Some(socket), Some(index)) = (&mut stream, sending) {
                if sent == 0 {
                    shared.pool.lock().expect("peer pool poisoned").slots[index]
                        .phases
                        .send_ns = shared.metrics.now();
                }
                match socket.write(&request[sent..]) {
                    Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                    Ok(n) => sent += n,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
                if sent != request.len() {
                    continue;
                }
                pending.push_back(index);
                shared
                    .metrics
                    .count(|c| c.max_inflight = c.max_inflight.max(pending.len()));
                sending = shared
                    .pool
                    .lock()
                    .expect("peer pool poisoned")
                    .queued
                    .pop_front();
                if let Some(index) = sending {
                    sent = 0;
                    let pool = shared.pool.lock().expect("peer pool poisoned");
                    Header {
                        op: GET,
                        status: OK,
                        id: index as u64 + 1,
                        count: 1,
                        bytes: request.len(),
                    }
                    .encode(&mut request);
                    request[HEADER..].copy_from_slice(&pool.slots[index].hash);
                }
            }
            while let (Some(socket), Some(&index)) = (&mut stream, pending.front()) {
                match socket.read(&mut response[received..expected]) {
                    Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                    Ok(n) => {
                        if received == 0 {
                            first = shared.metrics.now();
                        }
                        received += n;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e),
                }
                if received < expected {
                    continue;
                }
                let h = Header::decode(&response, true)?;
                if h.op != GET || h.id != index as u64 + 1 {
                    return Err(invalid());
                }
                if received == HEADER && h.bytes > HEADER {
                    expected = h.bytes;
                    continue;
                }
                let error = match h.status {
                    OK => None,
                    MISSING => Some(io::ErrorKind::NotFound),
                    _ => Some(io::ErrorKind::Other),
                };
                finish(&shared, index, &response[HEADER..received], error, first);
                pending.pop_front();
                received = 0;
                expected = HEADER;
                first = 0;
            }
            let oldest = pending.front().copied().or(sending);
            if let Some(index) = oldest
                && shared.pool.lock().expect("peer pool poisoned").slots[index]
                    .queued
                    .elapsed()
                    >= TIMEOUT
            {
                return Err(io::ErrorKind::TimedOut.into());
            }
            Ok(())
        })();
        if let Err(error) = outcome {
            stream = None;
            // Capture this failed batch before notifying any owner. A new
            // request submitted after its error notification can reconnect.
            if let Some(index) = sending.take() {
                pending.push_back(index);
            }
            {
                let mut pool = shared.pool.lock().expect("peer pool poisoned");
                while let Some(index) = pool.queued.pop_front() {
                    pending.push_back(index);
                }
            }
            while let Some(index) = pending.pop_front() {
                finish(&shared, index, &[], Some(error.kind()), first);
                first = 0;
            }
            received = 0;
            expected = HEADER;
            first = 0;
        }
        let mut fds = [
            libc::pollfd {
                fd: shared.wake.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: if pending.is_empty() && sending.is_none() {
                    -1
                } else {
                    stream.as_ref().map_or(-1, AsRawFd::as_raw_fd)
                },
                events: (if pending.is_empty() { 0 } else { libc::POLLIN })
                    | (if sending.is_some() { libc::POLLOUT } else { 0 }),
                revents: 0,
            },
        ];
        // SAFETY: fds contains two live or ignored descriptors for this call.
        unsafe {
            libc::poll(
                fds.as_mut_ptr(),
                fds.len() as libc::nfds_t,
                if pending.is_empty() && sending.is_none() {
                    -1
                } else {
                    50
                },
            );
        }
        let _ = shared.wake.read();
    }
    if let Some(index) = sending {
        finish(&shared, index, &[], Some(io::ErrorKind::Interrupted), 0);
    }
    while let Some(index) = pending.pop_front() {
        finish(&shared, index, &[], Some(io::ErrorKind::Interrupted), 0);
    }
    loop {
        let index = shared
            .pool
            .lock()
            .expect("peer pool poisoned")
            .queued
            .pop_front();
        let Some(index) = index else {
            break;
        };
        finish(&shared, index, &[], Some(io::ErrorKind::Interrupted), 0);
    }
}
