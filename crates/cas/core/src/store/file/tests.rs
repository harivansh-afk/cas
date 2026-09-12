use super::*;
use crate::{
    budget::Amount,
    direct::faults::{self, Fault},
    manifest::{
        file::{Identity, Manifest},
        format::Extent,
    },
};
use std::{os::unix::fs::FileExt, path::Path, sync::Mutex};

mod readers;

const CONFIG: Config = Config {
    store: [1; 16],
    segment_bytes: (MAX_BATCH_BYTES + BLOCK_SIZE) as u64,
};

fn memory(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn metadata() -> Arc<Budget> {
    memory(128 * 1024 * 1024)
}
fn io_memory() -> Arc<Budget> {
    memory(1024 * 1024)
}

fn create(path: &Path) -> Store {
    let tickets = Tickets::open(path, metadata()).unwrap();
    Store::create(tickets, CONFIG, metadata(), io_memory()).unwrap()
}

fn inspect(path: &Path) -> io::Result<Inspection> {
    Store::inspect(
        Tickets::open(path, metadata())?,
        CONFIG,
        metadata(),
        io_memory(),
    )
}

fn hash(block: &[u8; BLOCK_SIZE]) -> Hash {
    Chunk::new(block).unwrap().hash()
}

fn insert(store: &mut Store, blocks: &[[u8; BLOCK_SIZE]]) -> io::Result<Inserted> {
    let chunks = blocks
        .iter()
        .map(|block| Chunk::new(block).unwrap())
        .collect::<std::vec::Vec<_>>();
    store.insert(&chunks)
}

fn read(store: &Store, block: &[u8; BLOCK_SIZE]) {
    let mut destination = AlignedBuffer::new(BLOCK_SIZE);
    store
        .plan(hash(block))
        .unwrap()
        .unwrap()
        .load(destination.as_mut_slice())
        .unwrap();
    assert_eq!(destination.as_slice(), block);
}

#[test]
fn packed_store_rotates_shared_tickets_and_rebuilds_inline_hashes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    let blocks = (1..=198)
        .map(|byte| [byte as u8; BLOCK_SIZE])
        .collect::<std::vec::Vec<_>>();
    for batch in blocks.chunks(MAX_CHUNKS) {
        assert_eq!(insert(&mut store, batch).unwrap().written, batch.len());
    }
    assert_eq!(store.status().chunks, 198);
    assert_eq!(store.status().segments, 4);
    assert_eq!(store.shared.tickets.status().highest, 4);
    for block in &blocks {
        read(&store, block);
    }
    let before = store.status().encoded_bytes;
    let duplicate = insert(&mut store, &[blocks[0], blocks[0], blocks[100]]).unwrap();
    assert_eq!(
        (duplicate.written, duplicate.reused, duplicate.encoded_bytes),
        (0, 3, 0)
    );
    assert_eq!(store.status().encoded_bytes, before);
    drop(store);
    let inspected = inspect(root.path()).unwrap();
    assert!(inspected.status().failed);
    assert_eq!(inspected.status().chunks, 198);
    for block in &blocks {
        assert!(inspected.contains(&hash(block)));
    }
    let recovered = inspected.recover().unwrap();
    assert!(!recovered.status().failed);
    for block in &blocks {
        read(&recovered, block);
    }
}

#[test]
fn concurrent_identical_insertions_produce_one_durable_chunk() {
    let root = tempfile::tempdir().unwrap();
    let owner = Mutex::new(create(root.path()));
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let owner = &owner;
            scope.spawn(move || {
                for _ in 0..8 {
                    insert(&mut owner.lock().unwrap(), &[[7; BLOCK_SIZE]; 3]).unwrap();
                }
            });
        }
    });
    let store = owner.lock().unwrap();
    assert_eq!(store.status().chunks, 1);
    assert_eq!(store.status().encoded_bytes, 3 * BLOCK_SIZE as u64);
    read(&store, &[7; BLOCK_SIZE]);
}

#[test]
fn batch_write_and_sync_failures_never_publish_new_hashes() {
    for fault in [Fault::ShortWrite, Fault::Sync] {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
        faults::inject(fault);
        assert!(insert(&mut store, &[[2; BLOCK_SIZE], [3; BLOCK_SIZE]]).is_err());
        assert!(store.status().failed);
        assert_eq!(store.status().chunks, 1);
        assert!(store.plan(hash(&[1; BLOCK_SIZE])).is_err());
        assert!(insert(&mut store, &[[4; BLOCK_SIZE]]).is_err());
        drop(store);
        let inspection = inspect(root.path()).unwrap();
        assert!(inspection.contains(&hash(&[1; BLOCK_SIZE])));
        assert_eq!(
            inspection.contains(&hash(&[2; BLOCK_SIZE])),
            fault == Fault::Sync
        );
        let recovered = inspection.recover().unwrap();
        read(&recovered, &[1; BLOCK_SIZE]);
        if fault == Fault::Sync {
            read(&recovered, &[2; BLOCK_SIZE]);
            read(&recovered, &[3; BLOCK_SIZE]);
        } else {
            let archives = fs::read_dir(root.path().join("chunks/rejected"))
                .unwrap()
                .collect::<Result<std::vec::Vec<_>, _>>()
                .unwrap();
            assert_eq!(archives.len(), 1);
            assert_eq!(
                fs::metadata(archives[0].path()).unwrap().len(),
                BLOCK_SIZE as u64
            );
        }
    }
}

#[test]
fn owned_read_pins_keep_actual_io_locks_after_store_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[4; BLOCK_SIZE]]).unwrap();
    let read = store.plan(hash(&[4; BLOCK_SIZE])).unwrap().unwrap();
    drop(store);
    assert!(inspect(root.path()).is_err());
    let mut destination = AlignedBuffer::new(BLOCK_SIZE);
    read.load(destination.as_mut_slice()).unwrap();
    assert_eq!(destination.as_slice(), &[4; BLOCK_SIZE]);
    drop(read);
    assert!(inspect(root.path()).unwrap().recover().is_ok());
}

#[test]
fn metadata_and_output_budgets_fail_before_payload_io() {
    let root = tempfile::tempdir().unwrap();
    let budget = memory(1024 * 1024);
    let tickets = Tickets::open(root.path(), Arc::clone(&budget)).unwrap();
    let mut store = Store::create(tickets, CONFIG, Arc::clone(&budget), io_memory()).unwrap();
    insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
    let file = root.path().join("chunks").join(segments::name(1));
    let original = fs::read(&file).unwrap();
    let remainder = 1024 * 1024 - budget.usage().current.bytes;
    let held = budget
        .reserve(Amount {
            bytes: remainder,
            requests: 0,
        })
        .unwrap();
    let missing = (2..=64)
        .map(|b| [b; BLOCK_SIZE])
        .collect::<std::vec::Vec<_>>();
    assert!(insert(&mut store, &missing).is_err());
    assert!(!store.status().failed);
    assert_eq!(fs::read(file).unwrap(), original);
    drop((held, store));
    assert_eq!(budget.usage().current.bytes, 0);

    let root = tempfile::tempdir().unwrap();
    let tickets = Tickets::open(root.path(), metadata()).unwrap();
    let mut store = Store::create(tickets, CONFIG, metadata(), memory(0)).unwrap();
    assert!(insert(&mut store, &[[1; BLOCK_SIZE]]).is_err());
    assert!(!store.status().failed);
    assert_eq!(fs::read_dir(root.path().join("chunks")).unwrap().count(), 0);
}

#[test]
fn failed_segment_allocation_retains_its_ticket_and_headerless_creation() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    faults::inject(Fault::Allocate);
    assert!(insert(&mut store, &[[1; BLOCK_SIZE]]).is_err());
    assert!(store.status().failed && store.shared.tickets.status().failed);
    drop(store);
    let inspection = inspect(root.path()).unwrap();
    assert_eq!(inspection.status().chunks, 0);
    let mut recovered = inspection.recover().unwrap();
    assert_eq!(recovered.shared.tickets.status().highest, 1);
    insert(&mut recovered, &[[1; BLOCK_SIZE]]).unwrap();
    assert_eq!(
        recovered
            .plan(hash(&[1; BLOCK_SIZE]))
            .unwrap()
            .unwrap()
            .address()
            .segment(),
        2
    );
    assert_eq!(
        fs::read_dir(root.path().join("chunks/rejected"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn payload_magic_is_data_and_reads_check_the_actual_header_and_crc() {
    let root = tempfile::tempdir().unwrap();
    let mut fake = Builder::new(1).unwrap();
    fake.push(Chunk::new(&[8; BLOCK_SIZE]).unwrap()).unwrap();
    let fake = fake.seal(999, 1, 1).unwrap();
    let mut payload = [0; BLOCK_SIZE];
    payload.copy_from_slice(&fake.bytes()[..BLOCK_SIZE]);
    let mut store = create(root.path());
    insert(&mut store, &[payload, [3; BLOCK_SIZE]]).unwrap();
    read(&store, &payload);
    let plan = store.plan(hash(&payload)).unwrap().unwrap();
    let file = root.path().join("chunks").join(segments::name(1));
    fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .write_all_at(&[0], plan.address().offset())
        .unwrap();
    let mut destination = AlignedBuffer::new(BLOCK_SIZE);
    assert!(plan.load(destination.as_mut_slice()).is_err());
}

fn image_path(root: &Path) -> std::path::PathBuf {
    let path = root.join("images/02020202020202020202020202020202");
    fs::create_dir_all(&path).unwrap();
    path
}

const IMAGE: Identity = Identity {
    store: CONFIG.store,
    image: [2; 16],
    image_bytes: 16 * BLOCK_SIZE as u64,
};

#[test]
fn manifest_and_store_reopen_validate_required_chunks_before_recovery() {
    for corrupt in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut store = create(root.path());
        let path = image_path(root.path());
        let mut manifest = Manifest::create(&path, IMAGE, metadata()).unwrap();
        insert(&mut store, &[[7; BLOCK_SIZE], [9; BLOCK_SIZE]]).unwrap();
        let edits = [
            Extent {
                start: 0,
                end: 1,
                hash: Some(hash(&[7; BLOCK_SIZE])),
            },
            Extent {
                start: 7,
                end: 8,
                hash: Some(hash(&[9; BLOCK_SIZE])),
            },
        ];
        let prepared = manifest.prepare(&edits, 1).unwrap();
        manifest.publish(prepared).unwrap();
        let address = store
            .plan(hash(&[9; BLOCK_SIZE]))
            .unwrap()
            .unwrap()
            .address();
        drop((store, manifest));
        let file = root
            .path()
            .join("chunks")
            .join(segments::name(address.segment()));
        if corrupt {
            fs::OpenOptions::new()
                .write(true)
                .open(&file)
                .unwrap()
                .write_all_at(&[0], address.offset())
                .unwrap();
        }
        let original = fs::read(&file).unwrap();
        let inspected = inspect(root.path()).unwrap();
        let image = Manifest::inspect(&path, IMAGE, 1, metadata(), |hash| {
            if inspected.contains(&hash) {
                Ok(())
            } else {
                Err(io::Error::other("required chunk unavailable"))
            }
        });
        if corrupt {
            assert!(image.is_err());
            assert_eq!(fs::read(file).unwrap(), original);
            assert!(!root.path().join("chunks/rejected").exists());
            assert!(!path.join("rejected").exists());
        } else {
            let store = inspected.recover().unwrap();
            let manifest = image.unwrap().recover().unwrap();
            let mut tree = manifest.tree().unwrap();
            for block in 0..16 {
                let expected = match block {
                    0 => Some(hash(&[7; BLOCK_SIZE])),
                    7 => Some(hash(&[9; BLOCK_SIZE])),
                    _ => None,
                };
                assert_eq!(tree.get(block).unwrap(), expected);
            }
            read(&store, &[7; BLOCK_SIZE]);
            read(&store, &[9; BLOCK_SIZE]);
        }
    }
}

#[test]
fn read_errors_and_foreign_headers_do_not_authorize_repair() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[4; BLOCK_SIZE]]).unwrap();
    drop(store);
    let file = root.path().join("chunks").join(segments::name(1));
    let original = fs::read(&file).unwrap();
    for successful_reads in 0..3 {
        faults::inject_after(Fault::Read, successful_reads);
        assert!(
            matches!(inspect(root.path()), Err(ref error) if error.raw_os_error() == Some(libc::EIO))
        );
        assert_eq!(fs::read(&file).unwrap(), original);
    }
    let inspected = inspect(root.path()).unwrap();
    faults::inject(Fault::Sync);
    assert!(inspected.recover().is_err());
    assert_eq!(fs::read(&file).unwrap(), original);
    assert!(inspect(root.path()).unwrap().recover().is_ok());
    let foreign = SegmentHeader {
        store: [6; 16],
        number: 1,
        capacity: CONFIG.segment_bytes,
    }
    .encode()
    .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .unwrap()
        .write_all_at(foreign.as_slice(), 0)
        .unwrap();
    assert!(inspect(root.path()).is_err());
    assert!(!root.path().join("chunks/rejected").exists());
}

#[test]
fn duplicate_records_rebuild_to_a_verified_location_and_remain_surplus() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[3; BLOCK_SIZE]]).unwrap();
    drop(store);
    let directory = root.path().join("chunks");
    let file = direct::open(&directory.join(segments::name(2)), true).unwrap();
    let header = SegmentHeader {
        store: CONFIG.store,
        number: 2,
        capacity: CONFIG.segment_bytes,
    };
    direct::write(&file, &header.encode().unwrap(), 0).unwrap();
    let mut builder = Builder::new(1).unwrap();
    builder.push(Chunk::new(&[3; BLOCK_SIZE]).unwrap()).unwrap();
    let batch = builder.seal(2, 1, 1).unwrap();
    direct::write_bytes(&file, batch.bytes(), BLOCK_SIZE as u64).unwrap();
    file.sync_all().unwrap();
    File::open(&directory).unwrap().sync_all().unwrap();
    drop(file);
    let mut store = inspect(root.path()).unwrap().recover().unwrap();
    assert_eq!((store.status().chunks, store.status().segments), (1, 2));
    assert_eq!(
        store
            .plan(hash(&[3; BLOCK_SIZE]))
            .unwrap()
            .unwrap()
            .address()
            .segment(),
        1
    );
    assert_eq!(store.status().encoded_bytes, 6 * BLOCK_SIZE as u64);
    insert(&mut store, &[[4; BLOCK_SIZE]]).unwrap();
    read(&store, &[3; BLOCK_SIZE]);
    read(&store, &[4; BLOCK_SIZE]);
}

#[test]
fn torn_segment_creations_are_archived_without_reusing_the_ticket() {
    for length in [1, BLOCK_SIZE - 1, BLOCK_SIZE] {
        let root = tempfile::tempdir().unwrap();
        drop(create(root.path()));
        let directory = root.path().join("chunks");
        let torn = std::vec![0; length];
        fs::write(directory.join(segments::name(1)), &torn).unwrap();
        let mut recovered = inspect(root.path()).unwrap().recover().unwrap();
        let archives = fs::read_dir(directory.join("rejected"))
            .unwrap()
            .collect::<Result<std::vec::Vec<_>, _>>()
            .unwrap();
        assert_eq!(archives.len(), 1);
        assert_eq!(fs::read(archives[0].path()).unwrap(), torn);
        assert!(!directory.join(segments::name(1)).exists());
        insert(&mut recovered, &[[6; BLOCK_SIZE]]).unwrap();
        assert_eq!(
            recovered
                .plan(hash(&[6; BLOCK_SIZE]))
                .unwrap()
                .unwrap()
                .address()
                .segment(),
            2
        );
    }
}

#[test]
fn repaired_tail_requires_allocation_before_any_new_payload_output() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
    faults::inject(Fault::ShortWrite);
    assert!(insert(&mut store, &[[2; BLOCK_SIZE]]).is_err());
    drop(store);
    let mut store = inspect(root.path()).unwrap().recover().unwrap();
    let file = root.path().join("chunks").join(segments::name(1));
    let original = fs::read(&file).unwrap();
    faults::inject(Fault::Allocate);
    assert!(
        matches!(insert(&mut store, &[[2; BLOCK_SIZE]]), Err(ref error) if error.raw_os_error() == Some(libc::ENOSPC))
    );
    assert_eq!(fs::read(file).unwrap(), original);
    assert_eq!(store.status().chunks, 1);
    assert!(store.status().failed);
}

#[test]
fn exhausted_record_ids_fail_before_output_and_empty_insertion_allocates_nothing() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    assert_eq!(store.insert(&[]).unwrap().encoded_bytes, 0);
    assert_eq!(fs::read_dir(root.path().join("chunks")).unwrap().count(), 0);
    insert(&mut store, &[[1; BLOCK_SIZE]]).unwrap();
    let file = root.path().join("chunks").join(segments::name(1));
    let original = fs::read(&file).unwrap();
    for (batch, ordinal) in [(u64::MAX, 2), (2, u64::MAX)] {
        store.shared.lock().segments[0].next_batch = batch;
        store.shared.lock().segments[0].next_ordinal = ordinal;
        assert!(insert(&mut store, &[[2; BLOCK_SIZE]]).is_err());
        assert_eq!(fs::read(&file).unwrap(), original);
        assert!(!store.status().failed);
        assert_eq!(store.status().chunks, 1);
    }
}
