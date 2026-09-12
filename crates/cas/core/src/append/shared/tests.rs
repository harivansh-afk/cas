mod compaction;
mod rotation;

use super::*;
use crate::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    aligned::AlignedBuffer,
    append::{
        format::{Builder, Kind, RequestId},
        segment,
    },
    budget::Amount,
    chunk::Chunk,
    chunk_index::Hash,
    direct::faults::{self, Fault},
    manifest::{
        file::{Identity, Manifest},
        format::Extent,
    },
    store::file::{self as chunk_file, Store},
};
use std::{
    fs,
    os::{fd::AsRawFd, unix::fs::FileExt},
    path::Path,
};

const CONFIG: Config = Config {
    store: [1; 16],
    image: [2; 16],
    image_bytes: 8 * BLOCK_SIZE as u64,
    segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
};
fn memory() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 128 * MAX_REQUEST_BYTES,
        requests: 0,
    })
}
fn id(sequence: u64) -> RequestId {
    RequestId {
        attachment: 1,
        serial: sequence * 2,
        queue: 0,
        head: (sequence % 256) as u16,
    }
}
fn write(log: &mut Log, block: u64, value: u8) -> u64 {
    let sequence = log.status().issued + 1;
    let mut builder = Builder::new(log.config().image_bytes, BLOCK_SIZE).unwrap();
    builder
        .write(
            id(sequence),
            block * BLOCK_SIZE as u64,
            BLOCK_SIZE,
            |bytes| {
                bytes.fill(value);
                Ok(())
            },
        )
        .unwrap();
    log.append(builder).unwrap();
    sequence
}

struct Fixture {
    root: tempfile::TempDir,
    tickets: Arc<Tickets>,
    store: Store,
    manifest: Option<Manifest>,
    log: Option<Log>,
    config: Config,
}
impl Fixture {
    fn new() -> Self {
        Self::with_config(CONFIG)
    }

    fn with_config(config: Config) -> Self {
        let root = tempfile::tempdir().unwrap();
        let tickets = Tickets::open(root.path(), memory()).unwrap();
        let store = Store::create(
            Arc::clone(&tickets),
            chunk_file::Config {
                store: config.store,
                segment_bytes: 128 * BLOCK_SIZE as u64,
            },
            memory(),
            memory(),
        )
        .unwrap();
        let image = staging(&tickets, config.image).parent().unwrap().to_owned();
        fs::create_dir_all(&image).unwrap();
        File::open(image.parent().unwrap())
            .unwrap()
            .sync_all()
            .unwrap();
        let manifest = Manifest::create(
            &image,
            Identity {
                store: config.store,
                image: config.image,
                image_bytes: config.image_bytes,
            },
            memory(),
        )
        .unwrap();
        let log = Log::create_shared(
            Arc::clone(&tickets),
            config,
            Limits::default(),
            memory(),
            manifest.view().unwrap(),
        )
        .unwrap();
        Self {
            root,
            tickets,
            store,
            manifest: Some(manifest),
            log: Some(log),
            config,
        }
    }
    fn identity() -> Identity {
        Identity {
            store: CONFIG.store,
            image: CONFIG.image,
            image_bytes: CONFIG.image_bytes,
        }
    }
    fn log(&mut self) -> &mut Log {
        self.log.as_mut().unwrap()
    }
    fn path(&self) -> PathBuf {
        staging(&self.tickets, self.config.image)
    }
    fn put(&mut self, value: u8) -> Hash {
        let bytes = [value; BLOCK_SIZE];
        let chunk = Chunk::new(&bytes).unwrap();
        self.store.insert(&[chunk]).unwrap();
        chunk.hash()
    }
    fn commit(&mut self, durable: u64, extents: &[Extent]) {
        let manifest = self.manifest.as_mut().unwrap();
        let prepared = manifest.prepare(extents, durable).unwrap();
        manifest.publish(prepared).unwrap();
    }
    fn inspect(&mut self) -> (Inspection, SharedRecovery) {
        drop(self.log.take());
        drop(self.manifest.take());
        let inspection = Manifest::inspect(
            self.path().parent().unwrap(),
            Identity {
                store: self.config.store,
                image: self.config.image,
                image_bytes: self.config.image_bytes,
            },
            0,
            memory(),
            |hash| require(self.store.plan(hash)?.is_some(), "missing test chunk"),
        )
        .unwrap();
        let wal = Log::inspect_shared(
            Arc::clone(&self.tickets),
            &inspection,
            Limits::default(),
            memory(),
        )
        .unwrap();
        (inspection, wal)
    }
    fn fresh(&mut self, required: u64) {
        let (manifest, wal) = self.inspect();
        let manifest = manifest.recover().unwrap();
        let log = wal.fresh(manifest.view().unwrap(), required).unwrap();
        self.log = Some(log);
        self.manifest = Some(manifest);
    }
    fn read(&self, plan: &crate::append::ReadPlan) -> Vec<u8> {
        let mut output = AlignedBuffer::new(plan.bytes());
        plan.read_with(&mut output, |hash, bytes| {
            self.store.plan(hash)?.unwrap().load(bytes)
        })
        .unwrap();
        output.as_slice().to_vec()
    }
}

fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn visit(path: &Path, out: &mut Vec<(PathBuf, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                visit(&path, out);
            } else {
                out.push((path.clone(), fs::read(path).unwrap()));
            }
        }
    }
    let mut result = Vec::new();
    visit(path, &mut result);
    result.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    result
}
fn punch(file: &File, offset: u64, length: u64) {
    // SAFETY: a live test file descriptor and bounded aligned positive ranges.
    let result = unsafe {
        libc::fallocate(
            file.as_raw_fd(),
            libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
            offset as i64,
            length as i64,
        )
    };
    assert_eq!(result, 0, "{}", io::Error::last_os_error());
    file.sync_all().unwrap();
}

#[test]
fn shared_chunk_and_image_tickets_define_fresh_epochs_and_survive_reopen() {
    let mut f = Fixture::new();
    assert_eq!(f.log().status().epoch, 1);
    f.put(11); // Chunk segment gets ticket 2.
    f.log().flush().unwrap();
    f.log().rollover().unwrap(); // Ticket 3, same epoch.
    assert_eq!(f.log().status().epoch, 1);
    f.log().new_attachment().unwrap(); // Ticket 4 and epoch 4.
    assert_eq!(f.log().status().epoch, 4);
    write(f.log(), 0, 33);
    f.fresh(1); // Ticket 5 and epoch 5.
    assert_eq!(f.log().status().epoch, 5);
    let path = f.path();
    for ticket in [1, 3, 4, 5] {
        let bytes = fs::read(path.join(segment::name(ticket))).unwrap();
        let header = crate::append::format::SegmentHeader::decode(&bytes[..BLOCK_SIZE]).unwrap();
        assert_eq!(header.number, ticket);
        assert_eq!(header.epoch, if ticket < 4 { 1 } else { ticket });
    }
    let Fixture {
        root,
        tickets,
        store,
        manifest,
        log,
        ..
    } = f;
    drop((log, manifest, store, tickets));
    let tickets = Tickets::open(root.path(), memory()).unwrap();
    assert_eq!(tickets.status().highest, 5);
}

#[test]
fn punched_base_payload_recovers_with_zero_and_newer_write_precedence() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    write(f.log(), 1, 20);
    f.log().flush().unwrap();
    let a = f.put(10);
    let b = f.put(20);
    f.commit(
        2,
        &[
            Extent {
                start: 0,
                end: 1,
                hash: Some(a),
            },
            Extent {
                start: 1,
                end: 2,
                hash: Some(b),
            },
        ],
    );
    let file = f.log().current().file.clone();
    punch(&file, 2 * BLOCK_SIZE as u64, BLOCK_SIZE as u64);
    punch(&file, 4 * BLOCK_SIZE as u64, BLOCK_SIZE as u64);
    drop(file);
    f.fresh(2);
    assert_eq!(
        (
            f.log().status().published,
            f.log().status().durable,
            f.log().status().intervals
        ),
        (2, 2, 0)
    );
    let old = f.log().read_plan(0, 3 * BLOCK_SIZE, 2).unwrap();
    let mut no_loader = AlignedBuffer::new(3 * BLOCK_SIZE);
    assert!(old.read_into(&mut no_loader).is_err());
    let mut zero = Builder::new(CONFIG.image_bytes, 0).unwrap();
    zero.zero(id(3), 0, BLOCK_SIZE as u64).unwrap();
    f.log().append(zero).unwrap();
    write(f.log(), 1, 30);
    let new = f.log().read_plan(0, 3 * BLOCK_SIZE, 4).unwrap();
    assert_eq!(
        (new.staged(0), new.staged(1), new.staged(2)),
        (true, true, false)
    );
    assert_eq!(
        f.read(&old),
        [[10; BLOCK_SIZE], [20; BLOCK_SIZE], [0; BLOCK_SIZE]].concat()
    );
    assert_eq!(
        f.read(&new),
        [[0; BLOCK_SIZE], [30; BLOCK_SIZE], [0; BLOCK_SIZE]].concat()
    );
    drop(old);
    drop(new);
    f.fresh(4);
    let plan = f.log().read_plan(0, 3 * BLOCK_SIZE, 4).unwrap();
    assert_eq!(
        f.read(&plan),
        [[0; BLOCK_SIZE], [30; BLOCK_SIZE], [0; BLOCK_SIZE]].concat()
    );
}

#[test]
fn live_identity_below_d_uses_headers_and_replays_only_the_owned_tail() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    punch(
        &f.log().current().file,
        2 * BLOCK_SIZE as u64,
        BLOCK_SIZE as u64,
    );
    let epoch = f.log().status().epoch;
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let mutation = |sequence| Mutation {
        id: id(sequence),
        sequence,
        offset: 0,
        length: BLOCK_SIZE as u64,
        kind: Kind::Write,
    };
    let mut live = wal
        .live(
            manifest.view().unwrap(),
            1,
            epoch,
            2,
            vec![mutation(1), mutation(2)],
        )
        .unwrap();
    assert_eq!(live.next(), Some(mutation(2)));
    live.replay_next(|bytes| {
        bytes.fill(42);
        Ok(())
    })
    .unwrap();
    drop(live); // Another crash before recovery FENCE.
    drop(manifest);
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let live = wal
        .live(
            manifest.view().unwrap(),
            1,
            epoch,
            2,
            vec![mutation(1), mutation(2)],
        )
        .unwrap();
    assert!(live.next().is_none());
    let mut log = live.finish().unwrap();
    assert_eq!((log.status().published, log.status().durable), (2, 2));
    let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut bytes).unwrap();
    assert_eq!(bytes.as_slice(), &[42; BLOCK_SIZE]);
}

#[test]
fn missing_retired_segment_is_allowed_only_at_or_below_d_without_live_owners() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    f.log().rollover().unwrap();
    drop(f.log.take());
    fs::remove_file(f.path().join(segment::name(1))).unwrap();
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let before = snapshot(&f.path());
    let old = Mutation {
        id: id(1),
        sequence: 1,
        offset: 0,
        length: BLOCK_SIZE as u64,
        kind: Kind::Write,
    };
    assert!(
        wal.live(manifest.view().unwrap(), 1, 1, 1, vec![old])
            .is_err()
    );
    assert_eq!(snapshot(&f.path()), before);
    drop(manifest);
    f.fresh(1);
    let plan = f.log().read_plan(0, BLOCK_SIZE, 1).unwrap();
    assert_eq!(f.read(&plan), vec![10; BLOCK_SIZE]);
}

#[test]
fn splitting_a_batch_or_losing_headers_before_d_never_repairs_the_wal() {
    let mut f = Fixture::new();
    let mut batch = Builder::new(CONFIG.image_bytes, 2 * BLOCK_SIZE).unwrap();
    for sequence in 1..=2 {
        batch
            .write(
                id(sequence),
                (sequence - 1) * BLOCK_SIZE as u64,
                BLOCK_SIZE,
                |bytes| {
                    bytes.fill(10);
                    Ok(())
                },
            )
            .unwrap();
    }
    f.log().append(batch).unwrap();
    f.log().flush().unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    drop(f.log.take());
    drop(f.manifest.take());
    let manifest = Manifest::inspect(
        f.path().parent().unwrap(),
        Fixture::identity(),
        0,
        memory(),
        |_| Ok(()),
    )
    .unwrap();
    let before = snapshot(&f.path());
    assert!(
        Log::inspect_shared(
            Arc::clone(&f.tickets),
            &manifest,
            Limits::default(),
            memory()
        )
        .is_err()
    );
    assert_eq!(snapshot(&f.path()), before);
    drop(manifest);
    // Fix the simulated cut, then corrupt its required header. Neither an intact
    // COMMIT nor the surviving payload permits skipping that WAL header.
    let manifest = Manifest::inspect(
        f.path().parent().unwrap(),
        Fixture::identity(),
        0,
        memory(),
        |_| Ok(()),
    )
    .unwrap()
    .recover()
    .unwrap();
    f.manifest = Some(manifest);
    f.commit(2, &[]);
    drop(f.manifest.take());
    let file = fs::OpenOptions::new()
        .write(true)
        .open(f.path().join(segment::name(1)))
        .unwrap();
    file.write_all_at(&[0xff], BLOCK_SIZE as u64).unwrap();
    drop(file);
    let before = snapshot(&f.path());
    let manifest = Manifest::inspect(
        f.path().parent().unwrap(),
        Fixture::identity(),
        0,
        memory(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        Log::inspect_shared(
            Arc::clone(&f.tickets),
            &manifest,
            Limits::default(),
            memory()
        )
        .is_err()
    );
    assert_eq!(snapshot(&f.path()), before);
}

#[test]
fn stale_or_unsynced_manifest_cannot_authorize_wal_suffix_repair() {
    let mut f = Fixture::new();
    let stale = f.manifest.as_ref().unwrap().view().unwrap();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    drop(stale); // Reopen must acquire the actual manifest file lock.
    drop(f.log.take());
    let path = f.path().join(segment::name(1));
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.set_len(file.metadata().unwrap().len() + 17).unwrap();
    drop(file);
    let (manifest, wal) = f.inspect();
    let before = snapshot(&f.path());
    faults::inject(Fault::Sync);
    assert!(manifest.recover().is_err());
    assert_eq!(snapshot(&f.path()), before);
    // The WAL's candidate pin also excludes a second manifest inspection until
    // this failed recovery owner is dropped.
    assert!(
        Manifest::inspect(
            f.path().parent().unwrap(),
            Fixture::identity(),
            0,
            memory(),
            |_| Ok(())
        )
        .is_err()
    );
    drop(wal);
    let (manifest, wal) = f.inspect();
    let mut manifest = manifest.recover().unwrap();
    let newer = manifest.prepare(&[], 1).unwrap();
    manifest.publish(newer).unwrap();
    assert!(wal.fresh(manifest.view().unwrap(), 1).is_err());
    assert_eq!(snapshot(&f.path()), before);
    drop(manifest);
    f.fresh(1);
    let archive = snapshot(&f.path().join("rejected"));
    assert_eq!(archive.len(), 1);
    assert_eq!(archive[0].1, vec![0; 17]);
}

#[test]
fn missing_p_above_d_and_failed_recovery_sync_do_not_return_a_serving_log() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let before = snapshot(&f.path());
    assert!(wal.fresh(manifest.view().unwrap(), 2).is_err());
    assert_eq!(snapshot(&f.path()), before);
    drop(manifest);
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let live = wal.live(manifest.view().unwrap(), 1, 1, 1, vec![]).unwrap();
    faults::inject(Fault::Sync);
    assert!(live.finish().is_err());
    drop(manifest);
    f.fresh(1);
    assert_eq!(f.log().status().durable, 1);
}

#[test]
fn missing_prefix_above_d_and_an_unrelated_identical_view_are_rejected() {
    let mut f = Fixture::new();
    let (manifest, wal) = f.inspect();
    let other = tempfile::tempdir().unwrap();
    let other_manifest = Manifest::create(other.path(), Fixture::identity(), memory()).unwrap();
    assert_eq!(manifest.selected().commit, other_manifest.current());
    let before = snapshot(&f.path());
    assert!(wal.fresh(other_manifest.view().unwrap(), 0).is_err());
    assert_eq!(snapshot(&f.path()), before);
    drop(manifest);
    f.fresh(0);
    let previous = f.log().current().header.number;
    write(f.log(), 0, 1);
    f.log().flush().unwrap();
    f.log().rollover().unwrap();
    drop(f.log.take());
    drop(f.manifest.take());
    for entry in fs::read_dir(f.path()).unwrap() {
        let path = entry.unwrap().path();
        let number = segment::number(path.file_name().unwrap().to_str().unwrap()).unwrap();
        if number <= previous {
            fs::remove_file(path).unwrap();
        }
    }
    let before = snapshot(&f.path());
    let manifest = Manifest::inspect(
        f.path().parent().unwrap(),
        Fixture::identity(),
        0,
        memory(),
        |_| Ok(()),
    )
    .unwrap();
    assert!(
        Log::inspect_shared(
            Arc::clone(&f.tickets),
            &manifest,
            Limits::default(),
            memory()
        )
        .is_err()
    );
    assert_eq!(snapshot(&f.path()), before);
}

#[test]
fn prepared_live_plan_is_read_only_and_requires_the_exact_stabilized_manifest() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    let epoch = f.log().status().epoch;
    drop(f.log.take());
    let segment = f.path().join(segment::name(epoch));
    let file = fs::OpenOptions::new().write(true).open(&segment).unwrap();
    file.set_len(file.metadata().unwrap().len() + 17).unwrap();
    drop(file);
    let (manifest, wal) = f.inspect();
    let before = snapshot(f.root.path());
    let retained = Mutation {
        id: id(1),
        sequence: 1,
        offset: 0,
        length: BLOCK_SIZE as u64,
        kind: Kind::Write,
    };
    let plan = wal.prepare_live(1, epoch, 1, [retained]).unwrap();
    assert_eq!(snapshot(f.root.path()), before);
    let other = tempfile::tempdir().unwrap();
    let other_manifest = Manifest::create(other.path(), Fixture::identity(), memory()).unwrap();
    assert!(
        plan.start(
            other_manifest.view().unwrap(),
            crate::space::Recovery::default()
        )
        .is_err()
    );
    assert_eq!(snapshot(f.root.path()), before);
    drop(manifest);
    let (manifest, wal) = f.inspect();
    let plan = wal.prepare_live(1, epoch, 1, [retained]).unwrap();
    let manifest = manifest.recover().unwrap();
    let replay = plan
        .start(manifest.view().unwrap(), crate::space::Recovery::default())
        .unwrap();
    assert!(replay.next().is_none());
    let mut log = replay.finish().unwrap();
    let mut output = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut output).unwrap();
    assert_eq!(output.as_slice(), &[10; BLOCK_SIZE]);
    assert_eq!(snapshot(&f.path().join("rejected"))[0].1, vec![0; 17]);
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn live_replay_retains_its_physical_governor_through_append_and_finish() {
    let mut f = Fixture::new();
    let epoch = f.log().status().epoch;
    let (manifest, wal) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let limits = crate::space::Limits::new(
        crate::space::Observation::inspect(&f.tickets)
            .unwrap()
            .capacity(),
        CONFIG.segment_bytes,
        crate::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )
    .unwrap();
    let physical = crate::space::Governor::open(Arc::clone(&f.tickets), limits).unwrap();
    let weak = Arc::downgrade(&physical);
    let mutation = Mutation {
        id: id(1),
        sequence: 1,
        offset: 0,
        length: BLOCK_SIZE as u64,
        kind: Kind::Write,
    };
    let plan = wal.prepare_live(0, epoch, 1, [mutation]).unwrap();
    let mut replay = plan
        .start(
            manifest.view().unwrap(),
            crate::space::Recovery::governed(&physical),
        )
        .unwrap();
    drop(physical);
    let before = snapshot(f.root.path());
    let held = weak
        .upgrade()
        .unwrap()
        .background(BLOCK_SIZE as u64)
        .unwrap();
    let error = replay
        .replay_next(|bytes| {
            bytes.fill(9);
            Ok(())
        })
        .unwrap_err();
    assert!(
        matches!(error, super::super::Error::Io(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    assert_eq!(replay.published(), 0);
    assert_eq!(snapshot(f.root.path()), before);
    drop(held);
    replay
        .replay_next(|bytes| {
            bytes.fill(9);
            Ok(())
        })
        .unwrap();
    assert_eq!(weak.upgrade().unwrap().status().promised, 0);
    let mut log = replay.finish().unwrap();
    assert!(weak.upgrade().is_none());
    assert_eq!(log.status().durable, 1);
    let mut output = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut output).unwrap();
    assert_eq!(output.as_slice(), &[9; BLOCK_SIZE]);
}

#[test]
fn a_failed_shared_segment_creation_poison_stops_every_allocator_user() {
    let mut f = Fixture::new();
    f.log().flush().unwrap();
    faults::inject(Fault::Allocate);
    assert!(f.log().rollover().is_err());
    assert!(f.log().status().failed);
    assert!(f.tickets.status().failed);
    let before = snapshot(&f.path());
    let bytes = [1; BLOCK_SIZE];
    assert!(f.store.insert(&[Chunk::new(&bytes).unwrap()]).is_err());
    assert_eq!(snapshot(&f.path()), before);
    assert!(f.path().join(segment::name(2)).exists());
    let Fixture {
        root,
        tickets,
        store,
        manifest,
        log,
        ..
    } = f;
    drop((log, manifest, store, tickets));
    assert_eq!(
        Tickets::open(root.path(), memory())
            .unwrap()
            .status()
            .highest,
        2
    );
}

#[test]
fn read_coverage_spans_all_256_blocks_including_zero_only_intervals() {
    let root = tempfile::tempdir().unwrap();
    let config = Config {
        image_bytes: MAX_REQUEST_BYTES as u64,
        ..CONFIG
    };
    let mut log = Log::create(root.path().join("wal"), config, Limits::default()).unwrap();
    let mut batch = Builder::new(config.image_bytes, 0).unwrap();
    let blocks = [0, 63, 64, 127, 128, 191, 192, 255];
    for (sequence, block) in blocks.iter().enumerate() {
        batch
            .zero(
                id(sequence as u64 + 1),
                block * BLOCK_SIZE as u64,
                BLOCK_SIZE as u64,
            )
            .unwrap();
    }
    log.append(batch).unwrap();
    let plan = log.read_plan(0, MAX_REQUEST_BYTES, 8).unwrap();
    assert!(plan.ranges().is_empty());
    for block in 0..256 {
        assert_eq!(plan.staged(block), blocks.contains(&(block as u64)));
    }
}
