//! Publish an exact immutable root while the shared worker owns quiescence.
use super::*;
use cas_core::catalog::{Change, Entry, Id, Kind};
use std::{fs, fs::File, os::unix::fs::MetadataExt};

pub type SnapshotHandle = administration::Handle<SnapshotReport>;

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct SnapshotReport {
    pub snapshot: Id,
    pub image: Id,
    pub cut: u64,
    pub manifest_generation: u64,
    pub manifest_end: u64,
    pub catalog_generation: u64,
    pub pause_micros: u64,
}

pub(super) struct Request {
    pub image: Id,
    pub snapshot: Id,
    pub done: administration::Request<SnapshotReport>,
}

impl Host {
    pub fn snapshot(&self, image: Id, snapshot: Id) -> io::Result<SnapshotHandle> {
        let (done, handle) = administration::Request::new(&self.shared)?;
        self.ready
            .as_ref()
            .ok_or_else(|| io::Error::other("host is shutting down"))?
            .try_send(Ready::Snapshot(Request {
                image,
                snapshot,
                done,
            }))
            .map_err(|_| io::Error::other("snapshot queue unavailable"))?;
        Ok(handle)
    }
}

impl worker::Owner {
    pub fn snapshot(&mut self, image: Id, snapshot: Id) -> io::Result<SnapshotReport> {
        let catalog = self
            .catalog
            .as_ref()
            .ok_or_else(|| io::Error::other("host has no catalog owner"))?;
        if snapshot == [0; 16] || catalog.contents().get(snapshot).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "snapshot needs a fresh nonzero catalog ID",
            ));
        }
        if !catalog
            .contents()
            .get(image)
            .is_some_and(|entry| matches!(entry.kind, Kind::Image { .. }))
        {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "snapshot image is absent from catalog",
            ));
        }
        let index = self
            .endpoints
            .iter()
            .position(|endpoint| endpoint.manifest.current().image == image)
            .ok_or_else(|| io::Error::other("catalog image has no runtime owner"))?;
        if !self.shared.admission.attached(index) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "snapshot image reactor is not attached",
            ));
        }
        self.endpoints[index].healthy()?;
        let directory = recovery::path(
            self.store.tickets().root(),
            Entry {
                id: snapshot,
                kind: Kind::Snapshot(self.endpoints[index].manifest.view()?.key()),
            },
        );
        let parent = directory.parent().expect("canonical snapshot directory");
        match parent.symlink_metadata() {
            Ok(metadata)
                if metadata.is_dir() && metadata.dev() == self.store.tickets().device()? => {}
            Ok(_) => {
                return Err(io::Error::other(
                    "snapshot namespace is outside the storage domain",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
        }
        match directory.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "snapshot output already exists",
                ));
            }
        }
        let started = Instant::now();
        let mut report = self.quiesced(|owner, generation, started| {
            let endpoint = &mut owner.endpoints[index];
            if endpoint.quiescent != Some(generation) || !owner.shared.admission.attached(index) {
                return Err(io::Error::other(
                    "snapshot target lost its quiescent reactor",
                ));
            }
            let cut = endpoint.health.lock()?.durable;
            while endpoint.manifest.current().durable < cut {
                check_deadline(started)?;
                let before = endpoint.manifest.current().durable;
                endpoint.compact(
                    &mut owner.store,
                    owner.shared.physical.as_ref(),
                    Some(generation),
                )?;
                if endpoint.manifest.current().durable == before {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "snapshot compaction could not reach its cut",
                    ));
                }
            }
            let view = endpoint.manifest.view()?;
            if view.commit().durable != cut {
                return Err(io::Error::other(
                    "snapshot manifest differs from its fenced cut",
                ));
            }
            let catalog = owner.catalog.as_mut().expect("checked catalog owner");
            let prepared = catalog.prepare(Change::Insert(Entry {
                id: snapshot,
                kind: Kind::Snapshot(view.key()),
            }))?;
            owner
                .snapshots
                .try_reserve(1)
                .map_err(|_| io::ErrorKind::OutOfMemory)?;
            let bytes = view
                .end()
                .checked_add(prepared.output_bytes() as u64)
                .and_then(|bytes| bytes.checked_add(capacity::METADATA_MARGIN))
                .ok_or_else(|| io::Error::other("snapshot output bound overflow"))?;
            let permit = owner
                .shared
                .physical
                .as_ref()
                .ok_or_else(|| io::Error::other("catalog snapshot requires physical governor"))?
                .foreground(bytes)?;
            #[cfg(test)]
            {
                let pause = owner.shared.control.lock().unwrap().snapshot.take();
                if let Some(pause) = pause {
                    pause.wait();
                }
            }
            check_deadline(started)?;
            let retained = permit.run(|| {
                let parent = directory.parent().expect("canonical snapshot directory");
                match fs::create_dir(parent) {
                    Ok(()) => File::open(owner.store.tickets().root())?.sync_all()?,
                    Err(error)
                        if error.kind() == io::ErrorKind::AlreadyExists && parent.is_dir() => {}
                    Err(error) => return Err(error),
                }
                fs::create_dir(&directory)?;
                File::open(parent)?.sync_all()?;
                let retained = Snapshot::create(
                    &view,
                    &directory,
                    Arc::clone(&owner.shared.resources.metadata),
                )?;
                check_deadline(started)?;
                catalog.publish(prepared)?;
                Ok(retained)
            })?;
            owner.snapshots.push(retained);
            check_deadline(started)?;
            Ok(SnapshotReport {
                snapshot,
                image,
                cut,
                manifest_generation: view.commit().generation,
                manifest_end: view.end(),
                catalog_generation: catalog.contents().generation(),
                pause_micros: 0,
            })
        })?;
        report.pause_micros = started.elapsed().as_micros().try_into().unwrap_or(u64::MAX);
        Ok(report)
    }
}

fn check_deadline(started: Instant) -> io::Result<()> {
    if started.elapsed() >= IO_DEADLINE {
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "snapshot deadline expired",
        ))
    } else {
        Ok(())
    }
}
