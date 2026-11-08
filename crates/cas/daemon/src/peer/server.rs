use super::{TIMEOUT, metrics::Metrics, wire::*};
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator, BudgetArc},
    cache::Cache,
    store::file::Reader,
};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) const CONNECTIONS: usize = 4;
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}
impl Server {
    pub fn start(
        address: SocketAddr,
        reader: Reader,
        cache: BudgetArc<Cache>,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
        scheduler: (BudgetArc<cas_core::scheduler::Scheduler>, usize),
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let mut server = Self {
            address: listener.local_addr()?,
            stop: Arc::new(AtomicBool::new(false)),
            workers: Vec::with_capacity(CONNECTIONS),
        };
        for index in 0..CONNECTIONS {
            let scheduling =
                cas_core::scheduler::Scheduler::port(&scheduler.0, scheduler.1 + index)?;
            let listener = listener.try_clone()?;
            let stop = Arc::clone(&server.stop);
            let reader = reader.clone();
            let cache = cache.clone();
            let metrics = metrics.clone();
            // Allocate aligned O_DIRECT scratch once per connection worker, not per GET.
            let mut page =
                AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(budget)))?;
            let mut payload =
                AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(Arc::clone(budget)))?;
            server
                .workers
                .push(
                    thread::Builder::new()
                        .name("cas-peer-serve".into())
                        .spawn(move || {
                            while !stop.load(Ordering::Acquire) {
                                match listener.accept() {
                                    Ok((mut socket, _)) => {
                                        let result = (|| {
                                            socket.set_nodelay(true)?;
                                            socket.set_read_timeout(Some(
                                                Duration::from_millis(50),
                                            ))?;
                                            socket.set_write_timeout(Some(
                                                Duration::from_millis(50),
                                            ))?;
                                            serve(
                                                &mut socket,
                                                &stop,
                                                &reader,
                                                &cache,
                                                &metrics,
                                                (page.as_mut_slice(), payload.as_mut_slice()),
                                                &scheduling,
                                            )
                                        })();
                                        if result.is_err() {
                                            metrics.count(|c| c.server_errors += 1);
                                        }
                                    }
                                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                                        thread::sleep(Duration::from_millis(5))
                                    }
                                    Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                                    Err(_) => break,
                                }
                            }
                        })?,
                );
        }
        Ok(server)
    }
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if self.workers.iter().any(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        for worker in self.workers.drain(..) {
            worker
                .join()
                .map_err(|_| io::Error::other("peer service worker panicked"))?;
        }
        Ok(())
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}
fn check(stop: &AtomicBool, deadline: Option<Instant>) -> io::Result<()> {
    if stop.load(Ordering::Acquire) {
        return Err(io::ErrorKind::Interrupted.into());
    }
    if deadline.is_some_and(|d| Instant::now() >= d) {
        return Err(io::ErrorKind::TimedOut.into());
    }
    Ok(())
}
fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
    )
}
fn read(
    socket: &mut TcpStream,
    mut bytes: &mut [u8],
    stop: &AtomicBool,
    deadline: Option<Instant>,
) -> io::Result<()> {
    while !bytes.is_empty() {
        check(stop, deadline)?;
        match socket.read(bytes) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if transient(&e) => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write(
    socket: &mut TcpStream,
    mut bytes: &[u8],
    stop: &AtomicBool,
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        check(stop, Some(deadline))?;
        match socket.write(bytes) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(n) => bytes = &bytes[n..],
            Err(e) if transient(&e) => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn serve(
    socket: &mut TcpStream,
    stop: &AtomicBool,
    reader: &Reader,
    cache: &Cache,
    metrics: &Metrics,
    scratch: (&mut [u8], &mut [u8]),
    scheduling: &cas_core::scheduler::Port,
) -> io::Result<()> {
    let (page, payload) = scratch;
    let mut request = [0; FRAME];
    let mut response = [0; FRAME];
    let mut connection_op = None;
    loop {
        // Idle connections have no request deadline. Once a frame starts, slow
        // partial reads/writes cannot extend its absolute deadline.
        // Every legal request is at least HEADER + one hash. Read that
        // prefix together: the common GET needs one receive, without consuming
        // any bytes from a following pipelined request.
        let mut received = loop {
            check(stop, None)?;
            match socket.read(&mut request[..HEADER + 32]) {
                Ok(0) => return Ok(()),
                Ok(n) => break n,
                Err(e) if transient(&e) => (),
                Err(e) => return Err(e),
            }
        };
        let deadline = Instant::now() + TIMEOUT;
        if received < HEADER {
            read(socket, &mut request[received..HEADER], stop, Some(deadline))?;
            received = HEADER;
        }
        let h = Header::decode(&request, false)?;
        if connection_op.is_some_and(|op| op != h.op) {
            return Err(invalid());
        }
        connection_op = Some(h.op);
        read(
            socket,
            &mut request[received..h.bytes],
            stop,
            Some(deadline),
        )?;
        let mut reply = Header { bytes: HEADER, ..h };
        if h.op == GET {
            let hash = request[HEADER..h.bytes].try_into().unwrap();
            let result = (|| -> io::Result<bool> {
                if let Some(bytes) = cache.get(&hash) {
                    response[HEADER..].copy_from_slice(bytes.as_slice());
                    metrics.count(|c| c.server_memory += 1);
                    return Ok(true);
                }
                let Some(plan) = reader.plan(hash)? else {
                    return Ok(false);
                };
                scheduling.ready(true)?;
                let grant: io::Result<()> = (|| {
                    loop {
                        check(stop, Some(deadline))?;
                        if scheduling.take(2 * BLOCK_SIZE, false)? {
                            return Ok(());
                        }
                        thread::sleep(Duration::from_micros(100));
                    }
                })();
                scheduling.ready(false)?;
                grant?;
                plan.load_with_scratch(page, payload)?;
                verify(&hash, payload)?;
                // Read-fill only, bounded by the existing host cache budget.
                match cache.fill(hash, payload) {
                    Ok(_) => (),
                    Err(e) if e.kind() == io::ErrorKind::OutOfMemory => (),
                    Err(e) => return Err(e),
                }
                response[HEADER..].copy_from_slice(payload);
                metrics.count(|c| c.server_store += 1);
                Ok(true)
            })();
            match result {
                Ok(true) => reply.bytes = FRAME,
                Ok(false) => {
                    reply.status = MISSING;
                    metrics.count(|c| c.server_missing += 1);
                }
                Err(_) => {
                    reply.status = FAILED;
                    metrics.count(|c| c.server_errors += 1);
                }
            }
        } else {
            metrics.count(|c| c.has += 1);
            reply.bytes += h.count.div_ceil(8);
            response[HEADER..reply.bytes].fill(0);
            for (i, bytes) in request[HEADER..h.bytes]
                .as_chunks::<32>()
                .0
                .iter()
                .enumerate()
            {
                // HAS advertises only locally fenced storage, never ephemeral cache.
                match reader.plan(*bytes) {
                    Ok(Some(_)) => (),
                    Ok(None) => response[HEADER + i / 8] |= 1 << (i % 8),
                    Err(_) => {
                        reply.status = FAILED;
                        reply.bytes = HEADER;
                        break;
                    }
                }
            }
        }
        reply.encode(&mut response);
        write(socket, &response[..reply.bytes], stop, deadline)?;
    }
}

/// Provisioning probe; use a separate connection from GET. One means missing.
pub fn has(address: SocketAddr, hashes: &[[u8; 32]], missing: &mut [u8]) -> io::Result<()> {
    if hashes.is_empty() || hashes.len() > MAX_HASHES || missing.len() != hashes.len().div_ceil(8) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut socket = TcpStream::connect_timeout(&address, TIMEOUT)?;
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(TIMEOUT))?;
    socket.set_write_timeout(Some(TIMEOUT))?;
    let mut bytes = [0; FRAME];
    let h = Header {
        op: HAS,
        status: OK,
        id: 1,
        count: hashes.len(),
        bytes: HEADER + hashes.len() * 32,
    };
    h.encode(&mut bytes);
    for (out, hash) in bytes[HEADER..h.bytes]
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(hashes)
    {
        out.copy_from_slice(hash);
    }
    socket.write_all(&bytes[..h.bytes])?;
    socket.read_exact(&mut bytes[..HEADER])?;
    let reply = Header::decode(&bytes, true)?;
    if reply.op != HAS || reply.id != h.id || reply.count != h.count || reply.status != OK {
        return Err(invalid());
    }
    socket.read_exact(missing)
}
