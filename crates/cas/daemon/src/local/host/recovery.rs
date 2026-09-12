//! One read-only dependency check; no individual repair handle escapes.
use super::*;
use cas_core::{catalog, catalog::Catalog, manifest::file, segments::Tickets};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
mod live;
pub use live::{Prepared, Replay, Retained};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Cold,
    Retained,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prefix {
    pub image: catalog::Id,
    pub published: u64,
}

pub enum Prefixes<'a> {
    Cold,
    /// Exactly one entry for every catalog image, in canonical ID order.
    Retained(&'a [Prefix]),
}

pub struct Config {
    pub store: cas_core::store::file::Config,
    pub append: append::Limits,
}

pub struct Image<L = append::SharedRecovery> {
    pub(super) manifest: file::Inspection,
    pub(super) log: L,
    pub(super) required: Prefix,
}

/// A complete, locked dependency graph, still without serving or repair access.
pub struct Inspection<L = append::SharedRecovery> {
    segment_bytes: u64,
    pub(super) resources: Arc<Resources>,
    pub(super) tickets: Arc<Tickets>,
    pub(super) catalog: catalog::Inspection,
    pub(super) store: cas_core::store::file::Inspection,
    pub(super) images: BudgetVec<Image<L>, BudgetAllocator>,
    pub(super) snapshots: BudgetVec<(catalog::Id, file::SnapshotInspection), BudgetAllocator>,
}

impl Inspection {
    pub fn open(
        root: &Path,
        config: Config,
        resources: Arc<Resources>,
        prefixes: Prefixes<'_>,
    ) -> io::Result<Checked> {
        Self::scan(root, config, resources)?.require(prefixes)
    }

    /// Locks and inspects storage before frontend carrier negotiation supplies P.
    pub fn scan(root: &Path, config: Config, resources: Arc<Resources>) -> io::Result<Self> {
        let tickets = Tickets::open(root, Arc::clone(&resources.metadata))?;
        let device = tickets.device()?;
        let catalog = Catalog::inspect(
            Arc::clone(&tickets),
            config.store.store,
            Arc::clone(&resources.metadata),
        )?;
        let store = Store::inspect(
            Arc::clone(&tickets),
            config.store,
            Arc::clone(&resources.metadata),
            resources.read_memory(),
        )?;
        let image_count = catalog
            .contents()
            .entries()
            .filter(|entry| matches!(entry.kind, catalog::Kind::Image { .. }))
            .count();
        let mut images = reserved_vec(image_count, &resources.metadata)?;
        let mut snapshots =
            reserved_vec(catalog.contents().len() - image_count, &resources.metadata)?;
        let verify = |hash| {
            if store.contains(&hash) {
                Ok(())
            } else {
                Err(io::Error::other(
                    "catalog dependency references an unavailable chunk",
                ))
            }
        };
        for entry in catalog.contents().entries() {
            let directory = path(root, entry);
            if std::fs::metadata(&directory)?.dev() != device {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "catalog member lies on another filesystem",
                ));
            }
            match entry.kind {
                catalog::Kind::Image { image_bytes } => {
                    let manifest = Manifest::inspect(
                        &directory,
                        file::Identity {
                            store: config.store.store,
                            image: entry.id,
                            image_bytes,
                        },
                        0,
                        Arc::clone(&resources.metadata),
                        verify,
                    )?;
                    let log = Log::inspect_shared(
                        Arc::clone(&tickets),
                        &manifest,
                        config.append,
                        Arc::clone(&resources.metadata),
                    )
                    .map_err(io::Error::other)?;
                    if log.config().segment_bytes != config.store.segment_bytes {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidInput,
                            "WAL and store segment geometry differ",
                        ));
                    }
                    images.push(Image {
                        manifest,
                        log,
                        required: Prefix {
                            image: entry.id,
                            published: 0,
                        },
                    });
                }
                catalog::Kind::Snapshot(key) => snapshots.push((
                    entry.id,
                    Snapshot::inspect(&directory, key, Arc::clone(&resources.metadata), verify)?,
                )),
            }
        }
        Ok(Self {
            segment_bytes: config.store.segment_bytes,
            resources,
            tickets,
            catalog,
            store,
            images,
            snapshots,
        })
    }

    pub fn require(mut self, prefixes: Prefixes<'_>) -> io::Result<Checked> {
        if let Prefixes::Retained(required) = &prefixes
            && (required.len() != self.images.len()
                || required
                    .windows(2)
                    .any(|pair| pair[0].image >= pair[1].image))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retained prefixes must cover every image in canonical order",
            ));
        }
        for (index, image) in self.images.iter_mut().enumerate() {
            if let Prefixes::Retained(required) = &prefixes {
                if required[index].image != image.required.image {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "retained prefix image differs from catalog",
                    ));
                }
                image.required = required[index];
            }
            image
                .log
                .require_prefix(image.required.published)
                .map_err(io::Error::other)?;
        }
        Ok(Checked {
            inspected: self,
            cold: matches!(prefixes, Prefixes::Cold),
        })
    }

    pub fn report(&self) -> serde_json::Value {
        let images: Vec<_> = self
            .images
            .iter()
            .map(|image| {
                let selected = image.manifest.selected();
                serde_json::json!({ "id":image.required.image, "required_p":null,
                "wal":image.log.status(), "manifest_d":selected.commit.durable,
                "manifest_end":selected.end, "manifest_file_bytes":selected.file_bytes })
            })
            .collect();
        let tickets = self.tickets.status();
        serde_json::json!({ "catalog_generation":self.catalog.contents().generation(),
            "store":self.store.status(), "tickets":{"highest":tickets.highest,"failed":tickets.failed}, "images":images,
            "snapshots":self.snapshots.len(), "metadata":self.resources.metadata.usage(),
            "read_memory":self.resources.read_memory().usage(), "prefixes_checked":false, "repaired":false })
    }

    pub fn contents(&self) -> &catalog::Contents {
        self.catalog.contents()
    }

    pub fn observation(&self) -> io::Result<cas_core::space::Observation> {
        cas_core::space::Observation::inspect(&self.tickets)
    }

    pub fn images(&self) -> impl ExactSizeIterator<Item = (catalog::Id, append::Status)> + '_ {
        self.images
            .iter()
            .map(|image| (image.required.image, image.log.status()))
    }
}

/// Every catalog dependency and explicit required P has passed inspection.
pub struct Checked {
    inspected: Inspection,
    cold: bool,
}

impl Checked {
    pub fn recover_cold(self, limits: cas_core::space::Limits) -> io::Result<Recovered> {
        if !self.cold {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "retained recovery cannot use cold stabilization",
            ));
        }
        stabilize(
            self.inspected,
            limits,
            Mode::Cold,
            |log, repair| log.validate_recovery(repair).map_err(io::Error::other),
            |log, manifest, required, repair| {
                log.fresh_with(manifest.view()?, required.published, repair)
                    .map_err(io::Error::other)
            },
        )
    }

    pub fn contents(&self) -> &catalog::Contents {
        self.inspected.contents()
    }

    pub fn images(&self) -> impl ExactSizeIterator<Item = (Prefix, append::Status)> + '_ {
        self.inspected
            .images
            .iter()
            .map(|image| (image.required, image.log.status()))
    }

    pub fn report(&self) -> serde_json::Value {
        let mut report = self.inspected.report();
        report["prefixes_checked"] = true.into();
        report["cold"] = self.cold.into();
        for (index, image) in self.inspected.images.iter().enumerate() {
            report["images"][index]["required_p"] = image.required.published.into();
        }
        report
    }
}

/// Stabilized membership; no reader escapes before the whole graph is durable.
pub struct Recovered {
    mode: Mode,
    resources: Arc<Resources>,
    physical: Arc<cas_core::space::Governor>,
    catalog: Catalog,
    store: Store,
    images: BudgetVec<(Log, Manifest), BudgetAllocator>,
    snapshots: BudgetVec<Snapshot, BudgetAllocator>,
}

impl Recovered {
    pub fn contents(&self) -> &catalog::Contents {
        self.catalog.contents()
    }

    pub fn into_host(self, staging_bytes: u64) -> io::Result<Host> {
        Host::build_owned(
            self.resources,
            self.store,
            Roots {
                images: self.images,
                snapshots: self.snapshots,
            },
            staging_bytes,
            Some(self.physical),
            Context {
                catalog: Some(self.catalog),
                mode: Some(self.mode),
            },
        )
    }
}

fn stabilize<L>(
    mut inspected: Inspection<L>,
    limits: cas_core::space::Limits,
    mode: Mode,
    validate: impl Fn(&L, cas_core::space::Recovery<'_>) -> io::Result<()>,
    mut recover: impl FnMut(L, &Manifest, Prefix, cas_core::space::Recovery<'_>) -> io::Result<Log>,
) -> io::Result<Recovered> {
    let physical = cas_core::space::Governor::open(Arc::clone(&inspected.tickets), limits)?;
    capacity::validate_geometry(inspected.segment_bytes, &inspected.tickets, &physical)?;
    let repair = cas_core::space::Recovery::governed(&physical);
    inspected.store.validate_recovery(repair)?;
    inspected.catalog.validate_recovery(repair)?;
    for image in &inspected.images {
        image.manifest.validate_recovery(repair)?;
        validate(&image.log, repair)?;
    }
    for (_, snapshot) in &inspected.snapshots {
        snapshot.validate_recovery(repair)?;
    }
    let mut images = reserved_vec(inspected.images.len(), &inspected.resources.metadata)?;
    let mut snapshots = reserved_vec(inspected.snapshots.len(), &inspected.resources.metadata)?;
    // All roots and output bounds have passed before the first repair.
    let store = inspected.store.recover_with(repair)?;
    for image in inspected.images {
        let manifest = image.manifest.recover_with(repair)?;
        let log = recover(image.log, &manifest, image.required, repair)?;
        images.push((log, manifest));
    }
    for (_, snapshot) in inspected.snapshots {
        snapshots.push(repair.output(0, || snapshot.recover())?);
    }
    let catalog = repair.output(0, || inspected.catalog.recover())?;
    Ok(Recovered {
        mode,
        resources: inspected.resources,
        physical,
        catalog,
        store,
        images,
        snapshots,
    })
}

fn path(root: &Path, entry: catalog::Entry) -> PathBuf {
    let group = match entry.kind {
        catalog::Kind::Image { .. } => "images",
        catalog::Kind::Snapshot(_) => "snapshots",
    };
    let id: String = entry.id.iter().map(|byte| format!("{byte:02x}")).collect();
    root.join(group).join(id)
}
