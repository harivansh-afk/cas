//! The storage root layout shared by host initialization and offline import:
//! one creation recipe for the store namespace and for an image's manifest
//! and WAL, with the directory syncs that make each step durable in order.
use crate::{
    append::{self, Limits, Log},
    budget::Budget,
    chunk::ChunkSize,
    manifest::file::{Identity, Manifest},
    segments::Tickets,
    store::file::{self, Store},
};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

pub const IMAGES: &str = "images";
pub const SNAPSHOTS: &str = "snapshots";

pub fn image_directory(root: &Path, image: [u8; 16]) -> PathBuf {
    root.join(IMAGES).join(crate::id::hex(image))
}

/// Create the chunk store with its durable geometry, then the image and
/// snapshot namespaces, syncing the root before returning. The caller has
/// validated capacity and holds the root through `tickets`.
pub fn create_store(
    tickets: Arc<Tickets>,
    config: file::Config,
    chunk_size: ChunkSize,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
) -> io::Result<Store> {
    let root = tickets.root().to_owned();
    let store = Store::create_sized(tickets, config, chunk_size, metadata, io_memory)?;
    fs::create_dir(root.join(IMAGES))?;
    fs::create_dir(root.join(SNAPSHOTS))?;
    File::open(&root)?.sync_all()?;
    Ok(store)
}

/// Create an image directory, sync its parent, then its manifest and a fresh
/// shared WAL whose first segment and recovery base are durable on return.
/// Catalog publication of the image follows separately. A failure leaves
/// any created output in place for inspection.
pub fn create_image(
    tickets: Arc<Tickets>,
    config: append::Config,
    chunk_size: ChunkSize,
    limits: Limits,
    metadata: Arc<Budget>,
) -> io::Result<(Log, Manifest)> {
    let directory = image_directory(tickets.root(), config.image);
    fs::create_dir(&directory)?;
    File::open(tickets.root().join(IMAGES))?.sync_all()?;
    let manifest = Manifest::create_sized(
        &directory,
        Identity {
            store: config.store,
            image: config.image,
            image_bytes: config.image_bytes,
        },
        chunk_size,
        Arc::clone(&metadata),
    )?;
    let log = Log::create_shared(tickets, config, limits, metadata, manifest.view()?)
        .map_err(io::Error::other)?;
    Ok((log, manifest))
}
