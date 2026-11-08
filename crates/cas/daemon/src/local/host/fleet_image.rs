//! Durable receiving roots remain non-writable until the complete dependency
//! graph and the peer's fenced generation have both been checked.
use super::*;
use crate::fleet::{
    self as protocol,
    image::{self as wire, Metadata, Mode},
    state::{Phase, Root},
};
use cas_core::catalog::{Change, Entry, Kind as CatalogKind};
use std::{
    fs::{self, File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::PathBuf,
};

pub(super) struct Incoming {
    meta: Metadata,
    files: Option<[File; 2]>,
    received: [u64; 2],
    hashes: [blake3::Hasher; 2],
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn micros(start: Instant) -> u64 {
    start.elapsed().as_micros().try_into().unwrap_or(u64::MAX)
}
fn buffered(view: &cas_core::manifest::file::View) -> io::Result<File> {
    File::open(format!("/proc/self/fd/{}", view.file().as_raw_fd()))
}
fn checksum(file: &File, length: u64) -> io::Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut bytes = [0; 64 * 1024];
    let mut offset = 0;
    while offset < length {
        let wanted = bytes.len().min((length - offset) as usize);
        file.read_exact_at(&mut bytes[..wanted], offset)?;
        hasher.update(&bytes[..wanted]);
        offset += wanted as u64;
    }
    Ok(*hasher.finalize().as_bytes())
}
fn spool(root: &std::path::Path, meta: &Metadata, part: usize) -> PathBuf {
    root.join("fleet").join(format!(
        "incoming-{:032x}-{}-{part}",
        u128::from_be_bytes(meta.image),
        meta.generation
    ))
}
impl worker::Owner {
    pub fn export_image(&mut self, body: &[u8]) -> io::Result<protocol::ImageReport> {
        let request: wire::Request = serde_json::from_slice(body)?;
        let runtime = self.shared.fleet.as_ref().ok_or_else(invalid)?.clone();
        if request.destination != self.shared.resources.peer.fleet.peer_bulk_address.unwrap()
            || request.target == [0; 16]
            || (request.mode == Mode::Migration) != (request.target == request.source)
        {
            return Err(invalid());
        }
        let index = self
            .endpoints
            .iter()
            .position(|e| !e.retired && e.manifest.current().image == request.source)
            .ok_or(io::ErrorKind::NotFound)?;
        let original = runtime
            .state
            .lock()
            .unwrap()
            .root(request.source)?
            .ok_or_else(invalid)?;
        let node = runtime.state.lock().unwrap().placement.node;
        if !original.writable(node)
            && !(request.mode == Mode::Migration
                && original.phase == Phase::Fenced
                && original.writer == 1 - node)
        {
            return Err(invalid());
        }
        if self.catalog.is_none() {
            return Err(io::Error::other(
                "image transfer requires catalog ownership",
            ));
        }
        let mut peer = protocol::Client::member(
            request.destination,
            request.mode.flow(),
            runtime.metrics.clone(),
            runtime.state.lock().unwrap().placement,
            runtime.store,
        )?;
        let started = Instant::now();
        self.quiesced(|owner, generation, _| {
            let freeze_micros = micros(started);
            let cut = if owner.shared.admission.attached(index) {
                owner.endpoints[index].health.lock()?.durable
            } else {
                let attachments = owner
                    .attachments
                    .upgrade()
                    .ok_or(io::ErrorKind::BrokenPipe)?;
                let images = attachments.lock().unwrap();
                images
                    .iter()
                    .flatten()
                    .find(|a| a.image == request.source)
                    .ok_or_else(invalid)?
                    .log
                    .status()
                    .durable
            };
            if request.mode == Mode::Provision {
                while owner.endpoints[index].manifest.current().durable < cut {
                    if owner.endpoints[index].quiescent != Some(generation) {
                        return Err(io::Error::other(
                            "provision source must be compacted or attached",
                        ));
                    }
                    let before = owner.endpoints[index].manifest.current().durable;
                    owner.endpoints[index].compact(
                        &mut owner.store,
                        owner.shared.physical.as_ref(),
                        Some(generation),
                    )?;
                    if owner.endpoints[index].manifest.current().durable == before {
                        return Err(io::ErrorKind::WouldBlock.into());
                    }
                }
            }
            let view = owner.endpoints[index].manifest.view()?;
            let tail = if owner.shared.admission.attached(index) {
                let Reply::Transfer(tail) =
                    owner.endpoints[index].exchange(Event::Transfer(generation))?
                else {
                    return Err(invalid());
                };
                tail
            } else {
                let attachments = owner
                    .attachments
                    .upgrade()
                    .ok_or(io::ErrorKind::BrokenPipe)?;
                let images = attachments.lock().unwrap();
                images
                    .iter()
                    .flatten()
                    .find(|a| a.image == request.source)
                    .ok_or_else(invalid)?
                    .log
                    .capture_transfer(owner.shared.resources.read_memory())
                    .map_err(io::Error::other)?
            };
            let manifest = buffered(&view)?;
            let mut tail_file = tempfile::tempfile_in(owner.store.tickets().root().join("fleet"))?;
            // A logical tail is smaller than its original WAL framing. The
            // per-image staging quota bounds the scratch export reservation.
            let bound = append::Limits::default().staging_bytes + capacity::METADATA_MARGIN;
            let output = || tail.write(&mut tail_file);
            match &owner.shared.physical {
                Some(p) => p.foreground(bound)?.run(output)?,
                None => {
                    let mut output = output;
                    output()?
                }
            }
            let tail_bytes = tail_file.metadata()?.len();
            let meta = Metadata {
                store: view.commit().store,
                source: request.source,
                image: request.target,
                image_bytes: view.commit().image_bytes,
                segment_bytes: owner.store.config().segment_bytes,
                d: view.commit().durable,
                e: cut,
                mode: request.mode,
                generation: if request.mode == Mode::Provision {
                    1
                } else if original.phase == Phase::Fenced {
                    original.generation
                } else {
                    original.generation.checked_add(1).ok_or_else(invalid)?
                },
                writer: 1 - node,
                manifest_bytes: view.end(),
                manifest_hash: checksum(&manifest, view.end())?,
                tail_bytes,
                tail_hash: checksum(&tail_file, tail_bytes)?,
            };
            // Verify the future local owner already has its chunks. Provision
            // never silently turns into an image data copy.
            let placement = runtime.state.lock().unwrap().placement;
            let mut hashes = reserved_vec(0, &owner.shared.resources.compaction)?;
            view.tree()?.walk(|e| {
                if let Some(hash) = e.hash
                    && placement.remote(&hash)
                {
                    hashes
                        .try_reserve(1)
                        .map_err(|_| io::ErrorKind::OutOfMemory)?;
                    hashes.push(hash);
                }
                Ok(())
            })?;
            hashes.sort_unstable();
            hashes.dedup();
            let mut frame = [0; 63 * 32];
            let mut missing = [0; 8];
            for batch in hashes.chunks(63) {
                loop {
                    protocol::chunks::has(&mut peer, batch, &mut frame, &mut missing)?;
                    if (0..batch.len()).all(|i| missing[i / 8] & (1 << (i % 8)) == 0) {
                        break;
                    }
                    if started.elapsed() > IO_DEADLINE {
                        return Err(io::ErrorKind::TimedOut.into());
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            }
            let prepared_at = micros(started);
            let prepare_micros = prepared_at - freeze_micros;
            if request.mode == Mode::Migration {
                let fenced = if original.phase == Phase::Fenced {
                    if original.manifest != meta.digest()? || original.durable != cut {
                        return Err(invalid());
                    }
                    original.clone()
                } else {
                    original.handoff(node, cut, meta.digest()?)?
                };
                owner.root(&fenced)?;
                #[cfg(test)]
                protocol::crash_point("source-fenced");
            } else {
                owner.root(&meta.root(Phase::Fenced)?)?;
            }
            let mut begin = serde_json::to_vec(&meta)?;
            let mut status = [0];
            if peer.exchange(wire::IMPORT, &mut begin, &mut status)? != 1 {
                return Err(invalid());
            }
            let fenced_at = micros(started);
            let fence_micros = fenced_at - prepared_at;
            if status[0] == 0 {
                for (part, file, length) in [
                    (0u8, &manifest, meta.manifest_bytes),
                    (1, &tail_file, meta.tail_bytes),
                ] {
                    let mut offset = 0u64;
                    while offset < length {
                        let mut bytes = [0; 41 + 64 * 1024];
                        bytes[..32].copy_from_slice(&meta.digest()?);
                        bytes[32] = part;
                        bytes[33..41].copy_from_slice(&offset.to_le_bytes());
                        let n = (length - offset).min(64 * 1024) as usize;
                        file.read_exact_at(&mut bytes[41..41 + n], offset)?;
                        peer.exchange(wire::IMPORT_WRITE, &mut bytes[..41 + n], &mut [])?;
                        offset += n as u64;
                    }
                }
            } else if status[0] != 1 {
                return Err(invalid());
            }
            let transferred_at = micros(started);
            let transfer_micros = transferred_at - fenced_at;
            peer.exchange(wire::IMPORT_FINISH, &mut meta.digest()?, &mut [])?;
            let installed_at = micros(started);
            let install_micros = installed_at - transferred_at;
            if request.mode == Mode::Migration
                && owner.endpoints[index].quiescent == Some(generation)
            {
                match owner.endpoints[index].exchange(Event::FenceWriter(generation))? {
                    Reply::Applied => (),
                    _ => return Err(invalid()),
                };
                owner.endpoints[index].quiescent = None;
                let mut health = owner.endpoints[index].health.lock()?;
                owner.endpoints[index].health.fence_writer();
                health.fail("image writer transferred to peer".into());
            }
            let total_micros = micros(started);
            Ok(protocol::ImageReport {
                image: meta.image,
                mode: request.mode,
                manifest_bytes: meta.manifest_bytes,
                tail_bytes: meta.tail_bytes,
                freeze_micros,
                prepare_micros,
                fence_micros,
                transfer_micros,
                install_micros,
                finalize_micros: total_micros - installed_at,
                total_micros,
            })
        })
    }
    fn root(&self, root: &Root) -> io::Result<()> {
        let runtime = self.shared.fleet.as_ref().unwrap();
        let mut state = runtime.state.lock().unwrap();
        let output = || state.put_root(root);
        match &self.shared.physical {
            Some(p) => p.foreground(4096 + capacity::METADATA_MARGIN)?.run(output),
            None => {
                let mut output = output;
                output()
            }
        }
    }
    fn proof(&self, meta: &Metadata) -> io::Result<()> {
        let runtime = self.shared.fleet.as_ref().unwrap();
        let mut peer = protocol::Client::member(
            self.shared.resources.peer.fleet.peer_bulk_address.unwrap(),
            meta.mode.flow(),
            runtime.metrics.clone(),
            runtime.state.lock().unwrap().placement,
            runtime.store,
        )?;
        let mut output = [0; 4096];
        let mut image = meta.image;
        let n = peer.exchange(wire::ROOT, &mut image, &mut output)?;
        let root: Root = serde_json::from_slice(&output[..n])?;
        if root != meta.root(Phase::Fenced)? {
            return Err(io::Error::other(
                "peer has not fenced this exact writer generation",
            ));
        }
        Ok(())
    }
    pub fn import_image(&mut self, body: &[u8]) -> io::Result<u8> {
        let meta: Metadata = serde_json::from_slice(body)?;
        let runtime = self.shared.fleet.as_ref().ok_or_else(invalid)?.clone();
        let placement = runtime.state.lock().unwrap().placement;
        if meta.store != self.store.config().store
            || meta.segment_bytes != self.store.config().segment_bytes
            || meta.writer != placement.node
            || meta.image == [0; 16]
            || meta.generation == 0
            || meta.e < meta.d
            || meta.manifest_bytes < 8192
            || !meta.manifest_bytes.is_multiple_of(BLOCK_SIZE as u64)
            || meta.manifest_bytes > i64::MAX as u64
            || meta.tail_bytes < 24
            || meta.tail_bytes > append::Limits::default().staging_bytes
            || (meta.mode == Mode::Migration) != (meta.image == meta.source)
            || meta.mode == Mode::Provision && meta.e != meta.d
        {
            return Err(invalid());
        }
        if self.shared.admission.status().paused
            || self.incoming.as_ref().is_some_and(|old| old.meta != meta)
        {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.proof(&meta)?;
        let root = runtime.state.lock().unwrap().root(meta.image)?;
        // Returning migration may replace an older, fenced incarnation. Retire
        // its catalog reference before changing files or publishing the higher
        // receiving cut. A crash then sees either the old complete fenced image,
        // or no image; it never opens old bytes under the new durable prefix.
        if meta.mode == Mode::Migration
            && root.as_ref().is_some_and(|r| {
                r.phase == Phase::Fenced
                    && r.writer != placement.node
                    && r.generation.checked_add(1) == Some(meta.generation)
            })
            && self
                .catalog
                .as_ref()
                .ok_or_else(invalid)?
                .contents()
                .get(meta.image)
                .is_some()
        {
            let index = self
                .endpoints
                .iter()
                .position(|e| !e.retired && e.manifest.current().image == meta.image)
                .ok_or_else(invalid)?;
            if self.shared.admission.attached(index) {
                return Err(io::Error::other(
                    "detach the previous writer before migrating back",
                ));
            }
            let prepared = self
                .catalog
                .as_ref()
                .unwrap()
                .prepare(Change::Remove(meta.image))?;
            let bytes = prepared.output_bytes() as u64 + capacity::METADATA_MARGIN;
            let output = || self.catalog.as_mut().unwrap().publish(prepared);
            match &self.shared.physical {
                Some(p) => p.foreground(bytes)?.run(output)?,
                None => output()?,
            }
            self.endpoints[index].retired = true;
            self.endpoints[index].health.fence_writer();
            self.attachments
                .upgrade()
                .ok_or(io::ErrorKind::BrokenPipe)?
                .lock()
                .unwrap()[index]
                .take();
            #[cfg(test)]
            protocol::crash_point("destination-retired");
        }
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| io::Error::other("image transfer requires a catalog"))?;
        let complete = root == Some(meta.root(Phase::Active)?);
        if catalog.contents().get(meta.image).is_some() {
            if !complete && root != Some(meta.root(Phase::Receiving)?) {
                return Err(io::ErrorKind::AlreadyExists.into());
            }
            // Catalog publication follows the complete durable graph. A crash
            // before root activation can finish without overwriting live files.
            self.incoming = Some(Incoming {
                meta,
                files: None,
                received: [0; 2],
                hashes: std::array::from_fn(|_| blake3::Hasher::new()),
            });
            return Ok(1);
        }
        if complete || self.endpoints.len() == self.shared.compaction.len() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        self.root(&meta.root(Phase::Receiving)?)?;
        #[cfg(test)]
        protocol::crash_point("destination-receiving");
        let files = [
            spool(self.store.tickets().root(), &meta, 0),
            spool(self.store.tickets().root(), &meta, 1),
        ]
        .map(|path| {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)
        });
        let [first, second] = files;
        self.incoming = Some(Incoming {
            meta,
            files: Some([first?, second?]),
            received: [0; 2],
            hashes: std::array::from_fn(|_| blake3::Hasher::new()),
        });
        Ok(0)
    }
    pub fn import_bytes(&mut self, body: &[u8]) -> io::Result<()> {
        if !(42..=41 + 64 * 1024).contains(&body.len()) {
            return Err(invalid());
        }
        let incoming = self.incoming.as_mut().ok_or_else(invalid)?;
        if body[..32] != incoming.meta.digest()? || body[32] > 1 {
            return Err(invalid());
        }
        let part = body[32] as usize;
        let offset = u64::from_le_bytes(body[33..41].try_into().unwrap());
        let bytes = &body[41..];
        let length = if part == 0 {
            incoming.meta.manifest_bytes
        } else {
            incoming.meta.tail_bytes
        };
        let file = &incoming.files.as_ref().ok_or_else(invalid)?[part];
        if offset < incoming.received[part] {
            if bytes.len() as u64 > incoming.received[part] - offset {
                return Err(invalid());
            }
            let mut old = [0; 64 * 1024];
            file.read_exact_at(&mut old[..bytes.len()], offset)?;
            if &old[..bytes.len()] != bytes {
                return Err(invalid());
            }
            return Ok(());
        }
        if offset != incoming.received[part] || bytes.len() as u64 > length.saturating_sub(offset) {
            return Err(invalid());
        }
        let output = || file.write_all_at(bytes, offset);
        match &self.shared.physical {
            Some(p) => p
                .foreground(bytes.len() as u64 + capacity::METADATA_MARGIN)?
                .run(output)?,
            None => output()?,
        };
        incoming.hashes[part].update(bytes);
        incoming.received[part] += bytes.len() as u64;
        #[cfg(test)]
        protocol::crash_point("destination-bytes");
        Ok(())
    }
    pub fn finish_import(&mut self, body: &[u8], ready: &mailbox::Sender<Ready>) -> io::Result<()> {
        let incoming = self.incoming.take().ok_or_else(invalid)?;
        let result = self.install(&incoming, body, ready);
        if result.is_err() {
            self.incoming = Some(incoming);
        }
        result
    }
    fn install(
        &mut self,
        incoming: &Incoming,
        body: &[u8],
        ready: &mailbox::Sender<Ready>,
    ) -> io::Result<()> {
        let meta = &incoming.meta;
        if body != meta.digest()? {
            return Err(invalid());
        }
        if incoming.files.is_none() {
            if !self
                .endpoints
                .iter()
                .any(|e| !e.retired && e.manifest.current().image == meta.image)
            {
                return Err(io::Error::other(
                    "published import needs cold recovery before activation",
                ));
            }
            self.proof(meta)?;
            return self.root(&meta.root(Phase::Active)?);
        }
        if incoming.received != [meta.manifest_bytes, meta.tail_bytes]
            || incoming.hashes[0].finalize().as_bytes() != &meta.manifest_hash
            || incoming.hashes[1].finalize().as_bytes() != &meta.tail_hash
        {
            return Err(invalid());
        }
        for file in incoming.files.as_ref().unwrap() {
            file.sync_all()?;
        }
        let entry = Entry {
            id: meta.image,
            kind: CatalogKind::Image {
                image_bytes: meta.image_bytes,
            },
        };
        let directory = recovery::path(self.store.tickets().root(), entry);
        let catalog = self.catalog.as_ref().ok_or_else(invalid)?;
        let prepared = catalog.prepare(Change::Insert(entry))?;
        let metadata = self.shared.resources.metadata.clone();
        let index = self.endpoints.len();
        let mut staging = cas_core::space::Staging::reserve(
            &self.shared.staging,
            index,
            append::Limits::default().staging_bytes,
        )?;
        let confirmations = self
            .shared
            .fleet
            .as_ref()
            .unwrap()
            .state
            .lock()
            .unwrap()
            .confirmation_bytes(
                usize::try_from(meta.manifest_bytes / 64).map_err(|_| invalid())?,
            )?;
        // Reserve a conservative complete destination WAL bound, including
        // repacking and one fresh segment. Retained rejected outputs stay charged.
        let bound = append::Limits::default().staging_bytes
            + meta.manifest_bytes
            + confirmations
            + capacity::METADATA_MARGIN
            + prepared.output_bytes() as u64;
        let permit = self
            .shared
            .physical
            .as_ref()
            .map(|p| p.foreground(bound))
            .transpose()?;
        let output = || -> io::Result<(Log, Manifest)> {
            if directory.exists() {
                // This image has no catalog entry and its exact receiving root
                // was checked before import. Preserve interrupted output under
                // another unreferenced image name: Tickets scans these WAL names
                // on restart, so no rejected allocation can ever be reused.
                let mut attempt = 0u64;
                loop {
                    let mut h = blake3::Hasher::new();
                    h.update(b"CAS rejected import\0");
                    h.update(&meta.digest()?);
                    h.update(&attempt.to_le_bytes());
                    let id = u128::from_be_bytes(h.finalize().as_bytes()[..16].try_into().unwrap());
                    let rejected = directory.parent().unwrap().join(format!("{id:032x}"));
                    if !rejected.exists() {
                        fs::rename(&directory, rejected)?;
                        File::open(directory.parent().unwrap())?.sync_all()?;
                        break;
                    }
                    attempt = attempt.checked_add(1).ok_or_else(invalid)?;
                }
            }
            fs::create_dir(&directory)?;
            File::open(directory.parent().unwrap())?.sync_all()?;
            fs::copy(
                spool(self.store.tickets().root(), meta, 0),
                directory.join("manifest.v2"),
            )?;
            let runtime = self.shared.fleet.as_ref().unwrap();
            let placement = runtime.state.lock().unwrap().placement;
            let mut remote = reserved_vec(0, &self.shared.resources.compaction)?;
            let inspection = Manifest::inspect(
                &directory,
                cas_core::manifest::file::Identity {
                    store: meta.store,
                    image: meta.source,
                    image_bytes: meta.image_bytes,
                },
                meta.d,
                metadata.clone(),
                |hash| {
                    if self.shared.reader.plan(hash)?.is_some() {
                        return Ok(());
                    }
                    if placement.local(&hash) {
                        return Err(io::Error::other(
                            "destination owner lacks provisioned chunk",
                        ));
                    }
                    remote
                        .try_reserve(1)
                        .map_err(|_| io::ErrorKind::OutOfMemory)?;
                    remote.push(hash);
                    Ok(())
                },
            )?;
            if inspection.selected().commit.durable != meta.d
                || inspection.selected().end != meta.manifest_bytes
            {
                return Err(invalid());
            }
            remote.sort_unstable();
            remote.dedup();
            let mut peer = protocol::Client::member(
                self.shared.resources.peer.fleet.peer_bulk_address.unwrap(),
                meta.mode.flow(),
                runtime.metrics.clone(),
                runtime.state.lock().unwrap().placement,
                runtime.store,
            )?;
            let mut frame = [0; 63 * 32];
            let mut missing = [0; 8];
            for batch in remote.chunks(63) {
                protocol::chunks::has(&mut peer, batch, &mut frame, &mut missing)?;
                if (0..batch.len()).any(|i| missing[i / 8] & (1 << (i % 8)) != 0) {
                    return Err(io::Error::other(
                        "fleet is missing a provisioned dependency",
                    ));
                }
            }
            runtime.state.lock().unwrap().confirm(&remote)?;
            let manifest = inspection.recover()?;
            let manifest = if meta.mode == Mode::Provision {
                manifest.into_provisioned(meta.image)?
            } else {
                manifest
            };
            let mut log = Log::create_transferred(
                self.store.tickets().clone(),
                append::Config {
                    store: meta.store,
                    image: meta.image,
                    image_bytes: meta.image_bytes,
                    segment_bytes: meta.segment_bytes,
                },
                append::Limits::default(),
                metadata.clone(),
                manifest.view()?,
            )
            .map_err(io::Error::other)?;
            if meta.mode == Mode::Migration {
                let mut tail = File::open(spool(self.store.tickets().root(), meta, 1))?;
                log.import_transfer(&mut tail, meta.e)
                    .map_err(io::Error::other)?;
            } else {
                log.flush().map_err(io::Error::other)?;
            }
            File::open(&directory)?.sync_all()?;
            #[cfg(test)]
            protocol::crash_point("destination-durable");
            self.catalog.as_mut().unwrap().publish(prepared)?;
            #[cfg(test)]
            protocol::crash_point("destination-catalog");
            Ok((log, manifest))
        };
        staging.start();
        let (log, manifest) = match permit {
            Some(p) => p.run(output)?,
            None => output()?,
        };
        let allocated = log.status().allocated_bytes;
        staging.installed(allocated)?;
        let (attachment, endpoint) = make_image(
            &self.shared,
            ready,
            index,
            log,
            manifest,
            Some(recovery::Mode::Cold),
            None,
        )?;
        let images = self
            .attachments
            .upgrade()
            .ok_or(io::ErrorKind::BrokenPipe)?;
        images.lock().unwrap().push(Some(attachment));
        self.endpoints.push(endpoint);
        self.proof(meta)?;
        self.root(&meta.root(Phase::Active)?)?;
        #[cfg(test)]
        protocol::crash_point("destination-active");
        Ok(())
    }
}
