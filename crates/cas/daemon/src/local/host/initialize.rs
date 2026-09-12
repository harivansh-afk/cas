//! Prepare complete initial membership before creating its durable dependencies.
use super::{Resources, capacity, reserved_vec};
use crate::deadline::{Deadline, RECOVERY_TIMEOUT};
use cas_core::{
    BLOCK_SIZE,
    append::{self, Log},
    catalog::{self, Entry, Initial, Kind},
    manifest::file::{Identity, Manifest},
    segments::Tickets,
    space::{self, Governor, Observation},
    store::file::{self, Store},
};
use std::{fs, fs::File, io, path::PathBuf, sync::Arc};

#[derive(Clone, Copy)]
pub struct Image {
    pub id: catalog::Id,
    pub bytes: u64,
}
impl std::str::FromStr for Image {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (id, bytes) = value
            .split_once('=')
            .ok_or("expected image-id=size-in-bytes")?;
        Ok(Self {
            id: crate::host_service::parse_id(id)?,
            bytes: bytes.parse().map_err(|_| "invalid image size in bytes")?,
        })
    }
}

pub struct Config {
    pub root: PathBuf,
    pub store: file::Config,
    pub append: append::Limits,
    pub staging_bytes: u64,
    pub images: Vec<Image>,
}

/// Bounded waiting never transfers running IO or its root lock to another owner.
pub fn create(config: Config) -> io::Result<serde_json::Value> {
    let deadline = Deadline::after(RECOVERY_TIMEOUT);
    let resources = Arc::new(Resources::default());
    let report_resources = Arc::clone(&resources);
    let result = deadline.run(move || initialize(config, resources, deadline));
    // Build diagnostic JSON only after the operation, outside storage admission.
    match result {
        Ok(mut report) => {
            report["metadata_after_drop"] =
                serde_json::to_value(report_resources.metadata.usage())?;
            Ok(report)
        }
        Err(error) => Err(error),
    }
}

pub(super) fn initialize(
    mut config: Config,
    resources: Arc<Resources>,
    deadline: Deadline,
) -> io::Result<serde_json::Value> {
    config.store.validate()?;
    config.images.sort_unstable_by_key(|image| image.id);
    if config.images.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "initial catalog needs images",
        ));
    }
    let initial = Initial::prepare(
        config.store.store,
        config.images.iter().map(|image| Entry {
            id: image.id,
            kind: Kind::Image {
                image_bytes: image.bytes,
            },
        }),
        Arc::clone(&resources.metadata),
    )?;
    for image in &config.images {
        append_config(&config.store, *image)
            .validate(config.append)
            .map_err(io::Error::other)?;
    }
    let count = config.images.len();
    let segment = config.store.segment_bytes;
    if u128::from(segment) * 100 >= u128::from(config.append.staging_bytes) * 60
        || u128::from(segment) * count as u128 * 100 >= u128::from(config.staging_bytes) * 60
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "initial WALs prevent staging resumption",
        ));
    }
    let output = segment
        .checked_add((2 * BLOCK_SIZE) as u64)
        .and_then(|bytes| bytes.checked_mul(count as u64))
        .and_then(|bytes| bytes.checked_add(initial.output_bytes() as u64))
        .and_then(|bytes| bytes.checked_add(space::METADATA_MARGIN))
        .ok_or_else(|| io::Error::other("initial allocation bound overflow"))?;
    // The encoded catalog becomes the sole image table before the worker writes.
    drop(config.images);
    let mut images = reserved_vec(count, &resources.metadata)?;
    let tickets = Tickets::open(&config.root, Arc::clone(&resources.metadata))?;
    if fs::read_dir(&config.root)?.next().transpose()?.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "initial storage root is not empty",
        ));
    }
    let observation = Observation::inspect(&tickets)?;
    let limits = space::Limits::new(
        observation.capacity(),
        segment,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )?;
    let physical = Governor::open(Arc::clone(&tickets), limits)?;
    capacity::validate_geometry(segment, &tickets, &physical)?;
    let permit = physical.foreground(output)?;
    let (store, catalog) = permit.run(|| {
        deadline.check()?;
        let store = Store::create(
            Arc::clone(&tickets),
            config.store,
            Arc::clone(&resources.metadata),
            resources.read_memory(),
        )?;
        fs::create_dir(config.root.join("images"))?;
        fs::create_dir(config.root.join("snapshots"))?;
        File::open(&config.root)?.sync_all()?;
        for entry in initial.contents().entries() {
            deadline.check()?;
            let Kind::Image { image_bytes } = entry.kind else {
                unreachable!("initial images only")
            };
            let directory = super::recovery::path(&config.root, entry);
            fs::create_dir(&directory)?;
            File::open(config.root.join("images"))?.sync_all()?;
            let manifest = Manifest::create(
                &directory,
                Identity {
                    store: config.store.store,
                    image: entry.id,
                    image_bytes,
                },
                Arc::clone(&resources.metadata),
            )?;
            let log = Log::create_shared(
                Arc::clone(&tickets),
                append_config(
                    &config.store,
                    Image {
                        id: entry.id,
                        bytes: image_bytes,
                    },
                ),
                config.append,
                Arc::clone(&resources.metadata),
                manifest.view()?,
            )
            .map_err(io::Error::other)?;
            images.push((log, manifest));
        }
        deadline.check()?;
        let catalog = initial.publish(Arc::clone(&tickets))?;
        Ok((store, catalog))
    })?;
    let report = serde_json::json!({
        "schema_version": 1, "operation": "host_init", "success": true,
        "store": config.store.store, "catalog_generation": catalog.contents().generation(),
        "images": count, "segment_bytes": segment, "reserved_output_bytes": output,
        "physical": physical.status(), "limits": limits,
        "metadata": resources.metadata.usage(),
    });
    drop((images, catalog, store, tickets, physical));
    Ok(report)
}

fn append_config(store: &file::Config, image: Image) -> append::Config {
    append::Config {
        store: store.store,
        image: image.id,
        image_bytes: image.bytes,
        segment_bytes: store.segment_bytes,
    }
}
