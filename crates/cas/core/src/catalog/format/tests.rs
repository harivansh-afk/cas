use super::*;
use crate::budget::Amount;

fn metadata(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn image(id: u8) -> Entry {
    Entry {
        id: [id; 16],
        kind: Kind::Image {
            image_bytes: BLOCK_SIZE as u64,
        },
    }
}

fn copy(contents: &Contents) -> Contents {
    let metadata = metadata(1024 * 1024);
    let mut buffer = AlignedBuffer::try_new_in(
        contents.bytes().len(),
        BudgetAllocator::new(Arc::clone(&metadata)),
    )
    .unwrap();
    buffer.as_mut_slice().copy_from_slice(contents.bytes());
    Contents { buffer, metadata }
}

#[test]
fn repaired_crc_does_not_hide_invalid_headers_entries_or_padding() {
    let source = Contents::empty([1; 16], metadata(1024 * 1024))
        .unwrap()
        .changed(Change::Insert(image(2)))
        .unwrap()
        .changed(Change::Insert(Entry {
            id: [9; 16],
            kind: Kind::Snapshot(SnapshotKey {
                commit: Commit {
                    store: [1; 16],
                    image: [2; 16],
                    image_bytes: 64 * BLOCK_SIZE as u64,
                    generation: 4,
                    root: Root {
                        offset: 8192,
                        height: 1,
                    },
                    durable: 19,
                },
                end: 16384,
            }),
        }))
        .unwrap();
    source.validate([1; 16]).unwrap();
    // Reserved fields in both entry types, exact geometry, snapshots and CRC
    // are separate invariants. Every mutation below has its CRC recomputed.
    for offset in [
        0,
        8,
        10,
        16,
        40,
        48,
        56,
        HEADER + 16,
        HEADER + 18,
        HEADER + 24,
        HEADER + 32,
        HEADER + 82,
        HEADER + ENTRY + 18,
        HEADER + ENTRY + 24,
        HEADER + ENTRY + 56,
        HEADER + ENTRY + 72,
        HEADER + ENTRY + 80,
        HEADER + ENTRY + 82,
        HEADER + 2 * ENTRY,
    ] {
        let mut invalid = copy(&source);
        invalid.buffer.as_mut_slice()[offset] ^= 1;
        invalid.seal();
        assert!(
            invalid.validate([1; 16]).is_err(),
            "accepted changed byte {offset}"
        );
    }
    for offset in [
        32,
        HEADER,
        HEADER + ENTRY,
        HEADER + ENTRY + 32,
        HEADER + ENTRY + 48,
    ] {
        let mut invalid = copy(&source);
        let length = if offset == 32 || offset == HEADER + ENTRY + 48 {
            8
        } else {
            16
        };
        invalid.buffer.as_mut_slice()[offset..offset + length].fill(0);
        invalid.seal();
        assert!(invalid.validate([1; 16]).is_err());
    }
    for duplicate in [false, true] {
        let mut invalid = copy(&source);
        let id = if duplicate { [2; 16] } else { [1; 16] };
        invalid.buffer.as_mut_slice()[HEADER + ENTRY..HEADER + ENTRY + 16].copy_from_slice(&id);
        invalid.seal();
        assert!(invalid.validate([1; 16]).is_err());
    }
    let mut invalid = copy(&source);
    invalid.buffer.as_mut_slice()[CRC] ^= 1;
    assert!(invalid.validate([1; 16]).is_err());
}

#[test]
fn output_validation_and_checked_lengths_precede_allocation() {
    let account = metadata(BLOCK_SIZE);
    assert!(Contents::empty([0; 16], Arc::clone(&account)).is_err());
    let original = Contents::empty([1; 16], Arc::clone(&account)).unwrap();
    for entry in [
        image(0),
        Entry {
            id: [2; 16],
            kind: Kind::Image { image_bytes: 1 },
        },
        Entry {
            id: [2; 16],
            kind: Kind::Snapshot(SnapshotKey {
                commit: Commit {
                    store: [3; 16],
                    image: [2; 16],
                    image_bytes: BLOCK_SIZE as u64,
                    generation: 1,
                    root: Root::default(),
                    durable: 0,
                },
                end: 8192,
            }),
        },
    ] {
        assert_eq!(
            original
                .changed(Change::Insert(entry))
                .err()
                .unwrap()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }
    assert_eq!(account.usage().rejected, 0);
    for count in [
        u64::MAX,
        u64::MAX / ENTRY as u64,
        i64::MAX as u64 / ENTRY as u64,
    ] {
        assert!(encoded_size(count).is_err());
    }
    let mut exhausted = copy(&original);
    put64(exhausted.buffer.as_mut_slice(), 32, u64::MAX);
    exhausted.seal();
    assert!(exhausted.changed(Change::Insert(image(2))).is_err());
    assert_eq!(account.usage().current.bytes, BLOCK_SIZE);
}

#[test]
fn growth_holds_both_encoded_versions_and_inspection_scratch() {
    let account = metadata(3 * BLOCK_SIZE);
    let mut contents = Contents::empty([1; 16], Arc::clone(&account)).unwrap();
    for entry in (2..33).map(image) {
        contents = contents.changed(Change::Insert(entry)).unwrap();
    }
    assert_eq!(account.usage().current.bytes, BLOCK_SIZE);
    let next = contents.changed(Change::Insert(image(33))).unwrap();
    assert_eq!(account.usage().current.bytes, 3 * BLOCK_SIZE);
    assert!(next.changed(Change::Remove([2; 16])).is_err());
    drop(contents);
    let smaller = next.changed(Change::Remove([2; 16])).unwrap();
    assert_eq!(account.usage().current.bytes, 3 * BLOCK_SIZE);
    drop(smaller);
    let root = tempfile::tempdir().unwrap();
    let file = direct::open(&root.path().join("catalog"), true).unwrap();
    direct::write_bytes(&file, next.bytes(), 0).unwrap();
    assert!(Contents::read(&file, [1; 16], metadata(2 * BLOCK_SIZE)).is_err());
    let loaded = Contents::read(&file, [1; 16], metadata(3 * BLOCK_SIZE)).unwrap();
    assert_eq!(
        loaded.entries().collect::<Vec<_>>(),
        next.entries().collect::<Vec<_>>()
    );
    drop(next);
    assert_eq!(account.usage().current.bytes, 0);
}
