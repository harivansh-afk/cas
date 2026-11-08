//! Bulk IO crosses the same single store owner as local compaction. Credits and
//! request buffers remain owned after a transport timeout or cancelled caller.
use super::*;
use crate::fleet::{self, chunks, state};

pub(super) struct Runtime {
    pub store: [u8; 16],
    pub state: Mutex<state::State>,
    pub metrics: Arc<fleet::Metrics>,
    pub shipper: OnceLock<chunks::Shipper>,
}
impl Runtime {
    pub fn open(
        resources: &Resources,
        store: &Store,
        images: &[(Log, Manifest)],
        physical: Option<&Arc<cas_core::space::Governor>>,
    ) -> io::Result<Option<BudgetArc<Self>>> {
        let Some(placement) = resources.peer.fleet.placement()? else {
            if store.tickets().root().join("fleet/membership").exists() {
                return Err(io::Error::other(
                    "fleet store requires persisted membership configuration",
                ));
            }
            return Ok(None);
        };
        let mut state =
            state::State::inspect(store.tickets().root(), placement, &resources.metadata)?;
        let mut write = || -> io::Result<()> {
            state.initialize()?;
            for (_, manifest) in images {
                let image = manifest.current().image;
                if state.root(image)?.is_none() {
                    state.put_root(&state::Root {
                        image,
                        generation: 1,
                        writer: placement.node,
                        phase: state::Phase::Claiming,
                        durable: 0,
                        manifest: [0; 32],
                    })?;
                }
            }
            Ok(())
        };
        let bytes = (images.len() as u64 + 1) * 4096 + capacity::METADATA_MARGIN;
        match physical {
            Some(p) => p.foreground(bytes)?.run(write)?,
            None => write()?,
        };
        Ok(Some(BudgetArc::try_new(
            Self {
                store: store.config().store,
                state: Mutex::new(state),
                metrics: Arc::new(fleet::Metrics::default()),
                shipper: OnceLock::new(),
            },
            &resources.metadata,
        )?))
    }
    pub fn writable(&self, image: [u8; 16]) -> io::Result<bool> {
        let state = self.state.lock().unwrap();
        Ok(!state.failed
            && state
                .root(image)?
                .is_some_and(|r| r.writable(state.placement.node))
            && state.epoch()?.is_none_or(|e| e.released))
    }
}
pub(super) struct Request {
    pub channel: mailbox::Sender<Ready>,
    pub op: u8,
    pub flow: fleet::Flow,
    pub body: BudgetVec<u8, BudgetAllocator>,
    pub done: mailbox::Sender<io::Result<BudgetVec<u8, BudgetAllocator>>>,
    pub _credit: cas_core::budget::Lease,
}
impl Request {
    pub fn reject(self) {
        let _ = self.done.try_send(Err(io::ErrorKind::WouldBlock.into()));
    }
}
struct Gateway {
    shared: BudgetArc<SharedHost>,
    ready: mailbox::Sender<Ready>,
}
impl fleet::Handler for Gateway {
    fn exchange(
        &self,
        op: u8,
        flow: fleet::Flow,
        body: &[u8],
        output: &mut [u8],
    ) -> io::Result<usize> {
        if op == 7 && body.is_empty() {
            let runtime = self.shared.fleet.as_ref().unwrap();
            let value = serde_json::json!({"fleet":runtime.metrics.report(),
                "store":self.shared.reader.status(), "cache":self.shared.cache.status(),
                "placement":runtime.state.lock().unwrap().placement});
            let mut destination = &mut *output;
            serde_json::to_writer(&mut destination, &value)?;
            let remaining = destination.len();
            return Ok(output.len() - remaining);
        }
        if op == fleet::chunks::SYNC {
            if !body.is_empty() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let runtime = self.shared.fleet.as_ref().unwrap();
            let count = runtime.shipper.get().unwrap().synchronize()?;
            output[..8].copy_from_slice(&count.to_le_bytes());
            return Ok(8);
        }
        if op == fleet::image::CLAIM {
            let proposed: state::Root = serde_json::from_slice(body)?;
            let runtime = self.shared.fleet.as_ref().unwrap();
            let mut state = runtime.state.lock().unwrap();
            if proposed.generation != 1
                || proposed.writer == state.placement.node
                || proposed.phase != state::Phase::Fenced
                || proposed.durable != 0
                || proposed.manifest != [0; 32]
                || proposed.image == [0; 16]
                || state.epoch()?.is_some_and(|e| !e.released)
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let mut output = || state.put_root(&proposed);
            match &self.shared.physical {
                Some(p) => p
                    .foreground(4096 + capacity::METADATA_MARGIN)?
                    .run(output)?,
                None => output()?,
            }
            return Ok(0);
        }
        if op == fleet::image::ROOT {
            let image: [u8; 16] = body.try_into().map_err(|_| io::ErrorKind::InvalidInput)?;
            let runtime = self.shared.fleet.as_ref().unwrap();
            let state = runtime.state.lock().unwrap();
            let root = state.root(image)?.ok_or(io::ErrorKind::NotFound)?;
            let mut destination = &mut *output;
            serde_json::to_writer(&mut destination, &root)?;
            let remaining = destination.len();
            return Ok(output.len() - remaining);
        }
        if op == fleet::collection::STATUS {
            if !body.is_empty() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let runtime = self.shared.fleet.as_ref().unwrap();
            let state = runtime.state.lock().unwrap();
            let epoch = state.epoch()?;
            output[..40].fill(0);
            output[24..40].copy_from_slice(&runtime.store);
            output[0] = state.placement.node;
            output[1] = state.placement.k;
            if let Some(epoch) = epoch {
                output[2] = u8::from(!epoch.released);
                output[3] = u8::from(epoch.swept);
                output[8..16].copy_from_slice(&epoch.id.to_le_bytes());
            }
            return Ok(40);
        }
        if op == fleet::collection::RELEASE && body.len() == 8 {
            let runtime = self.shared.fleet.as_ref().unwrap();
            let state = runtime.state.lock().unwrap();
            let id = u64::from_le_bytes(body.try_into().unwrap());
            if state.epoch()?.is_some_and(|e| e.id == id && e.released) {
                return Ok(0);
            }
        }
        if op == chunks::HAS {
            if body.is_empty() || !body.len().is_multiple_of(32) || body.len() > 63 * 32 {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let n = (body.len() / 32).div_ceil(8);
            output[..n].fill(0);
            for (i, hash) in body.as_chunks::<32>().0.iter().enumerate() {
                if self.shared.reader.plan(*hash)?.is_none() {
                    output[i / 8] |= 1 << (i % 8);
                }
            }
            return Ok(n);
        }
        let credit = self
            .shared
            .resources
            .pools
            .administrative()
            .ok_or(io::ErrorKind::WouldBlock)?;
        let mut bytes = reserved_vec(body.len(), &self.shared.resources.metadata)?;
        bytes.extend_from_slice(body);
        let (done, receive) = mailbox::bounded(1, &self.shared.resources.metadata)?;
        self.ready
            .try_send(Ready::Fleet(Request {
                channel: self.ready.clone(),
                op,
                flow,
                body: bytes,
                done,
                _credit: credit,
            }))
            .map_err(|_| io::ErrorKind::WouldBlock)?;
        let reply = receive
            .recv_timeout(IO_DEADLINE)
            .map_err(|_| io::ErrorKind::TimedOut)??;
        if reply.len() > output.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        output[..reply.len()].copy_from_slice(&reply);
        Ok(reply.len())
    }
}
pub(super) fn start(
    shared: &BudgetArc<SharedHost>,
    ready: &mailbox::Sender<Ready>,
) -> io::Result<Option<fleet::Server>> {
    let Some(runtime) = &shared.fleet else {
        return Ok(None);
    };
    let config = &shared.resources.peer.fleet;
    let placement = runtime.state.lock().unwrap().placement;
    let shipper = chunks::Shipper::start(
        config.peer_bulk_address.unwrap(),
        (placement, runtime.store),
        shared.reader.clone(),
        &shared.resources.metadata,
        runtime.metrics.clone(),
        shared.io_scheduler.clone(),
    )?;
    runtime
        .shipper
        .set(shipper)
        .map_err(|_| io::Error::other("duplicate fleet shipper"))?;
    fleet::Server::start(
        config.peer_bulk_listen.unwrap(),
        Arc::new(Gateway {
            shared: shared.clone(),
            ready: ready.clone(),
        }),
        &shared.resources.metadata,
        runtime.metrics.clone(),
    )
    .map(Some)
}
impl worker::Owner {
    pub fn bulk(&mut self, request: Request) {
        let result = self.bulk_inner(request.op, request.flow, &request.body, &request.channel);
        if self.store.status().failed || self.shared.account_failed() {
            self.shared.gate.fail("fleet output failed".into());
        }
        let _ = request.done.try_send(result);
    }
    fn bulk_inner(
        &mut self,
        op: u8,
        _flow: fleet::Flow,
        body: &[u8],
        ready: &mailbox::Sender<Ready>,
    ) -> io::Result<BudgetVec<u8, BudgetAllocator>> {
        if self.shared.fleet.is_none() {
            return Err(io::ErrorKind::Unsupported.into());
        }
        if self.shared.admission.status().paused {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let mut response = reserved_vec(4096, &self.shared.resources.metadata)?;
        match op {
            fleet::image::EXPORT => {
                serde_json::to_writer(&mut response, &self.export_image(body)?)?;
                return Ok(response);
            }
            fleet::image::IMPORT => {
                response.push(self.import_image(body)?);
                return Ok(response);
            }
            fleet::image::IMPORT_WRITE => {
                self.import_bytes(body)?;
                return Ok(response);
            }
            fleet::image::IMPORT_FINISH => {
                self.finish_import(body, ready)?;
                return Ok(response);
            }
            chunks::PUT => (),
            _ => return Err(io::ErrorKind::Unsupported.into()),
        }
        let bytes = self.store.config().segment_bytes + capacity::METADATA_MARGIN;
        let permit = self
            .shared
            .physical
            .as_ref()
            .map(|p| p.background(bytes))
            .transpose()?;
        let mut output = || {
            chunks::decode(body, |chunks| {
                #[cfg(test)]
                fleet::crash_point("put-before");
                self.store.insert(chunks)?;
                #[cfg(test)]
                fleet::crash_point("put-durable");
                Ok(())
            })
        };
        match permit {
            Some(p) => p.run(output)?,
            None => output()?,
        };
        reserved_vec(0, &self.shared.resources.metadata)
    }
}

pub(super) fn claim(shared: &SharedHost, image: [u8; 16]) -> io::Result<()> {
    let Some(runtime) = &shared.fleet else {
        return Ok(());
    };
    let root = runtime
        .state
        .lock()
        .unwrap()
        .root(image)?
        .ok_or(io::ErrorKind::NotFound)?;
    if root.phase != state::Phase::Claiming {
        return Ok(());
    }
    let mut peer = fleet::Client::member(
        shared.resources.peer.fleet.peer_bulk_address.unwrap(),
        fleet::Flow::Provision,
        runtime.metrics.clone(),
        runtime.state.lock().unwrap().placement,
        runtime.store,
    )?;
    let mut proposed = root.clone();
    proposed.phase = state::Phase::Fenced;
    peer.exchange(
        fleet::image::CLAIM,
        &mut serde_json::to_vec(&proposed)?,
        &mut [],
    )?;
    let mut state = runtime.state.lock().unwrap();
    if state.root(image)? != Some(root.clone()) || state.epoch()?.is_some_and(|e| !e.released) {
        return Err(io::ErrorKind::WouldBlock.into());
    }
    proposed.phase = state::Phase::Active;
    let mut output = || state.put_root(&proposed);
    match &shared.physical {
        Some(p) => p.foreground(4096 + capacity::METADATA_MARGIN)?.run(output),
        None => output(),
    }
}
