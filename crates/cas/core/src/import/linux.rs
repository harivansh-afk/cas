//! WAL batches, FLUSH, compaction into the shared chunk store and committed
//! manifests, then catalog publication. No guest, daemon or governor runs;
//! the caller owns the root lock for the whole operation.
pub use crate::layout::image_directory;
use crate::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    append::{
        self, Limits, Log,
        format::{Builder, MAX_BATCH_BYTES, MAX_DESCRIPTORS, RequestId},
    },
    budget::{Budget, Usage},
    catalog::{Catalog, Change, Entry, Kind},
    chunk::ChunkSize,
    encoding::require,
    id::hex,
    layout,
    manifest::file::Manifest,
    segments::Tickets,
    store::file::{self, Store},
};
use sha2::Digest;
use std::{
    fs::{self, File},
    io,
    os::unix::fs::{FileExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Instant, SystemTime},
};

/// Completed owner boundaries, exposed for process-crash controls and progress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Point {
    /// A compaction pass settled the WAL prefix below this many input bytes.
    Settled { bytes: u64 },
    /// Every mutation is published, durable and compacted (D == E == P); the
    /// catalog does not yet list the image.
    Durable,
    /// The catalog lists the image.
    Published,
}

pub struct Request<'a> {
    pub root: &'a Path,
    pub store: [u8; 16],
    pub image: [u8; 16],
    pub input: &'a Path,
    /// Required geometry for a new root; checked against an existing root.
    pub chunk_size: Option<ChunkSize>,
    /// WAL and chunk segment capacity for a new root, bounded by the host's
    /// default staging limits so the root stays servable; an existing root keeps its own.
    pub segment_bytes: u64,
    /// Input bytes staged between FLUSH/compaction/reclamation passes.
    pub compaction_interval: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct StoreSample {
    pub chunks: usize,
    pub segments: usize,
    pub encoded_bytes: u64,
    pub allocated_bytes: u64,
    pub store_bytes_appended: u64,
    pub index_memory: crate::chunk_index::Memory,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Receipt {
    pub schema_version: u32,
    pub operation: &'static str,
    pub root: PathBuf,
    pub input: PathBuf,
    pub store: String,
    pub image: String,
    pub created_root: bool,
    pub chunk_bytes: usize,
    pub segment_bytes: u64,
    pub compaction_interval: u64,
    pub image_bytes: u64,
    /// Digests of the bytes actually staged, read once in order; the input's
    /// length, inode, mtime and ctime are rechecked at the end.
    pub input_sha256: String,
    pub input_blake3: String,
    /// WRITE payload bytes; zero runs at chunk granularity become ZERO mutations.
    pub data_bytes: u64,
    pub zero_bytes: u64,
    pub mutations: u64,
    pub batches: u64,
    pub settle_passes: u64,
    pub compaction_transactions: u64,
    pub wal_removed_segments: usize,
    pub wal_punch_requested_bytes: u64,
    pub wal: append::Status,
    pub manifest_bytes_written: u64,
    pub manifest_generation: u64,
    pub manifest_end: u64,
    pub store_before: StoreSample,
    pub store_after: StoreSample,
    pub unique_chunks_appended: usize,
    pub unique_chunk_bytes_appended: u64,
    pub store_bytes_appended: u64,
    pub catalog_generation: u64,
    pub metadata_usage: Usage,
    pub elapsed_seconds: f64,
    pub throughput_mib_per_second: f64,
    pub scope: &'static str,
}

#[derive(Default)]
struct Counters {
    data_bytes: u64,
    zero_bytes: u64,
    mutations: u64,
    batches: u64,
    settle_passes: u64,
    compaction_transactions: u64,
    removed_segments: usize,
    punch_requested_bytes: u64,
}

pub fn run(
    request: Request<'_>,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
) -> io::Result<Receipt> {
    run_with(request, metadata, io_memory, |_| Ok(()))
}

/// `observe` runs at each [`Point`]; an error or process exit there leaves the
/// root for inspection exactly as a crash at that boundary would.
pub fn run_with(
    request: Request<'_>,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
    mut observe: impl FnMut(Point) -> io::Result<()>,
) -> io::Result<Receipt> {
    let started = Instant::now();
    let input = Input::open(request.input)?;
    let mut root = Root::open(&request, Arc::clone(&metadata), io_memory)?;
    let store_before = sample(&root.store)?;
    let mut image = Image::create(&request, &root, input.bytes, Arc::clone(&metadata))?;
    let (counters, digests) = ingest(&input, &mut image, &mut root, &request, &mut observe)?;
    image.require_settled(counters.mutations)?;
    input.require_unchanged()?;
    observe(Point::Durable)?;
    root.publish(request.image, input.bytes)?;
    observe(Point::Published)?;
    let store_after = sample(&root.store)?;
    Ok(receipt(
        &request,
        &root,
        &image,
        &counters,
        digests,
        store_before,
        store_after,
        metadata.usage(),
        started.elapsed().as_secs_f64(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn receipt(
    request: &Request<'_>,
    root: &Root,
    image: &Image,
    counters: &Counters,
    digests: Digests,
    store_before: StoreSample,
    store_after: StoreSample,
    metadata_usage: Usage,
    elapsed_seconds: f64,
) -> Receipt {
    let image_bytes = image.log.config().image_bytes;
    let unique_chunks_appended = store_after.chunks - store_before.chunks;
    Receipt {
        schema_version: 1,
        operation: "offline_import",
        root: request.root.to_owned(),
        input: request.input.to_owned(),
        store: hex(request.store),
        image: hex(request.image),
        created_root: root.created,
        chunk_bytes: root.size.bytes(),
        segment_bytes: root.config.segment_bytes,
        compaction_interval: request.compaction_interval,
        image_bytes,
        input_sha256: digests.sha256,
        input_blake3: digests.blake3,
        data_bytes: counters.data_bytes,
        zero_bytes: counters.zero_bytes,
        mutations: counters.mutations,
        batches: counters.batches,
        settle_passes: counters.settle_passes,
        compaction_transactions: counters.compaction_transactions,
        wal_removed_segments: counters.removed_segments,
        wal_punch_requested_bytes: counters.punch_requested_bytes,
        wal: image.log.status(),
        manifest_bytes_written: image.manifest.bytes_written(),
        manifest_generation: image.manifest.current().generation,
        manifest_end: image.manifest.end(),
        unique_chunks_appended,
        unique_chunk_bytes_appended: (unique_chunks_appended * root.size.bytes()) as u64,
        store_bytes_appended: store_after.store_bytes_appended - store_before.store_bytes_appended,
        store_before,
        store_after,
        catalog_generation: root.catalog.contents().generation(),
        metadata_usage,
        elapsed_seconds,
        throughput_mib_per_second: image_bytes as f64 / (1024.0 * 1024.0) / elapsed_seconds,
        scope: "offline single-threaded import through WAL, compaction and catalog; no guest, daemon, governor or device counters",
    }
}

/// A raw image of whole blocks; its observed identity is rechecked at the end.
struct Input {
    file: File,
    bytes: u64,
    identity: InputIdentity,
}

#[derive(PartialEq, Eq)]
struct InputIdentity {
    device: u64,
    inode: u64,
    modified: SystemTime,
    changed: (i64, i64),
}

impl InputIdentity {
    fn of(metadata: &fs::Metadata) -> io::Result<Self> {
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            modified: metadata.modified()?,
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

impl Input {
    fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let metadata = file.metadata()?;
        let bytes = metadata.len();
        require(
            metadata.is_file() && bytes != 0 && bytes.is_multiple_of(BLOCK_SIZE as u64),
            "raw input must be a nonempty regular file of whole 4 KiB blocks",
        )?;
        Ok(Self {
            file,
            bytes,
            identity: InputIdentity::of(&metadata)?,
        })
    }

    fn require_unchanged(&self) -> io::Result<()> {
        let metadata = self.file.metadata()?;
        require(
            metadata.len() == self.bytes && InputIdentity::of(&metadata)? == self.identity,
            "input changed during import",
        )
    }
}

struct Digests {
    sha256: String,
    blake3: String,
}

struct Root {
    tickets: Arc<Tickets>,
    store: Store,
    catalog: Catalog,
    config: file::Config,
    size: ChunkSize,
    limits: Limits,
    created: bool,
    metadata: Arc<Budget>,
    io_memory: Arc<Budget>,
}

impl Root {
    /// An existing empty directory becomes a new root; a root with recorded
    /// geometry is inspected and recovered like a cold host open, without
    /// opening other images. Every parameter is validated before any output.
    fn open(
        request: &Request<'_>,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Self> {
        require(
            request.compaction_interval != 0
                && request
                    .compaction_interval
                    .is_multiple_of(MAX_REQUEST_BYTES as u64),
            "compaction interval must be a nonzero multiple of 1 MiB",
        )?;
        require(
            request.root.symlink_metadata().is_ok_and(|m| m.is_dir()),
            "root must be an existing directory; an empty one becomes a new root",
        )?;
        let tickets = Tickets::open(request.root, Arc::clone(&metadata))?;
        if Store::exists(&tickets)? {
            return Self::inspect(request, tickets, metadata, io_memory);
        }
        validate_new_segment(request.store, request.segment_bytes)?;
        validate_interval(request.segment_bytes, request.compaction_interval)?;
        require(
            fs::read_dir(request.root)?.next().transpose()?.is_none(),
            "a new root must be an empty directory",
        )?;
        Self::create(request, tickets, metadata, io_memory)
    }

    fn inspect(
        request: &Request<'_>,
        tickets: Arc<Tickets>,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Self> {
        let (config, size) = Store::recorded_geometry(&tickets, Arc::clone(&metadata))?;
        require(
            config.store == request.store,
            "store identity differs from the root's recorded store",
        )?;
        require(
            request.chunk_size.is_none_or(|requested| requested == size),
            "requested chunk size differs from the root's fixed geometry",
        )?;
        validate_interval(config.segment_bytes, request.compaction_interval)?;
        let catalog = Catalog::inspect(Arc::clone(&tickets), config.store, Arc::clone(&metadata))?;
        if catalog.contents().get(request.image).is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "catalog already lists this image ID",
            ));
        }
        let store = Store::inspect(
            Arc::clone(&tickets),
            config,
            Arc::clone(&metadata),
            Arc::clone(&io_memory),
        )?
        .recover()?;
        Ok(Self {
            tickets,
            store,
            catalog: catalog.recover()?,
            config,
            size,
            limits: Limits::default(),
            created: false,
            metadata,
            io_memory,
        })
    }

    /// The catalog exists before the geometry marker, so a root that records
    /// geometry always has a catalog; an interrupted creation leaves a
    /// non-empty directory without geometry, which is refused above.
    fn create(
        request: &Request<'_>,
        tickets: Arc<Tickets>,
        metadata: Arc<Budget>,
        io_memory: Arc<Budget>,
    ) -> io::Result<Self> {
        let size = request.chunk_size.unwrap_or_default();
        let config = file::Config {
            store: request.store,
            segment_bytes: request.segment_bytes,
        };
        let catalog = Catalog::create(Arc::clone(&tickets), config.store, Arc::clone(&metadata))?;
        let store = layout::create_store(
            Arc::clone(&tickets),
            config,
            size,
            Arc::clone(&metadata),
            Arc::clone(&io_memory),
        )?;
        Ok(Self {
            tickets,
            store,
            catalog,
            config,
            size,
            limits: Limits::default(),
            created: true,
            metadata,
            io_memory,
        })
    }

    /// Every referenced chunk and the manifest are already durable.
    fn publish(&mut self, image: [u8; 16], image_bytes: u64) -> io::Result<()> {
        let prepared = self.catalog.prepare(Change::Insert(Entry {
            id: image,
            kind: Kind::Image { image_bytes },
        }))?;
        self.catalog.publish(prepared)
    }
}

/// A new root must stay servable by `cas-host`, which opens WALs with the
/// default limits: the segment must fit them and leave room to roll over,
/// the same rule host initialization applies.
fn validate_new_segment(store: [u8; 16], segment_bytes: u64) -> io::Result<()> {
    file::Config {
        store,
        segment_bytes,
    }
    .validate()?;
    append::Config {
        store,
        image: [1; 16],
        image_bytes: BLOCK_SIZE as u64,
        segment_bytes,
    }
    .validate(Limits::default())
    .map_err(io::Error::other)?;
    require(
        u128::from(segment_bytes) * 100 < u128::from(Limits::default().staging_bytes) * 60,
        "segment size prevents WAL rollover under the host's default staging limit",
    )
}

/// Worst-case preallocated WAL bytes between two settle passes must fit the
/// default staging limit: the current segment plus every segment a full
/// interval of batches can open, counting one extra batch header per
/// possible descriptor split in each window. The WAL index must also hold
/// one mapping per block of the interval plus two batches of descriptors.
fn validate_interval(segment_bytes: u64, interval: u64) -> io::Result<()> {
    let limits = Limits::default();
    let splits = (interval / MAX_REQUEST_BYTES as u64) * 5 * BLOCK_SIZE as u64;
    let capacity = segment_bytes - (2 * BLOCK_SIZE + MAX_BATCH_BYTES) as u64;
    let bound = segment_bytes * (1 + (interval + splits).div_ceil(capacity));
    require(
        bound <= limits.staging_bytes
            && (interval / BLOCK_SIZE as u64) as usize + 2 * MAX_DESCRIPTORS <= limits.intervals,
        "compaction interval exceeds staging capacity; lower it or the segment size",
    )
}

struct Image {
    manifest: Manifest,
    log: Log,
}

impl Image {
    fn create(
        request: &Request<'_>,
        root: &Root,
        image_bytes: u64,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        let directory = image_directory(request.root, request.image);
        if directory.symlink_metadata().is_ok() {
            retain_rejected(&directory, request.image)?;
        }
        let (log, manifest) = layout::create_image(
            Arc::clone(&root.tickets),
            append::Config {
                store: request.store,
                image: request.image,
                image_bytes,
                segment_bytes: root.config.segment_bytes,
            },
            root.size,
            root.limits,
            metadata,
        )?;
        Ok(Self { manifest, log })
    }

    fn require_settled(&self, mutations: u64) -> io::Result<()> {
        let wal = self.log.status();
        require(
            wal.published == mutations
                && wal.durable == mutations
                && wal.compacted == mutations
                && self.manifest.current().durable == mutations,
            "import did not settle to D == E == P",
        )
    }

    /// FLUSH, compact every durable batch into the store and manifest, then
    /// reclaim covered WAL payload: the same sequence the host compactor runs.
    fn settle(&mut self, root: &mut Root, counters: &mut Counters) -> io::Result<()> {
        self.log.flush().map_err(io::Error::other)?;
        while let Some(selection) = self
            .log
            .select_compaction(Arc::clone(&root.metadata), Arc::clone(&root.io_memory))
            .map_err(io::Error::other)?
        {
            let input = selection.load_with_store(&root.store.reader()?)?;
            let compacted = input.write(&mut root.store, &mut self.manifest)?;
            self.log
                .publish_compaction(compacted)
                .map_err(io::Error::other)?;
            counters.compaction_transactions += 1;
        }
        let reclaimed = self
            .log
            .select_reclamation(None, Arc::clone(&root.metadata))
            .map_err(io::Error::other)?
            .run()?;
        let stats = self
            .log
            .apply_reclamation(reclaimed)
            .map_err(io::Error::other)?;
        counters.removed_segments += stats.removed_segments;
        counters.punch_requested_bytes += stats.punch_requested_bytes;
        counters.settle_passes += 1;
        Ok(())
    }
}

/// An interrupted import has no catalog entry (checked before this). Keep its
/// output under another unreferenced image name, as the fleet importer does:
/// `Tickets` scans those WAL names on every open, so no ticket is reused.
fn retain_rejected(directory: &Path, image: [u8; 16]) -> io::Result<()> {
    let parent = directory
        .parent()
        .ok_or_else(|| io::Error::other("image directory has no parent"))?;
    let mut attempt = 0u64;
    loop {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"CAS rejected import\0");
        hasher.update(&image);
        hasher.update(&attempt.to_le_bytes());
        let id: [u8; 16] = hasher.finalize().as_bytes()[..16].try_into().unwrap();
        let rejected = parent.join(hex(id));
        if rejected.symlink_metadata().is_err() {
            fs::rename(directory, rejected)?;
            return File::open(parent)?.sync_all();
        }
        attempt = attempt
            .checked_add(1)
            .ok_or_else(|| io::Error::other("rejected import names exhausted"))?;
    }
}

/// Stream the input in 1 MiB windows, settling every compaction interval and
/// once more at the end, so the WAL never exceeds its staging capacity.
fn ingest(
    input: &Input,
    image: &mut Image,
    root: &mut Root,
    request: &Request<'_>,
    observe: &mut impl FnMut(Point) -> io::Result<()>,
) -> io::Result<(Counters, Digests)> {
    let mut counters = Counters::default();
    let mut sha256 = sha2::Sha256::new();
    let mut blake3 = blake3::Hasher::new();
    let mut window = vec![0u8; MAX_REQUEST_BYTES];
    let mut offset = 0;
    let mut staged = 0;
    while offset < input.bytes {
        let length = (input.bytes - offset).min(MAX_REQUEST_BYTES as u64) as usize;
        input.file.read_exact_at(&mut window[..length], offset)?;
        sha256.update(&window[..length]);
        blake3.update(&window[..length]);
        stage(
            &mut image.log,
            &window[..length],
            offset,
            root.size,
            &mut counters,
        )?;
        offset += length as u64;
        staged += length as u64;
        if staged >= request.compaction_interval {
            image.settle(root, &mut counters)?;
            observe(Point::Settled { bytes: offset })?;
            staged = 0;
        }
    }
    if staged != 0 {
        image.settle(root, &mut counters)?;
        observe(Point::Settled { bytes: offset })?;
    }
    let digests = Digests {
        sha256: format!("{:x}", sha256.finalize()),
        blake3: blake3.finalize().to_hex().to_string(),
    };
    Ok((counters, digests))
}

/// A maximal run of all-zero or data units of the chunk size within a window.
struct Run {
    start: usize,
    length: usize,
    zero: bool,
}

fn runs(window: &[u8], size: ChunkSize) -> Vec<Run> {
    let mut runs: Vec<Run> = Vec::new();
    for (index, unit) in window.chunks(size.bytes()).enumerate() {
        let zero = unit.iter().all(|byte| *byte == 0);
        match runs.last_mut() {
            Some(last) if last.zero == zero => last.length += unit.len(),
            _ => runs.push(Run {
                start: index * size.bytes(),
                length: unit.len(),
                zero,
            }),
        }
    }
    runs
}

/// Append one window as ZERO and WRITE mutations, at most 63 per batch.
fn stage(
    log: &mut Log,
    window: &[u8],
    offset: u64,
    size: ChunkSize,
    counters: &mut Counters,
) -> io::Result<()> {
    let image_bytes = log.config().image_bytes;
    let runs = runs(window, size);
    let mut remaining: usize = runs
        .iter()
        .filter(|run| !run.zero)
        .map(|run| run.length)
        .sum();
    let mut builder = Builder::new(image_bytes, remaining).map_err(io::Error::other)?;
    for run in &runs {
        if builder.len() == MAX_DESCRIPTORS {
            append(log, builder, counters)?;
            builder = Builder::new(image_bytes, remaining).map_err(io::Error::other)?;
        }
        counters.mutations += 1;
        let id = RequestId {
            attachment: 1,
            serial: counters.mutations,
            queue: 0,
            head: 0,
        };
        let logical = offset + run.start as u64;
        if run.zero {
            builder
                .zero(id, logical, run.length as u64)
                .map_err(io::Error::other)?;
            counters.zero_bytes += run.length as u64;
            continue;
        }
        let payload = &window[run.start..run.start + run.length];
        builder
            .write(id, logical, run.length, |bytes| {
                bytes.copy_from_slice(payload);
                Ok(())
            })
            .map_err(io::Error::other)?;
        counters.data_bytes += run.length as u64;
        remaining -= run.length;
    }
    if !builder.is_empty() {
        append(log, builder, counters)?;
    }
    Ok(())
}

fn append(log: &mut Log, builder: Builder, counters: &mut Counters) -> io::Result<()> {
    log.append(builder).map_err(io::Error::other)?;
    counters.batches += 1;
    Ok(())
}

fn sample(store: &Store) -> io::Result<StoreSample> {
    let status = store.status();
    Ok(StoreSample {
        chunks: status.chunks,
        segments: status.segments,
        encoded_bytes: status.encoded_bytes,
        allocated_bytes: allocated(&store.tickets().root().join("chunks"))?,
        store_bytes_appended: status.store_bytes_appended,
        index_memory: status.index_memory,
    })
}

fn allocated(path: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        total += if metadata.is_dir() {
            allocated(&entry.path())?
        } else {
            metadata.blocks() * 512
        };
    }
    Ok(total)
}
