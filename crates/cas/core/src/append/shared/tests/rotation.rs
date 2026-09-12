use super::*;
use crate::append::{Error, RotationKind};
use std::{sync::mpsc, thread, time::Duration};

fn later() -> Builder {
    let mut builder = Builder::new(CONFIG.image_bytes, BLOCK_SIZE).unwrap();
    builder
        .write(id(2), 0, BLOCK_SIZE, |bytes| {
            bytes.fill(2);
            Ok(())
        })
        .unwrap();
    builder
}

#[test]
fn rotation_preparation_is_cancelable_and_installation_is_bound_to_the_actual_log() {
    let mut f = Fixture::new();
    write(f.log(), 0, 7);
    assert!(matches!(
        f.log().prepare_rotation(RotationKind::Rollover),
        Err(Error::Pending)
    ));
    f.log().flush().unwrap();
    let before = f.log().status();
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    assert!(f.log().status().rotating);
    assert_eq!(f.tickets.status().highest, before.epoch);
    assert!(f.log().check_append(&later()).is_err());
    assert!(f.log().prepare_fence().is_err());
    assert!(f.log().prepare_rotation(RotationKind::Rollover).is_err());
    f.log().cancel_rotation(prepared).unwrap();
    assert!(!f.log().status().rotating);
    assert!(f.log().check_append(&later()).is_ok());
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    // A different shared-store output can claim the next ticket before the WAL.
    f.put(11);
    let prior_ticket = f.tickets.status().highest;
    let created = prepared.create().unwrap();
    assert_eq!(f.tickets.status().highest, prior_ticket + 1);
    assert_eq!(f.log().status().segments, before.segments);
    f.log().install_rotation(created).unwrap();
    assert_eq!(f.log().status().epoch, before.epoch);
    assert_eq!(f.log().status().published, before.published);
    assert_eq!(f.log().status().durable, before.durable);
    assert!(!f.log().covers_flush(before.published));
    f.log().flush().unwrap();
    let prepared = f
        .log()
        .prepare_rotation(RotationKind::FreshAttachment)
        .unwrap();
    let created = prepared.create().unwrap();
    f.log().install_rotation(created).unwrap();
    assert_eq!(f.log().status().epoch, f.tickets.status().highest);
    assert!(f.log().status().epoch > before.epoch);
    f.log().flush().unwrap();
    let (manifest, recovery) = f.inspect();
    let manifest = manifest.recover().unwrap();
    let mut log = recovery.fresh(manifest.view().unwrap(), 1).unwrap();
    let mut output = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(0, &mut output).unwrap();
    assert_eq!(output.as_slice(), &[7; BLOCK_SIZE]);
}

#[test]
fn worker_io_leaves_old_reads_live_until_the_synced_receipt_is_installed() {
    for pause in [
        Fault::Allocate,
        Fault::ShortWrite,
        Fault::FileSync,
        Fault::DirectorySync,
    ] {
        let mut f = Fixture::new();
        write(f.log(), 0, 7);
        f.log().flush().unwrap();
        let pin = f.log().read_plan(0, BLOCK_SIZE, 1).unwrap();
        let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
        let (entered, observed) = mpsc::channel();
        let (release, resume) = mpsc::channel();
        let worker = thread::spawn(move || {
            faults::pause_before(pause, entered, resume);
            prepared.create()
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
        pin.read_into(&mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), &[7; BLOCK_SIZE]);
        assert_eq!(f.log().status().segments, 1);
        assert!(f.log().status().rotating);
        release.send(()).unwrap();
        let created = worker.join().unwrap().unwrap();
        f.log().install_rotation(created).unwrap();
        assert_eq!(f.log().status().segments, 2);
        assert!(!f.log().status().rotating);
    }
}

#[test]
fn failed_or_foreign_receipts_cannot_install_or_release_old_file_ownership() {
    let mut f = Fixture::new();
    write(f.log(), 0, 7);
    f.log().flush().unwrap();
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    let created = prepared.create().unwrap();
    let mut foreign = Fixture::new();
    let unused = foreign
        .log()
        .prepare_rotation(RotationKind::Rollover)
        .unwrap();
    assert!(foreign.log().install_rotation(created).is_err());
    assert!(foreign.log().status().rotating);
    foreign.log().cancel_rotation(unused).unwrap();

    // Losing a produced receipt requires explicit failure/recovery, not cancel.
    f.log().fail();
    let (manifest, recovery) = f.inspect();
    let manifest = manifest.recover().unwrap();
    f.log = Some(recovery.fresh(manifest.view().unwrap(), 1).unwrap());
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    let created = prepared.create().unwrap();
    f.log().fail();
    assert!(f.log().install_rotation(created).is_err());

    let mut f = Fixture::new();
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    let path = f.path();
    drop(f.log.take());
    assert!(Log::inspect(&path, Limits::default()).is_err());
    drop(prepared);
    Log::inspect(&path, Limits::default()).unwrap();
}

#[test]
fn allocation_header_and_sync_failures_retain_the_durable_prefix() {
    for fault in [
        Fault::Allocate,
        Fault::Write,
        Fault::FileSync,
        Fault::DirectorySync,
    ] {
        let mut f = Fixture::new();
        write(f.log(), 0, 7);
        f.log().flush().unwrap();
        let old = fs::read(f.path().join(segment::name(f.log().status().epoch))).unwrap();
        faults::inject(fault);
        assert!(f.log().rollover().is_err());
        assert!(f.log().status().failed);
        assert_eq!(f.log().status().published, 1);
        assert_eq!(
            fs::read(f.path().join(segment::name(f.log().status().epoch))).unwrap(),
            old
        );
        assert!(f.tickets.status().failed);
        // A new root owner must inspect retained filenames before another ticket.
        let store_config = f.store.config();
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
        let store = Store::inspect(Arc::clone(&tickets), store_config, memory(), memory()).unwrap();
        let path = staging(&tickets, CONFIG.image);
        let manifest = Manifest::inspect(
            path.parent().unwrap(),
            Fixture::identity(),
            0,
            memory(),
            |hash| {
                if store.contains(&hash) {
                    Ok(())
                } else {
                    Err(io::Error::other("missing chunk"))
                }
            },
        )
        .unwrap();
        let recovery =
            Log::inspect_shared(tickets, &manifest, Limits::default(), memory()).unwrap();
        recovery.require_prefix(1).unwrap();
        let _store = store.recover().unwrap();
        let manifest = manifest.recover().unwrap();
        let mut log = recovery.fresh(manifest.view().unwrap(), 1).unwrap();
        let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
        log.read_into(0, &mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), &[7; BLOCK_SIZE]);
    }
}

#[test]
fn exhausted_fence_and_metadata_preparation_do_not_create_a_segment() {
    let mut f = Fixture::with_config(Config {
        image_bytes: 2 * MAX_REQUEST_BYTES as u64,
        segment_bytes: (MAX_REQUEST_BYTES + 3 * BLOCK_SIZE) as u64,
        ..CONFIG
    });
    let mut builder = Builder::new(f.config.image_bytes, MAX_REQUEST_BYTES).unwrap();
    builder
        .write(id(1), 0, MAX_REQUEST_BYTES, |bytes| {
            bytes.fill(7);
            Ok(())
        })
        .unwrap();
    f.log().append(builder).unwrap();
    f.log().flush().unwrap();
    let ticket = f.tickets.status().highest;
    let status = f.log().status();
    assert!(matches!(f.log().prepare_fence(), Err(Error::Rollover)));
    assert_eq!(f.tickets.status().highest, ticket);
    assert_eq!(f.log().status().encoded_bytes, status.encoded_bytes);
    let budget = Arc::clone(&f.log().metadata);
    let held = budget
        .reserve(Amount {
            bytes: 128 * MAX_REQUEST_BYTES - budget.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    assert!(f.log().prepare_rotation(RotationKind::Rollover).is_err());
    assert!(!f.log().status().rotating);
    assert_eq!(f.tickets.status().highest, ticket);
    drop(held);
    let prepared = f.log().prepare_rotation(RotationKind::Rollover).unwrap();
    let created = prepared.create().unwrap();
    f.log().install_rotation(created).unwrap();
    assert!(f.log().prepare_fence().is_ok());
}
