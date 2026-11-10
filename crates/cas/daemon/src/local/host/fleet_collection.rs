use super::*;
use crate::fleet::{self as protocol, collection as wire, state::Epoch};

struct Live {
    local: BudgetVec<[u8; 32], BudgetAllocator>,
    peer: BudgetVec<[u8; 32], BudgetAllocator>,
    expected: Option<(usize, [u8; 32])>,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn digest(hashes: &[[u8; 32]]) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    for hash in hashes {
        h.update(hash);
    }
    *h.finalize().as_bytes()
}
impl worker::Owner {
    pub fn freeze(&mut self, request: fleet::Request) {
        if request.body.len() != 8 {
            let _ = request.done.try_send(Err(invalid()));
            return;
        }
        let epoch = u64::from_le_bytes(request.body[..].try_into().unwrap());
        let result = self.freeze_epoch(epoch, Some(request));
        if let Err(e) = result {
            self.shared.gate.fail(e.to_string());
        }
    }
    pub fn resume_fleet_epoch(&mut self) -> io::Result<()> {
        let epoch = self
            .shared
            .fleet
            .as_ref()
            .map(|r| r.state.lock().unwrap().epoch())
            .transpose()?
            .flatten();
        if let Some(epoch) = epoch.filter(|e| !e.released) {
            self.freeze_epoch(epoch.id, None)?;
        }
        Ok(())
    }
    fn freeze_epoch(&mut self, id: u64, mut request: Option<fleet::Request>) -> io::Result<()> {
        let runtime = self.shared.fleet.as_ref().ok_or_else(invalid)?.clone();
        let previous = runtime.state.lock().unwrap().epoch()?;
        if id == 0
            || previous
                .as_ref()
                .is_some_and(|old| old.id > id || old.id == id && old.released)
        {
            if let Some(request) = request.take() {
                let _ = request.done.try_send(Err(invalid()));
            }
            return Ok(());
        }
        let placement = runtime.state.lock().unwrap().placement;
        let shipper = runtime.shipper.get().unwrap();
        shipper.pause();
        let started = Instant::now();
        while !shipper.drained() {
            if started.elapsed() > IO_DEADLINE {
                return Err(io::ErrorKind::TimedOut.into());
            }
            thread::sleep(Duration::from_millis(1));
        }
        let result = self.quiesced(|owner, _generation, _started| {
            let mut live = Live {
                local: reserved_vec(0, &owner.shared.resources.compaction)?,
                peer: reserved_vec(0, &owner.shared.resources.compaction)?,
                expected: None,
            };
            let mut mark = |hash: [u8; 32]| -> io::Result<()> {
                live.local
                    .try_reserve(1)
                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                live.local.push(hash);
                Ok(())
            };
            for endpoint in &mut owner.endpoints {
                if endpoint.retired {
                    continue;
                }
                endpoint.healthy()?;
                endpoint
                    .manifest
                    .pinned_roots(owner.shared.resources.compaction.clone())?
                    .walk(|e| e.hash.map_or(Ok(()), &mut mark))?;
            }
            for snapshot in &mut owner.snapshots {
                snapshot
                    .pinned_roots(owner.shared.resources.compaction.clone())?
                    .walk(|e| e.hash.map_or(Ok(()), &mut mark))?;
            }
            live.local.sort_unstable();
            live.local.dedup();
            let mut epoch = Epoch {
                id,
                swept: previous.as_ref().is_some_and(|e| e.id == id && e.swept),
                released: false,
            };
            owner.epoch(&epoch)?;
            #[cfg(test)]
            protocol::crash_point("gc-frozen");
            let mut report = CollectionReport::default();
            if let Some(request) = request.take() {
                let _ = request.done.try_send(live.receipt(&owner.shared));
            }
            loop {
                let next = owner.input.recv().map_err(|_| io::ErrorKind::BrokenPipe)?;
                let request = match next {
                    Ready::Fleet(request) => request,
                    Ready::Collect(request) => {
                        request.complete(Err(io::ErrorKind::WouldBlock.into()));
                        continue;
                    }
                    Ready::Snapshot(request) => {
                        request.done.complete(Err(io::ErrorKind::WouldBlock.into()));
                        continue;
                    }
                    Ready::Image { .. } => continue,
                };
                let result = (|| -> io::Result<BudgetVec<u8, BudgetAllocator>> {
                    if request.body.len() < 8
                        || u64::from_le_bytes(request.body[..8].try_into().unwrap()) != id
                    {
                        return Err(invalid());
                    }
                    match request.op {
                        wire::FREEZE => live.receipt(&owner.shared),
                        wire::LIVE_READ => {
                            if request.body.len() != 16 {
                                return Err(invalid());
                            }
                            let offset = usize::try_from(u64::from_le_bytes(
                                request.body[8..].try_into().unwrap(),
                            ))
                            .map_err(|_| invalid())?;
                            if offset > live.local.len() {
                                return Err(invalid());
                            }
                            let end = (offset + 128).min(live.local.len());
                            let mut out = reserved_vec(
                                (end - offset) * 32,
                                &owner.shared.resources.metadata,
                            )?;
                            for hash in &live.local[offset..end] {
                                out.extend_from_slice(hash);
                            }
                            Ok(out)
                        }
                        wire::LIVE_ADD => {
                            live.accept(&request.body)?;
                            reserved_vec(0, &owner.shared.resources.metadata)
                        }
                        wire::SWEEP => {
                            live.complete()?;
                            if !epoch.swept {
                                owner.fleet_sweep(&live, &mut report)?;
                                epoch.swept = true;
                                owner.epoch(&epoch)?;
                                #[cfg(test)]
                                protocol::crash_point("gc-swept");
                            }
                            report.pause_micros =
                                started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
                            let mut out = reserved_vec(4096, &owner.shared.resources.metadata)?;
                            serde_json::to_writer(&mut out, &report)?;
                            Ok(out)
                        }
                        wire::RELEASE => {
                            if !epoch.swept {
                                return Err(invalid());
                            }
                            for (node, address) in owner.shared.resources.peer.fleet.peers()? {
                                let mut peer = protocol::Client::member_at(
                                    address,
                                    protocol::Flow::Collection,
                                    runtime.metrics.clone(),
                                    placement,
                                    runtime.store,
                                    node,
                                )?;
                                let mut status = [0; 40];
                                if peer.exchange(wire::STATUS, &mut [], &mut status)? != 40
                                    || u64::from_le_bytes(status[8..16].try_into().unwrap()) != id
                                    || status[3] == 0
                                {
                                    return Err(invalid());
                                }
                            }
                            epoch.released = true;
                            owner.epoch(&epoch)?;
                            reserved_vec(0, &owner.shared.resources.metadata)
                        }
                        _ => Err(io::ErrorKind::WouldBlock.into()),
                    }
                })();
                if request.op == wire::RELEASE && result.is_ok() {
                    return Ok((request, result));
                }
                let _ = request.done.try_send(result);
            }
        });
        match result {
            Ok((request, result)) => {
                shipper.resume();
                let _ = request.done.try_send(result);
                Ok(())
            }
            Err(error) => {
                if let Some(request) = request {
                    let _ = request
                        .done
                        .try_send(Err(io::Error::new(error.kind(), error.to_string())));
                }
                Err(error)
            }
        }
    }
    fn epoch(&self, epoch: &Epoch) -> io::Result<()> {
        let runtime = self.shared.fleet.as_ref().unwrap();
        let mut state = runtime.state.lock().unwrap();
        let output = || state.set_epoch(epoch);
        match &self.shared.physical {
            Some(p) => p.foreground(capacity::METADATA_MARGIN + 4096)?.run(output),
            None => {
                let mut output = output;
                output()
            }
        }
    }
    fn fleet_sweep(&mut self, live: &Live, report: &mut CollectionReport) -> io::Result<()> {
        let runtime = self.shared.fleet.as_ref().unwrap().clone();
        let mut state = runtime.state.lock().unwrap();
        let placement = state.placement;
        let mut all = reserved_vec(
            live.local.len() + live.peer.len(),
            &self.shared.resources.compaction,
        )?;
        all.extend_from_slice(&live.local);
        all.extend_from_slice(&live.peer);
        all.sort_unstable();
        all.dedup();
        let mut keep = reserved_vec(all.len(), &self.shared.resources.compaction)?;
        let mut remote = reserved_vec(all.len(), &self.shared.resources.compaction)?;
        for hash in &all {
            if placement.local(hash) {
                if self.shared.reader.plan(*hash)?.is_none() {
                    return Err(io::Error::other(
                        "owner is missing a live chunk; synchronize before sweep",
                    ));
                }
                keep.push(*hash);
            } else {
                remote.push(*hash);
            }
        }
        let mut confirmed = reserved_vec(remote.len(), &self.shared.resources.compaction)?;
        confirmed.extend_from_slice(&remote);
        let mut frame = [0; 63 * 32];
        let mut missing = [0; 8];
        for (node, address) in self.shared.resources.peer.fleet.peers()? {
            let mut peer = protocol::Client::member_at(
                address,
                protocol::Flow::Collection,
                runtime.metrics.clone(),
                placement,
                runtime.store,
                node,
            )?;
            let mut owned = reserved_vec(remote.len(), &self.shared.resources.compaction)?;
            owned.extend(
                remote
                    .iter()
                    .filter(|hash| placement.owns(hash, node))
                    .copied(),
            );
            for batch in owned.chunks(63) {
                protocol::chunks::has(&mut peer, batch, &mut frame, &mut missing)?;
                for (i, hash) in batch.iter().enumerate() {
                    if missing[i / 8] & (1 << (i % 8)) != 0 {
                        if self.shared.reader.plan(*hash)?.is_none() {
                            return Err(io::Error::other(
                                "live chunk lacks a durable owner copy; repair before sweep",
                            ));
                        }
                        keep.push(*hash);
                        if let Ok(index) = confirmed.binary_search(hash) {
                            confirmed.remove(index);
                        }
                    }
                }
            }
        }
        // This receipt is durable before the only local copy can be unlinked.
        let bytes = state.confirmation_bytes(confirmed.len())? + capacity::METADATA_MARGIN;
        let output = || state.confirm(&confirmed);
        match &self.shared.physical {
            Some(p) => p.foreground(bytes)?.run(output)?,
            None => {
                let mut output = output;
                output()?
            }
        }
        drop(state);
        let mut collection = self.store.begin_collection()?;
        for hash in &keep {
            collection.mark(hash)?;
        }
        let mut sweep = collection.finish_marking()?;
        while let Some(victim) = sweep.next_victim() {
            let output = || sweep.clean_next();
            match &self.shared.physical {
                Some(p) => {
                    p.background(victim.destination_bytes + capacity::METADATA_MARGIN)?
                        .run(output)?;
                }
                None => {
                    let mut output = output;
                    output()?;
                }
            }
        }
        report.chunks = sweep.finish()?;
        report.rounds = 1;
        Ok(())
    }
}
impl Live {
    fn receipt(&self, shared: &SharedHost) -> io::Result<BudgetVec<u8, BudgetAllocator>> {
        let mut out = reserved_vec(40, &shared.resources.metadata)?;
        out.extend_from_slice(&(self.local.len() as u64).to_le_bytes());
        out.extend_from_slice(&digest(&self.local));
        Ok(out)
    }
    fn accept(&mut self, body: &[u8]) -> io::Result<()> {
        if body.len() < 56 || !(body.len() - 56).is_multiple_of(32) {
            return Err(invalid());
        }
        let count = usize::try_from(u64::from_le_bytes(body[8..16].try_into().unwrap()))
            .map_err(|_| invalid())?;
        let hash: [u8; 32] = body[16..48].try_into().unwrap();
        let offset = usize::try_from(u64::from_le_bytes(body[48..56].try_into().unwrap()))
            .map_err(|_| invalid())?;
        if offset == 0 {
            self.peer.clear();
            self.expected = Some((count, hash));
            self.peer
                .try_reserve_exact(count)
                .map_err(|_| io::ErrorKind::OutOfMemory)?;
        }
        if self.expected != Some((count, hash))
            || offset != self.peer.len()
            || body[56..].len() / 32 > count.saturating_sub(offset)
        {
            return Err(invalid());
        }
        self.peer.extend_from_slice(body[56..].as_chunks::<32>().0);
        Ok(())
    }
    fn complete(&self) -> io::Result<()> {
        if self.expected != Some((self.peer.len(), digest(&self.peer)))
            || self.peer.windows(2).any(|p| p[0] >= p[1])
        {
            Err(invalid())
        } else {
            Ok(())
        }
    }
}
