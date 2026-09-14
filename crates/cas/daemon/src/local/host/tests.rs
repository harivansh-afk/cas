use super::*;
use cas_core::{
    aligned::AlignedBuffer, manifest::file::Identity, segments::Tickets, store::file::Config,
};
use std::{fs, path::Path};
mod admission;
mod allocation;
mod cache;
mod collection;
mod initialization;
mod quiescence;
mod recovery;
mod rotation;
mod scheduling;
mod snapshots;

const STORE: Config = Config {
    store: [1; 16],
    segment_bytes: 128 * BLOCK_SIZE as u64,
};
const IMAGE_BYTES: u64 = 64 * BLOCK_SIZE as u64;

fn runtime_store() -> Config {
    Config {
        segment_bytes: if std::env::var_os("CAS_SPACE_REPORT").is_some() {
            2 * MAX_REQUEST_BYTES as u64
        } else {
            STORE.segment_bytes
        },
        ..STORE
    }
}

fn start(resources: Arc<Resources>, store: Store, images: Vec<(Log, Manifest)>) -> Host {
    if std::env::var_os("CAS_SPACE_REPORT").is_some() {
        let initial = cas_core::space::Observation::inspect(store.tickets()).unwrap();
        let limits = cas_core::space::Limits::new(
            initial.capacity(),
            store.config().segment_bytes,
            cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
        )
        .unwrap();
        let physical =
            cas_core::space::Governor::open(Arc::clone(store.tickets()), limits).unwrap();
        Host::governed(
            resources,
            store,
            images,
            physical,
            1024 * MAX_REQUEST_BYTES as u64,
        )
        .unwrap()
    } else {
        Host::new(resources, store, images).unwrap()
    }
}

pub(crate) struct Pause {
    entered: mpsc::Sender<()>,
    resume: mpsc::Receiver<()>,
}

#[derive(Default)]
pub(crate) struct Control {
    pub compaction: Option<Pause>,
    pub fetch: Option<Pause>,
    pub before_fetch: Option<Pause>,
    pub rotation: Option<Pause>,
    pub collection: Option<Pause>,
    pub snapshot: Option<Pause>,
    pub quiescence_error: bool,
}

impl Pause {
    pub fn wait(self) {
        self.entered.send(()).unwrap();
        self.resume
            .recv_timeout(IO_DEADLINE + Duration::from_secs(10))
            .unwrap();
    }
}

fn identity(image: u8) -> Identity {
    Identity {
        store: STORE.store,
        image: [image; 16],
        image_bytes: IMAGE_BYTES,
    }
}
fn path(root: &Path, image: u8) -> std::path::PathBuf {
    root.join("images").join(format!("{image:02x}").repeat(16))
}

pub(crate) fn create(root: &Path, images: u8, resources: Arc<Resources>) -> Host {
    create_sized(root, images, resources, IMAGE_BYTES)
}

fn create_sized(root: &Path, images: u8, resources: Arc<Resources>, image_bytes: u64) -> Host {
    create_with_limits(
        root,
        images,
        resources,
        image_bytes,
        append::Limits::default(),
    )
}

fn create_with_limits(
    root: &Path,
    images: u8,
    resources: Arc<Resources>,
    image_bytes: u64,
    limits: append::Limits,
) -> Host {
    let (store, images) = create_images(
        root,
        images,
        &resources,
        image_bytes,
        limits,
        runtime_store(),
    );
    start(resources, store, images)
}

fn create_images(
    root: &Path,
    images: u8,
    resources: &Arc<Resources>,
    image_bytes: u64,
    limits: append::Limits,
    config: Config,
) -> (Store, Vec<(Log, Manifest)>) {
    let tickets = Tickets::open(root, Arc::clone(&resources.metadata)).unwrap();
    let store = Store::create(
        Arc::clone(&tickets),
        config,
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    fs::create_dir(root.join("images")).unwrap();
    let images = (2..2 + images)
        .map(|image| {
            let directory = path(root, image);
            fs::create_dir(&directory).unwrap();
            let manifest = Manifest::create(
                &directory,
                Identity {
                    image_bytes,
                    ..identity(image)
                },
                Arc::clone(&resources.metadata),
            )
            .unwrap();
            let log = Log::create_shared(
                Arc::clone(&tickets),
                append::Config {
                    store: STORE.store,
                    image: [image; 16],
                    image_bytes,
                    segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
                },
                limits,
                Arc::clone(&resources.metadata),
                manifest.view().unwrap(),
            )
            .unwrap();
            (log, manifest)
        })
        .collect();
    (store, images)
}

fn reopen(root: &Path, images: u8, resources: Arc<Resources>) -> Host {
    let (store, images) = recovered_images(root, images, &resources);
    start(resources, store, images)
}

fn recovered_images(
    root: &Path,
    images: u8,
    resources: &Arc<Resources>,
) -> (Store, Vec<(Log, Manifest)>) {
    let tickets = Tickets::open(root, Arc::clone(&resources.metadata)).unwrap();
    let store = Store::inspect(
        Arc::clone(&tickets),
        runtime_store(),
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    let inspected: Vec<_> = (2..2 + images)
        .map(|image| {
            let manifest = Manifest::inspect(
                &path(root, image),
                identity(image),
                0,
                Arc::clone(&resources.metadata),
                |hash| {
                    if store.contains(&hash) {
                        Ok(())
                    } else {
                        Err(io::Error::other("missing chunk"))
                    }
                },
            )
            .unwrap();
            let log = Log::inspect_shared(
                Arc::clone(&tickets),
                &manifest,
                append::Limits::default(),
                Arc::clone(&resources.metadata),
            )
            .unwrap();
            log.require_prefix(0).unwrap();
            (log, manifest)
        })
        .collect();
    let store = store.recover().unwrap();
    let images = inspected
        .into_iter()
        .map(|(log, manifest)| {
            let manifest = manifest.recover().unwrap();
            let log = log.fresh(manifest.view().unwrap(), 0).unwrap();
            (log, manifest)
        })
        .collect();
    (store, images)
}

fn attach(host: &mut Host, image: u8) -> Local {
    host.local(
        [image; 16],
        &EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap(),
    )
    .unwrap()
}

fn completed(local: &mut Local) -> Completed {
    let completed = received(local);
    assert!(completed.result.is_ok(), "{:?}", completed.result);
    completed
}

fn received(local: &mut Local) -> Completed {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(completed) = local.receive(false).unwrap() {
            return completed;
        }
        assert!(
            Instant::now() < deadline,
            "host request deadline: {}",
            local.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn write(local: &mut Local, id: u64, offset: usize, bytes: &[u8]) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let permit = loop {
        if let Some(permit) = local.prepare(Kind::Write(bytes.len())).unwrap() {
            break permit;
        }
        assert!(
            Instant::now() < deadline,
            "write admission deadline: {}",
            local.report()
        );
        thread::sleep(Duration::from_millis(1));
    };
    local
        .gather(
            id,
            QueueHead {
                queue: 0,
                head: (id % 256) as u16,
            },
            offset as u64,
            bytes.len(),
            permit,
            |target| {
                target.copy_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
    local.seal().unwrap();
    assert_eq!(completed(local).id, id);
}

fn read(local: &mut Local, id: u64, expected: &[u8]) {
    read_at(local, id, 0, expected)
}

fn read_at(local: &mut Local, id: u64, offset: u64, expected: &[u8]) {
    let permit = local.prepare(Kind::Read(expected.len())).unwrap().unwrap();
    local
        .enqueue(
            id,
            Operation::Read {
                offset,
                buffer: AlignedBuffer::new(expected.len()),
            },
            permit,
        )
        .unwrap();
    let completion = completed(local);
    assert_eq!(completion.id, id);
    let CompletionData::Read(bytes) = completion.data else {
        panic!("read response");
    };
    assert_eq!(bytes.as_slice(), expected);
}

fn drained(local: &mut Local, prefix: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let report = local.report();
        assert!(
            local.shared.health.lock().unwrap().failure.is_none(),
            "{report}"
        );
        if report["status"]["compacted"].as_u64().unwrap() >= prefix {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "idle compaction did not drain: {report}"
        );
        thread::sleep(Duration::from_millis(2));
    }
}

pub(crate) fn shutdown(mut host: Host) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match host.shutdown() {
            Ok(()) => break,
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(1))
            }
            Err(error) => panic!("host shutdown: {error}"),
        }
    }
}

#[test]
fn completed_compaction_reports_input_and_active_time_after_acknowledgment() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 1, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    write(&mut local, 0, 0, &[7; BLOCK_SIZE]);
    drained(&mut local, 1);
    let deadline = Instant::now() + Duration::from_secs(5);
    let totals = loop {
        let totals = *host.shared.compaction[0].lock().unwrap();
        if totals.batches > 0 {
            break totals;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(totals.input_bytes, BLOCK_SIZE as u64);
    assert_eq!(totals.candidate_output_bytes, BLOCK_SIZE as u64);
    assert!(totals.active_ns > 0);
    assert_eq!((totals.failed, totals.deferred), (0, 0));
    read(&mut local, 1, &[7; BLOCK_SIZE]);
    drop(local);
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn multiple_reactors_compact_private_images_and_reopen_shared_chunks() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 3, Arc::clone(&resources));
    let mut locals: Vec<_> = (2..5).map(|image| attach(&mut host, image)).collect();
    let mut models = vec![vec![7; IMAGE_BYTES as usize]; 3];
    for (local, model) in locals.iter_mut().zip(&models) {
        write(local, 0, 0, model);
    }
    for local in &mut locals {
        drained(local, 1);
    }
    assert_eq!(host.store_status().chunks, 1);
    for round in 0..24 {
        for (image, (local, model)) in locals.iter_mut().zip(&mut models).enumerate() {
            let block = (round * 11 + image * 7) % 63;
            let value = if round % 4 == 0 {
                0
            } else {
                (round + image + 10) as u8
            };
            let bytes = [value; 2 * BLOCK_SIZE];
            write(local, 1 + round as u64 * 2, block * BLOCK_SIZE, &bytes);
            model[block * BLOCK_SIZE..(block + 2) * BLOCK_SIZE].copy_from_slice(&bytes);
            read(local, 2 + round as u64 * 2, model);
        }
    }
    for local in &mut locals {
        drained(local, 25);
    }
    for (local, model) in locals.iter_mut().zip(&models) {
        read(local, 100, model);
    }
    drop(locals);
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    assert_eq!(resources.compaction.usage().current.bytes, 0);
    let mut host = reopen(root.path(), 3, Arc::clone(&resources));
    let mut locals: Vec<_> = (2..5).map(|image| attach(&mut host, image)).collect();
    for (local, model) in locals.iter_mut().zip(&models) {
        read(local, 0, model);
    }
    drop(locals);
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    assert_eq!(resources.read_memory().usage().current.bytes, 0);
}

#[test]
fn shared_failure_linearizes_before_every_image_completion() {
    let metadata = metadata_budget();
    let host = state::HostGate::new(&metadata).unwrap();
    let first = state::Gate::new(ImageState::default(), Some(host.clone()), &metadata).unwrap();
    let second = state::Gate::new(ImageState::default(), Some(host.clone()), &metadata).unwrap();
    let guard = first.lock().unwrap();
    let (started, start) = mpsc::channel();
    let (finished, finish) = mpsc::channel();
    let failing = thread::spawn(move || {
        started.send(()).unwrap();
        host.fail("shared store sync failed".into());
        finished.send(()).unwrap();
    });
    start.recv().unwrap();
    assert!(finish.recv_timeout(Duration::from_millis(20)).is_err());
    drop(guard);
    finish.recv_timeout(Duration::from_secs(1)).unwrap();
    failing.join().unwrap();
    assert!(first.lock().unwrap().publish(1).is_err());
    assert!(second.lock().unwrap().publish(1).is_err());
}

#[test]
fn host_request_and_control_limits_are_shared_across_images() {
    let pools = pools::HostPools::new();
    let images: Vec<_> = (0..16).map(|_| pools.image()).collect();
    let mut requests = Vec::new();
    for image in &images {
        for _ in 0..128 {
            if let Some(credit) = image.requests.reserve(Amount {
                bytes: 0,
                requests: 1,
            }) {
                requests.push(credit);
            }
        }
    }
    assert_eq!(requests.len(), 1024);
    let mut control = Vec::new();
    for image in &images {
        for _ in 0..8 {
            if let Some(credit) = image.control.reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 1,
            }) {
                control.push(credit);
            }
        }
    }
    assert_eq!(control.len(), 32);
    assert!(
        images[15]
            .requests
            .reserve(Amount {
                bytes: 0,
                requests: 1
            })
            .is_none()
    );
    drop(requests);
    assert!(
        images[15]
            .requests
            .reserve(Amount {
                bytes: 0,
                requests: 1
            })
            .is_some()
    );
    drop(control);
    assert!(
        images[15]
            .control
            .reserve(Amount {
                bytes: BLOCK_SIZE,
                requests: 1
            })
            .is_some()
    );
}

#[test]
fn stalled_compactor_keeps_reads_live_and_retains_ownership_after_deadline() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let old = vec![5; IMAGE_BYTES as usize];
    write(&mut first, 0, 0, &old);
    write(&mut second, 0, 0, &old);
    drained(&mut first, 1);
    drained(&mut second, 1);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().compaction = Some(Pause { entered, resume });
    write(&mut first, 1, 0, &[9; BLOCK_SIZE]);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    let held = resources.compaction.usage().current.bytes;
    assert!(held > 0);
    let promise = host.shared.physical.as_ref().map(|physical| {
        let status = physical.status();
        assert!(status.background_active && status.promised >= capacity::METADATA_MARGIN);
        status.promised
    });
    read(&mut second, 1, &old);
    let deadline = Instant::now() + IO_DEADLINE + Duration::from_secs(3);
    while first.shared.health.lock().unwrap().failure.is_none() {
        assert!(
            Instant::now() < deadline,
            "background deadline did not fail the image"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert!(second.shared.health.lock().unwrap().failure.is_some());
    assert_eq!(resources.compaction.usage().current.bytes, held);
    assert_eq!(
        host.shared
            .physical
            .as_ref()
            .map(|physical| physical.status().promised),
        promise
    );
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    assert_eq!(first.report()["status"]["compacted"], 1);
    release.send(()).unwrap();
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.compaction.usage().current.bytes, 0);
    let mut host = reopen(root.path(), 2, resources);
    let mut first = attach(&mut host, 2);
    let mut expected = old;
    expected[..BLOCK_SIZE].fill(9);
    read(&mut first, 0, &expected);
    drop(first);
    shutdown(host);
}

#[test]
fn continuous_overwrites_force_compaction_before_writes_settle() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 1, Arc::new(Resources::default()));
    let mut local = attach(&mut host, 2);
    let mut model = vec![0; IMAGE_BYTES as usize];
    write(&mut local, 0, 0, &[1; BLOCK_SIZE]);
    model[..BLOCK_SIZE].fill(1);
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(1, Operation::Flush, permit).unwrap();
    completed(&mut local);
    assert_eq!(local.status.durable, 1);
    let started = Instant::now();
    let mut writes = 0;
    while started.elapsed() < Duration::from_millis(1800) {
        let block = writes % 64;
        let bytes = [(writes % 251 + 1) as u8; BLOCK_SIZE];
        write(&mut local, 2 + writes as u64, block * BLOCK_SIZE, &bytes);
        model[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE].copy_from_slice(&bytes);
        writes += 1;
        thread::sleep(Duration::from_millis(5));
    }
    assert!(
        local.report()["status"]["compacted"].as_u64().unwrap() > 0,
        "compaction must run before the stream becomes idle"
    );
    read(&mut local, 2 + writes as u64, &model);
    drained(&mut local, 1 + writes as u64);
    drop(local);
    shutdown(host);
}

#[test]
fn unique_live_data_crosses_reactor_wal_rotations_and_reclamation() {
    let root = tempfile::tempdir().unwrap();
    let mut host = create_sized(
        root.path(),
        1,
        Arc::new(Resources::default()),
        8 * MAX_REQUEST_BYTES as u64,
    );
    let mut local = attach(&mut host, 2);
    let mut model = vec![1; 8 * MAX_REQUEST_BYTES];
    for (index, block) in model.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
        block[..8].copy_from_slice(&(index as u64).to_le_bytes());
    }
    let batch = 64 * BLOCK_SIZE;
    for (index, bytes) in model.chunks(batch).enumerate() {
        write(&mut local, index as u64, index * batch, bytes);
    }
    drained(&mut local, 32);
    for (index, bytes) in model.chunks(MAX_REQUEST_BYTES).enumerate() {
        read_at(
            &mut local,
            32 + index as u64,
            (index * MAX_REQUEST_BYTES) as u64,
            bytes,
        );
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while local.report()["status"]["segments"].as_u64() != Some(1) {
        assert!(
            Instant::now() < deadline,
            "retired WAL segments were not reclaimed"
        );
        thread::sleep(Duration::from_millis(2));
    }
    assert!(
        local.report()["status"]["encoded_bytes"].as_u64().unwrap() > 8 * MAX_REQUEST_BYTES as u64
    );
    assert!(
        local.report()["status"]["allocated_bytes"]
            .as_u64()
            .unwrap()
            <= 2 * MAX_REQUEST_BYTES as u64
    );
    assert_eq!(host.store_status().chunks, 2048);
    assert!(host.store_status().segments > 1);
    drop(local);
    shutdown(host);
}

#[test]
fn shared_chunk_corruption_fails_every_image_before_later_success() {
    use std::os::unix::fs::FileExt;
    let root = tempfile::tempdir().unwrap();
    let mut host = create(root.path(), 2, Arc::new(Resources::default()));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    let hash = cas_core::chunk::Chunk::new(&[7; BLOCK_SIZE])
        .unwrap()
        .hash();
    let pin = host.shared.reader.plan(hash).unwrap().unwrap();
    let address = pin.address();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(
            root.path()
                .join("chunks")
                .join(format!("segment-{:020}.v2", address.segment())),
        )
        .unwrap();
    file.write_all_at(&[0], address.offset()).unwrap();
    let permit = first.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    first
        .enqueue(
            1,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(completion) = first.receive(false).unwrap() {
            assert!(completion.result.is_err());
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    assert!(first.shared.health.lock().unwrap().failure.is_some());
    assert!(second.shared.health.lock().unwrap().publish(1).is_err());
    drop((pin, first, second));
    shutdown(host);
}

#[test]
fn read_page_metadata_denial_returns_ioerr_without_failing_the_shared_store() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut other = attach(&mut host, 3);
    let permit = first.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let shared = first.shared.clone();
    let gate = shared.health.lock().unwrap();
    let held = resources
        .metadata
        .reserve(Amount {
            bytes: 128 * MAX_REQUEST_BYTES - resources.metadata.usage().current.bytes - BLOCK_SIZE,
            requests: 0,
        })
        .unwrap();
    first
        .enqueue(
            0,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    drop(gate);
    notify(first.input_wake.as_ref().unwrap()).unwrap();
    let completed = received(&mut first);
    assert_eq!(completed.id, 0);
    let error = completed.result.as_ref().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::OutOfMemory);
    assert_eq!(error.to_string(), "aligned buffer allocation denied");
    assert!(shared.health.lock().unwrap().failure.is_some());
    assert!(host.shared.gate.failure().is_none());
    assert_eq!(shared.pools.read_requests.usage().current.requests, 1);
    assert_eq!(
        shared.pools.read.usage().current.bytes,
        BLOCK_SIZE + MAX_REQUEST_BYTES
    );
    drop((completed, held));
    let permit = other.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    other
        .enqueue(
            0,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    let response = received(&mut other);
    assert!(response.result.is_ok());
    drop((response, shared, first, other));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
    assert_eq!(resources.read_memory().usage().current, Amount::default());
}
