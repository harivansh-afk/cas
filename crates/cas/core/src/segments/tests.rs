use super::*;
use crate::{BLOCK_SIZE, budget::Amount, direct, store::format::SegmentHeader};
use std::{fs::File, os::unix::fs::symlink};

fn metadata(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn open(root: &Path) -> io::Result<Arc<Tickets>> {
    Tickets::open(root, metadata(1024 * 1024))
}

fn durable_header(directory: &Path, ticket: u64) -> io::Result<()> {
    let header = SegmentHeader {
        store: [1; 16],
        number: ticket,
        capacity: 64 * 1024,
    };
    let file = direct::open(&directory.join(name(ticket)), true)?;
    direct::write(&file, &header.encode()?, 0)?;
    file.sync_all()?;
    File::open(directory)?.sync_all()
}

fn directories(root: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let chunks = root.join("chunks");
    let staging = root.join("images/02020202020202020202020202020202/staging");
    fs::create_dir_all(&chunks).unwrap();
    fs::create_dir_all(&staging).unwrap();
    (chunks, staging)
}

#[test]
fn live_and_archived_names_determine_the_next_shared_ticket() {
    let root = tempfile::tempdir().unwrap();
    let (chunks, staging) = directories(root.path());
    durable_header(&chunks, 3).unwrap();
    // Allocation enumerates names. Each owning store/log separately validates
    // the corresponding format and identity before any repair or admission.
    durable_header(&staging, 7).unwrap();
    let archive = staging.join("rejected");
    fs::create_dir(&archive).unwrap();
    for number in [3, 7, 19] {
        fs::write(
            archive.join(format!("{}-from-00000000000000004096-0", name(number))),
            b"suffix",
        )
        .unwrap();
    }
    let tickets = open(root.path()).unwrap();
    assert_eq!(tickets.status().highest, 19);
    assert!(open(root.path()).is_err());
    tickets
        .allocate(|next| {
            assert_eq!(next, 20);
            durable_header(&chunks, next)
        })
        .unwrap();
    assert_eq!(tickets.status().highest, 20);
    drop(tickets);
    fs::remove_dir_all(archive).unwrap(); // Greater durable header now survives.
    assert_eq!(open(root.path()).unwrap().status().highest, 20);
}

#[test]
fn creation_failure_stops_the_owner_and_restart_retains_the_failed_ticket() {
    let root = tempfile::tempdir().unwrap();
    let (chunks, _) = directories(root.path());
    let tickets = open(root.path()).unwrap();
    let result: io::Result<()> = tickets.allocate(|number| {
        durable_header(&chunks, number)?;
        Err(io::Error::from_raw_os_error(libc::EIO))
    });
    assert!(result.is_err());
    assert!(tickets.status().failed);
    assert!(
        tickets
            .allocate(|_| -> io::Result<()> { panic!("failed allocator called creator") })
            .is_err()
    );
    let original = fs::read(chunks.join(name(1))).unwrap();
    drop(tickets);
    let recovered = open(root.path()).unwrap();
    assert_eq!(recovered.status().highest, 1);
    recovered
        .allocate(|number| {
            assert_eq!(number, 2);
            durable_header(&chunks, number)
        })
        .unwrap();
    assert_eq!(fs::read(chunks.join(name(1))).unwrap(), original);
}

#[test]
fn concurrent_creators_serialize_through_durable_headers_and_directory_sync() {
    let root = tempfile::tempdir().unwrap();
    let (chunks, staging) = directories(root.path());
    let tickets = open(root.path()).unwrap();
    std::thread::scope(|scope| {
        for thread in 0..8 {
            let tickets = Arc::clone(&tickets);
            let directory = if thread % 2 == 0 { &chunks } else { &staging };
            scope.spawn(move || {
                for _ in 0..8 {
                    tickets
                        .allocate(|number| durable_header(directory, number))
                        .unwrap();
                }
            });
        }
    });
    assert_eq!(tickets.status().highest, 64);
    assert!(!tickets.status().failed);
    let mut seen = Vec::new();
    for path in [&chunks, &staging] {
        for file in fs::read_dir(path).unwrap() {
            let file = file.unwrap();
            let header = SegmentHeader::decode(&fs::read(file.path()).unwrap()).unwrap();
            assert_eq!(file.file_name().to_str().unwrap(), name(header.number));
            seen.push(header.number);
        }
    }
    seen.sort_unstable();
    assert_eq!(seen, (1..=64).collect::<Vec<_>>());
    drop(tickets);
    assert_eq!(open(root.path()).unwrap().status().highest, 64);
}

#[test]
fn duplicate_live_tickets_and_unexpected_namespace_entries_fail_read_only() {
    let root = tempfile::tempdir().unwrap();
    let (chunks, staging) = directories(root.path());
    durable_header(&chunks, 1).unwrap();
    durable_header(&staging, 1).unwrap();
    let original = fs::read(chunks.join(name(1))).unwrap();
    assert!(open(root.path()).is_err());
    assert_eq!(fs::read(chunks.join(name(1))).unwrap(), original);
    fs::remove_file(staging.join(name(1))).unwrap();
    for filename in [
        "unrecognized",
        "segment-00000000000000000000.v2",
        "segment-00000000000000000001.v2-extra",
    ] {
        fs::write(chunks.join(filename), b"retain").unwrap();
        assert!(open(root.path()).is_err());
        assert_eq!(fs::read(chunks.join(filename)).unwrap(), b"retain");
        fs::remove_file(chunks.join(filename)).unwrap();
    }
    let images = root.path().join("images");
    for image in ["short", "EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE"] {
        fs::create_dir(images.join(image)).unwrap();
        assert!(open(root.path()).is_err());
        fs::remove_dir(images.join(image)).unwrap();
    }
    assert!(open(root.path()).is_ok());
}

#[test]
fn namespace_symlinks_cannot_hide_tickets_or_escape_the_store() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    symlink(outside.path(), root.path().join("chunks")).unwrap();
    assert!(open(root.path()).is_err());
    fs::remove_file(root.path().join("chunks")).unwrap();
    let (chunks, staging) = directories(root.path());
    for target in [
        chunks.join(name(1)),
        chunks.join("rejected"),
        staging.join(name(2)),
    ] {
        symlink(outside.path(), &target).unwrap();
        assert!(open(root.path()).is_err());
        fs::remove_file(target).unwrap();
    }
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}

#[test]
fn scan_allocations_are_charged_and_exhaustion_never_calls_the_creator() {
    let root = tempfile::tempdir().unwrap();
    let (chunks, _) = directories(root.path());
    durable_header(&chunks, MAX_SEGMENT).unwrap();
    let denied = metadata(0);
    assert!(Tickets::open(root.path(), Arc::clone(&denied)).is_err());
    assert_eq!(denied.usage().current.bytes, 0);
    let memory = metadata(4096);
    let tickets = Tickets::open(root.path(), Arc::clone(&memory)).unwrap();
    assert_eq!(memory.usage().current.bytes, 0);
    assert!(memory.usage().peak.bytes > 0);
    assert!(
        tickets
            .allocate(|_| -> io::Result<()> { panic!("exhausted allocator called creator") })
            .is_err()
    );
    assert_eq!(tickets.status().highest, MAX_SEGMENT);
    assert!(!tickets.status().failed);
    assert_eq!(fs::read_dir(&chunks).unwrap().count(), 1);
    assert_eq!(
        fs::metadata(chunks.join(name(MAX_SEGMENT))).unwrap().len(),
        BLOCK_SIZE as u64
    );
}
