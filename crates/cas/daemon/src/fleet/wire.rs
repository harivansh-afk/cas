//! Bulk frames are capped before allocation. Each connection has one exchange
//! at a time; its flow tag makes actual application wire bytes attributable.
use cas_core::{
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const HEADER: usize = 24;
pub(crate) const MAX_BODY: usize = 1024 * 1024;
const TIMEOUT: Duration = Duration::from_secs(30);
#[derive(Clone, Copy, Debug, serde::Serialize)]
#[repr(u8)]
pub enum Flow {
    Ship = 0,
    Provision = 1,
    Migration = 2,
    Sync = 3,
    Collection = 4,
    Control = 5,
}
impl Flow {
    fn decode(value: u8) -> io::Result<Self> {
        match value {
            0 => Ok(Self::Ship),
            1 => Ok(Self::Provision),
            2 => Ok(Self::Migration),
            3 => Ok(Self::Sync),
            4 => Ok(Self::Collection),
            5 => Ok(Self::Control),
            _ => Err(invalid()),
        }
    }
}
#[derive(Default)]
pub struct Metrics {
    tx: [AtomicU64; 6],
    rx: [AtomicU64; 6],
    put_acked_chunks: [AtomicU64; 6],
}
impl Metrics {
    pub fn report(&self) -> serde_json::Value {
        let flows = [
            "ship",
            "provision",
            "migration",
            "sync",
            "collection",
            "control",
        ];
        let mut result = serde_json::Map::new();
        for (i, name) in flows.iter().enumerate() {
            result.insert((*name).into(), serde_json::json!({"sent":self.tx[i].load(Ordering::Relaxed), "received":self.rx[i].load(Ordering::Relaxed),
                "put_acked_chunks":self.put_acked_chunks[i].load(Ordering::Relaxed)}));
        }
        result.into()
    }
}
#[derive(Clone, Copy)]
struct Header {
    op: u8,
    flow: Flow,
    status: u8,
    id: u64,
    bytes: usize,
}
impl Header {
    fn encode(self) -> [u8; HEADER] {
        let mut bytes = [0; HEADER];
        bytes[..4].copy_from_slice(&((HEADER + self.bytes) as u32).to_le_bytes());
        bytes[4..8].copy_from_slice(b"CASB");
        bytes[8] = 1;
        bytes[9] = self.op;
        bytes[10] = self.flow as u8;
        bytes[11] = self.status;
        bytes[12..20].copy_from_slice(&self.id.to_le_bytes());
        bytes
    }
    fn decode(bytes: &[u8; HEADER], reply: bool) -> io::Result<Self> {
        let length = u32::from_le_bytes(bytes[..4].try_into().unwrap()) as usize;
        if &bytes[4..8] != b"CASB"
            || bytes[8] != 1
            || bytes[9] == 0
            || bytes[20..] != [0; 4]
            || !(HEADER..=HEADER + MAX_BODY).contains(&length)
            || bytes[11] > 2
            || (!reply && bytes[11] != 0)
        {
            return Err(invalid());
        }
        let id = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        if id == 0 {
            return Err(invalid());
        }
        Ok(Self {
            op: bytes[9],
            flow: Flow::decode(bytes[10])?,
            status: bytes[11],
            id,
            bytes: length - HEADER,
        })
    }
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn transfer(
    socket: &mut TcpStream,
    bytes: &mut [u8],
    sending: bool,
    count: &AtomicU64,
    stop: Option<&AtomicBool>,
) -> io::Result<()> {
    let deadline = Instant::now() + TIMEOUT;
    let mut position = 0;
    while position < bytes.len() {
        if stop.is_some_and(|s| s.load(Ordering::Acquire)) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let result = if sending {
            socket.write(&bytes[position..])
        } else {
            socket.read(&mut bytes[position..])
        };
        match result {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => {
                position += n;
                count.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Interrupted
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
pub struct Client {
    dead: bool,
    socket: TcpStream,
    next: u64,
    flow: Flow,
    metrics: Arc<Metrics>,
}
impl Client {
    pub(crate) fn record_put_ack(&self, chunks: usize) {
        self.metrics.put_acked_chunks[self.flow as usize]
            .fetch_add(chunks as u64, Ordering::Relaxed);
    }

    #[cfg(test)]
    pub(crate) fn member(
        address: SocketAddr,
        flow: Flow,
        metrics: Arc<Metrics>,
        placement: super::Placement,
        store: [u8; 16],
    ) -> io::Result<Self> {
        Self::member_at(address, flow, metrics, placement, store, 1 - placement.node)
    }
    pub(crate) fn member_at(
        address: SocketAddr,
        flow: Flow,
        metrics: Arc<Metrics>,
        placement: super::Placement,
        store: [u8; 16],
        node: u8,
    ) -> io::Result<Self> {
        let mut client = Self::connect(address, flow, metrics)?;
        let mut status = [0; 40];
        if client.exchange(super::collection::STATUS, &mut [], &mut status)? != status.len()
            || status[0] != node
            || node >= placement.members
            || node == placement.node
            || status[4] != placement.members
            || status[1] != placement.k
            || status[24..] != store
        {
            return Err(io::Error::other("peer fleet identity or placement differs"));
        }
        Ok(client)
    }

    pub fn connect(address: SocketAddr, flow: Flow, metrics: Arc<Metrics>) -> io::Result<Self> {
        let socket = TcpStream::connect_timeout(&address, Duration::from_secs(2))?;
        configure(&socket)?;
        Ok(Self {
            dead: false,
            socket,
            next: 1,
            flow,
            metrics,
        })
    }
    pub fn exchange(&mut self, op: u8, body: &mut [u8], output: &mut [u8]) -> io::Result<usize> {
        if self.dead {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        self.dead = true;
        if body.len() > MAX_BODY {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let id = self.next;
        self.next = self.next.checked_add(1).ok_or_else(invalid)?;
        let mut header = Header {
            op,
            flow: self.flow,
            status: 0,
            id,
            bytes: body.len(),
        }
        .encode();
        let tx = &self.metrics.tx[self.flow as usize];
        let rx = &self.metrics.rx[self.flow as usize];
        transfer(&mut self.socket, &mut header, true, tx, None)?;
        transfer(&mut self.socket, body, true, tx, None)?;
        transfer(&mut self.socket, &mut header, false, rx, None)?;
        let h = Header::decode(&header, true)?;
        if h.id != id || h.op != op || h.flow as u8 != self.flow as u8 || h.bytes > output.len() {
            return Err(invalid());
        }
        transfer(&mut self.socket, &mut output[..h.bytes], false, rx, None)?;
        if h.status == 2 {
            self.dead = false;
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if h.status != 0 {
            return Err(io::Error::other("fleet operation refused or failed"));
        }
        self.dead = false;
        Ok(h.bytes)
    }
}
fn configure(socket: &TcpStream) -> io::Result<()> {
    socket.set_nodelay(true)?;
    socket.set_read_timeout(Some(Duration::from_millis(100)))?;
    socket.set_write_timeout(Some(Duration::from_millis(100)))
}
pub trait Handler: Send + Sync + 'static {
    fn exchange(&self, op: u8, flow: Flow, body: &[u8], output: &mut [u8]) -> io::Result<usize>;
}
pub struct Server {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
}
impl Server {
    pub fn start(
        address: SocketAddr,
        handler: Arc<dyn Handler>,
        budget: &Arc<Budget>,
        metrics: Arc<Metrics>,
        connections: usize,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let mut server = Self {
            address: listener.local_addr()?,
            stop: Arc::new(AtomicBool::new(false)),
            workers: Vec::new(),
        };
        // Administrative transfer, background PUT and its independent proof
        // query may overlap. GET uses another listener and worker pool.
        for _ in 0..connections {
            let listener = listener.try_clone()?;
            let stop = server.stop.clone();
            let handler = handler.clone();
            let metrics = metrics.clone();
            let mut request =
                AlignedBuffer::try_new_in(MAX_BODY, BudgetAllocator::new(budget.clone()))?;
            let mut response =
                AlignedBuffer::try_new_in(MAX_BODY, BudgetAllocator::new(budget.clone()))?;
            server
                .workers
                .push(
                    thread::Builder::new()
                        .name("cas-fleet-bulk".into())
                        .spawn(move || {
                            while !stop.load(Ordering::Acquire) {
                                match listener.accept() {
                                    Ok((mut socket, _)) => {
                                        let _ = configure(&socket).and_then(|()| {
                                            serve(
                                                &mut socket,
                                                &stop,
                                                handler.as_ref(),
                                                &metrics,
                                                request.as_mut_slice(),
                                                response.as_mut_slice(),
                                            )
                                        });
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
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn shutdown(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if self.workers.iter().any(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        for worker in self.workers.drain(..) {
            worker
                .join()
                .map_err(|_| io::Error::other("bulk worker panicked"))?;
        }
        Ok(())
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
fn serve(
    socket: &mut TcpStream,
    stop: &AtomicBool,
    handler: &dyn Handler,
    metrics: &Metrics,
    request: &mut [u8],
    response: &mut [u8],
) -> io::Result<()> {
    loop {
        let mut header = [0; HEADER];
        // The flow is unavailable until the header is validated; malformed
        // headers are excluded, valid headers and partial bodies are counted.
        let discarded = AtomicU64::new(0);
        transfer(socket, &mut header, false, &discarded, Some(stop))?;
        let h = Header::decode(&header, false)?;
        let rx = &metrics.rx[h.flow as usize];
        let tx = &metrics.tx[h.flow as usize];
        rx.fetch_add(HEADER as u64, Ordering::Relaxed);
        transfer(socket, &mut request[..h.bytes], false, rx, Some(stop))?;
        let result = handler.exchange(h.op, h.flow, &request[..h.bytes], response);
        if let Err(error) = &result {
            eprintln!("fleet op {}: {error}", h.op);
        }
        let (status, bytes) = match result {
            Ok(n) if n <= MAX_BODY => (0, n),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (2, 0),
            _ => (1, 0),
        };
        let mut reply = Header { status, bytes, ..h }.encode();
        transfer(socket, &mut reply, true, tx, Some(stop))?;
        transfer(socket, &mut response[..bytes], true, tx, Some(stop))?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lengths_and_flow_are_checked_before_payload_read() {
        let h = Header {
            op: 1,
            flow: Flow::Ship,
            status: 0,
            id: 1,
            bytes: MAX_BODY,
        };
        let bytes = h.encode();
        assert_eq!(Header::decode(&bytes, false).unwrap().bytes, MAX_BODY);
        for index in [0, 4, 8, 10, 11, 20] {
            let mut bad = bytes;
            bad[index] = 255;
            assert!(Header::decode(&bad, false).is_err());
        }
        assert!(
            Header::decode(
                &Header {
                    bytes: MAX_BODY + 1,
                    ..h
                }
                .encode(),
                false
            )
            .is_err()
        );
    }
}
