use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::FileExt;

use super::*;
use format::RequestId;

const IMAGE_BYTES: usize = 32 * BLOCK_SIZE;
const SEGMENT_BYTES: u64 = 2 * MAX_REQUEST_BYTES as u64;

fn config() -> Config {
    Config {
        store: [1; 16],
        image: [2; 16],
        image_bytes: IMAGE_BYTES as u64,
        segment_bytes: SEGMENT_BYTES,
    }
}

fn builder(serial: u64, offset: usize, value: u8) -> Builder {
    let mut builder = Builder::new(IMAGE_BYTES as u64, BLOCK_SIZE).unwrap();
    builder
        .write(
            RequestId {
                serial,
                attachment: 1,
                queue: 0,
                head: 0,
            },
            offset as u64,
            BLOCK_SIZE,
            |bytes| {
                bytes.fill(value);
                Ok(())
            },
        )
        .unwrap();
    builder
}

fn bytes(log: &mut Log) -> Vec<u8> {
    let mut buffer = AlignedBuffer::new(IMAGE_BYTES);
    log.read_into(0, &mut buffer).unwrap();
    buffer.as_slice().to_vec()
}

#[test]
fn randomized_operations_match_an_independent_image_through_cold_reopen() {
    for initial_seed in [1u64, 17, 0x20260911, 0xfeedbeef] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store");
        let mut log = Log::create(&path, config(), Limits::default()).unwrap();
        let mut expected = vec![0; IMAGE_BYTES];
        let mut seed = initial_seed;
        let mut sequence = 0;
        for serial in 1..=400 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let start = seed as usize % 32;
            let count = 1 + (seed >> 32) as usize % (32 - start);
            let range = start * BLOCK_SIZE..(start + count) * BLOCK_SIZE;
            match (seed >> 17) % 5 {
                0 | 1 => {
                    let value = (seed >> 8) as u8;
                    let mut builder = Builder::new(IMAGE_BYTES as u64, range.len()).unwrap();
                    builder
                        .write(
                            RequestId {
                                serial,
                                attachment: 1,
                                queue: 0,
                                head: 0,
                            },
                            range.start as u64,
                            range.len(),
                            |bytes| {
                                bytes.fill(value);
                                Ok(())
                            },
                        )
                        .unwrap();
                    let allocation = builder.allocation_address();
                    let batch = log.append(builder).unwrap();
                    assert_eq!(batch.allocation_address(), allocation);
                    expected[range].fill(value);
                    sequence += 1;
                }
                2 => {
                    let mut builder = Builder::new(IMAGE_BYTES as u64, 0).unwrap();
                    builder
                        .zero(
                            RequestId {
                                serial,
                                attachment: 1,
                                queue: 0,
                                head: 0,
                            },
                            range.start as u64,
                            range.len() as u64,
                        )
                        .unwrap();
                    log.append(builder).unwrap();
                    expected[range].fill(0);
                    sequence += 1;
                }
                3 => {
                    assert_eq!(log.flush().unwrap(), sequence);
                }
                _ => {
                    assert_eq!(bytes(&mut log), expected);
                }
            }
            assert_eq!(log.status().published, sequence);
            if serial % 41 == 0 {
                let epoch = log.status().epoch;
                drop(log);
                log = Log::open_with_expected_prefix(&path, Limits::default(), sequence).unwrap();
                assert_eq!(log.status().published, sequence);
                assert_eq!(log.status().durable, sequence);
                assert!(log.status().epoch > epoch);
            }
            assert_eq!(
                bytes(&mut log),
                expected,
                "seed {initial_seed}, operation {serial}"
            );
        }
    }
}

#[test]
fn packed_overlap_and_zero_precedence_preserve_surviving_payload_ranges() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let mut batch = Builder::new(IMAGE_BYTES as u64, 6 * BLOCK_SIZE).unwrap();
    let id = |serial| RequestId {
        serial,
        attachment: 1,
        queue: 0,
        head: 0,
    };
    batch
        .write(id(1), 0, 4 * BLOCK_SIZE, |bytes| {
            bytes.fill(1);
            Ok(())
        })
        .unwrap();
    batch
        .zero(id(2), BLOCK_SIZE as u64, 2 * BLOCK_SIZE as u64)
        .unwrap();
    batch
        .write(id(3), 2 * BLOCK_SIZE as u64, 2 * BLOCK_SIZE, |bytes| {
            bytes.fill(2);
            Ok(())
        })
        .unwrap();
    log.append(batch).unwrap();
    let mut expected = vec![0; IMAGE_BYTES];
    expected[..BLOCK_SIZE].fill(1);
    expected[2 * BLOCK_SIZE..4 * BLOCK_SIZE].fill(2);
    assert_eq!(bytes(&mut log), expected);
    drop(log);
    let mut log = Log::open(&path, Limits::default()).unwrap();
    assert_eq!(bytes(&mut log), expected);
}

#[test]
fn torn_terminal_batch_and_fence_are_archived_before_repair() {
    for tail in [
        1,
        BLOCK_SIZE - 1,
        BLOCK_SIZE,
        BLOCK_SIZE + 1,
        2 * BLOCK_SIZE - 1,
        2 * BLOCK_SIZE + 1,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store");
        let mut log = Log::create(&path, config(), Limits::default()).unwrap();
        log.append(builder(1, 0, 1)).unwrap();
        log.flush().unwrap();
        let boundary = log.offset;
        log.append(builder(2, 0, 2)).unwrap();
        log.flush().unwrap();
        let file = path.join(segment::name(1));
        drop(log);
        let original = fs::read(&file).unwrap();
        let cut = boundary + tail as u64;
        File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_len(cut)
            .unwrap();
        let mut recovered = Log::open_with_expected_prefix(&path, Limits::default(), 1).unwrap();
        let retained_second = tail > 2 * BLOCK_SIZE;
        assert_eq!(
            recovered.status().published,
            if retained_second { 2 } else { 1 }
        );
        assert_eq!(
            bytes(&mut recovered)[0],
            if retained_second { 2 } else { 1 }
        );
        let rejected_start = boundary as usize + if retained_second { 2 * BLOCK_SIZE } else { 0 };
        let archive = fs::read_dir(path.join("rejected"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(
            fs::read(archive).unwrap(),
            original[rejected_start..cut as usize]
        );
        assert_eq!(
            recovered.status().rejected_bytes,
            (cut as usize - rejected_start) as u64
        );
    }
}

#[test]
fn required_prefix_failure_does_not_change_files_or_accept_forged_payload_framing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let forged = Batch::fence(1, 9, 900).unwrap();
    let mut batch = Builder::new(IMAGE_BYTES as u64, BLOCK_SIZE).unwrap();
    batch
        .write(
            RequestId {
                serial: 1,
                attachment: 1,
                queue: 0,
                head: 0,
            },
            0,
            BLOCK_SIZE,
            |bytes| {
                bytes.copy_from_slice(forged.bytes());
                Ok(())
            },
        )
        .unwrap();
    log.append(batch).unwrap();
    log.flush().unwrap();
    drop(log);
    let file = path.join(segment::name(1));
    File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .write_all_at(&[0xff], BLOCK_SIZE as u64 + 56)
        .unwrap();
    let damaged = fs::read(&file).unwrap();
    assert!(matches!(
        Log::open_with_expected_prefix(&path, Limits::default(), 1),
        Err(Error::Prefix {
            recovered: 0,
            required: 1
        })
    ));
    assert_eq!(fs::read(&file).unwrap(), damaged);
    assert!(!path.join("rejected").exists());
    let mut recovered = Log::open(&path, Limits::default()).unwrap();
    assert_eq!(recovered.status().published, 0);
    assert_eq!(bytes(&mut recovered), vec![0; IMAGE_BYTES]);
}

#[test]
fn partial_write_and_sync_error_are_terminal_without_publishing_success() {
    for fault in [
        direct::faults::Fault::ShortWrite,
        direct::faults::Fault::Sync,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("store");
        let mut log = Log::create(&path, config(), Limits::default()).unwrap();
        log.append(builder(1, 0, 1)).unwrap();
        log.flush().unwrap();
        if fault == direct::faults::Fault::ShortWrite {
            direct::faults::inject(fault);
            assert!(log.append(builder(2, 0, 2)).is_err());
            assert_eq!(log.status().published, 1);
        } else {
            log.append(builder(2, 0, 2)).unwrap();
            direct::faults::inject(fault);
            assert!(log.flush().is_err());
            assert_eq!(log.status().published, 2);
        }
        assert!(log.status().failed);
        assert_eq!(log.status().durable, 1);
        assert!(matches!(log.append(builder(3, 0, 3)), Err(Error::Failed)));
        assert!(matches!(log.flush(), Err(Error::Failed)));
        assert!(matches!(
            log.read_into(0, &mut AlignedBuffer::new(BLOCK_SIZE)),
            Err(Error::Failed)
        ));
        drop(log);
        let mut reopened = Log::open_with_expected_prefix(&path, Limits::default(), 1).unwrap();
        assert_eq!(
            bytes(&mut reopened)[0],
            if fault == direct::faults::Fault::ShortWrite {
                1
            } else {
                2
            }
        );
    }
}

#[test]
fn segment_rotation_reserves_fence_space_and_capacity_failure_consumes_no_sequence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let limits = Limits {
        staging_bytes: 2 * SEGMENT_BYTES,
        ..Limits::default()
    };
    let mut log = Log::create(&path, config(), limits).unwrap();
    let mut accepted = 0;
    loop {
        match log.append(builder(accepted + 1, 0, 7)) {
            Ok(_) => accepted += 1,
            Err(Error::Capacity) => break,
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert!(accepted > 256);
    assert_eq!(log.status().segments, 2);
    assert!(log.status().allocated_bytes <= limits.staging_bytes);
    assert_eq!(log.status().published, accepted);
    assert_eq!(log.flush().unwrap(), accepted);
    assert_eq!(bytes(&mut log)[0], 7);
    drop(log);
    let reopened = Log::open_with_expected_prefix(&path, Limits::default(), accepted).unwrap();
    assert_eq!(reopened.status().published, accepted);
    assert_eq!(reopened.status().durable, accepted);
}

#[test]
fn immutable_payload_pin_retains_the_actual_file_lock_after_log_drop() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    log.append(builder(1, 0, 9)).unwrap();
    let payload = log
        .index
        .overlapping(0, 1)
        .next()
        .unwrap()
        .1
        .source
        .as_ref()
        .unwrap()
        .0
        .clone();
    let before = fs::read(path.join(segment::name(1))).unwrap();
    drop(log);
    assert!(Log::open(&path, Limits::default()).is_err());
    assert_eq!(fs::read(path.join(segment::name(1))).unwrap(), before);
    drop(payload);
    let mut recovered = Log::open(&path, Limits::default()).unwrap();
    assert_eq!(bytes(&mut recovered)[0], 9);
}

#[test]
fn recovery_never_reuses_a_rejected_segment_number() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    log.append(builder(1, 0, 1)).unwrap();
    log.flush().unwrap();
    drop(log);
    let incomplete = path.join(segment::name(8));
    let mut file = File::create(incomplete).unwrap();
    file.seek(SeekFrom::Start(100)).unwrap();
    file.write_all(&[1]).unwrap();
    file.sync_all().unwrap();
    let log = Log::open_with_expected_prefix(&path, Limits::default(), 1).unwrap();
    assert_eq!(log.current().header.number, 9);
    assert_eq!(log.current().header.epoch, 9);
    drop(log);
    let log = Log::open(&path, Limits::default()).unwrap();
    assert_eq!(log.current().header.number, 10);
}

#[test]
fn incompatible_builder_is_rejected_before_io_and_read_errors_stop_the_writer() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let initial_bytes = log.status().encoded_bytes;
    let mut wrong = Builder::new((2 * IMAGE_BYTES) as u64, 0).unwrap();
    wrong
        .zero(
            RequestId {
                serial: 1,
                attachment: 1,
                queue: 0,
                head: 0,
            },
            IMAGE_BYTES as u64,
            BLOCK_SIZE as u64,
        )
        .unwrap();
    assert!(log.append(wrong).is_err());
    assert_eq!(log.status().encoded_bytes, initial_bytes);
    assert_eq!(log.status().published, 0);
    log.append(builder(1, 0, 3)).unwrap();
    log.current().file.set_len(BLOCK_SIZE as u64).unwrap();
    assert!(
        log.read_into(0, &mut AlignedBuffer::new(BLOCK_SIZE))
            .is_err()
    );
    assert!(log.status().failed);
    assert!(matches!(log.flush(), Err(Error::Failed)));
}

#[test]
fn invalid_terminal_batch_discards_and_retains_all_later_segments() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    log.append(builder(1, 0, 1)).unwrap();
    log.flush().unwrap();
    log.rotate(Some(1)).unwrap();
    log.append(builder(2, 0, 2)).unwrap();
    log.flush().unwrap();
    drop(log);
    File::options()
        .write(true)
        .open(path.join(segment::name(1)))
        .unwrap()
        .write_all_at(&[255], 2 * BLOCK_SIZE as u64)
        .unwrap();
    let before = [1, 2].map(|number| fs::read(path.join(segment::name(number))).unwrap());
    assert!(matches!(
        Log::open_with_expected_prefix(&path, Limits::default(), 2),
        Err(Error::Prefix { .. })
    ));
    assert_eq!(
        [1, 2].map(|number| fs::read(path.join(segment::name(number))).unwrap()),
        before
    );
    let mut log = Log::open(&path, Limits::default()).unwrap();
    assert_eq!(log.current().header.number, 3);
    assert_eq!(bytes(&mut log), vec![0; IMAGE_BYTES]);
    assert!(!path.join(segment::name(2)).exists());
    let mut archives = fs::read_dir(path.join("rejected"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    archives.sort();
    assert_eq!(archives.len(), 2);
    assert_eq!(fs::read(&archives[0]).unwrap(), before[0][BLOCK_SIZE..]);
    assert_eq!(fs::read(&archives[1]).unwrap(), before[1]);
}

#[test]
fn a_partial_read_verifies_the_whole_original_write_before_returning_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("store");
    let mut log = Log::create(&path, config(), Limits::default()).unwrap();
    let mut batch = Builder::new(IMAGE_BYTES as u64, 2 * BLOCK_SIZE).unwrap();
    batch
        .write(
            RequestId {
                serial: 1,
                attachment: 1,
                queue: 0,
                head: 0,
            },
            0,
            2 * BLOCK_SIZE,
            |bytes| {
                bytes.fill(7);
                Ok(())
            },
        )
        .unwrap();
    log.append(batch).unwrap();
    let mut output = AlignedBuffer::new(BLOCK_SIZE);
    log.read_into(BLOCK_SIZE as u64, &mut output).unwrap();
    assert_eq!(output.as_slice(), &[7; BLOCK_SIZE]);
    // Damage a different portion of the same immutable WRITE.
    File::options()
        .write(true)
        .open(path.join(segment::name(1)))
        .unwrap()
        .write_all_at(&[3], 2 * BLOCK_SIZE as u64)
        .unwrap();
    assert!(log.read_into(BLOCK_SIZE as u64, &mut output).is_err());
    assert!(log.status().failed);
}

#[test]
fn index_budget_denial_precedes_creation_or_recovery_and_publication_reuses_slots() {
    use crate::budget::{Amount, Budget};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("log");
    let limits = Limits {
        intervals: 128,
        ..Limits::default()
    };
    let denied = Budget::new(Amount {
        bytes: 1024,
        requests: 0,
    });
    assert!(Log::create_with_metadata(&path, config(), limits, Arc::clone(&denied)).is_err());
    assert!(!path.exists());
    assert_eq!(denied.usage().current.bytes, 0);
    let metadata = Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    });
    let mut log =
        Log::create_with_metadata(&path, config(), limits, Arc::clone(&metadata)).unwrap();
    let charged = metadata.usage().current.bytes;
    let segment_metadata = log.status().segment_metadata_bytes;
    assert_eq!(
        charged,
        log.status().index_metadata_bytes + segment_metadata
    );
    let pins_denied = Budget::new(Amount {
        bytes: log.status().index_metadata_bytes,
        requests: 0,
    });
    let untouched = directory.path().join("pins-denied");
    assert!(
        Log::create_with_metadata(&untouched, config(), limits, Arc::clone(&pins_denied)).is_err()
    );
    assert!(!untouched.exists());
    assert_eq!(pins_denied.usage().current.bytes, 0);
    for block in 0..32 {
        log.append(builder(block as u64 + 1, block * BLOCK_SIZE, block as u8))
            .unwrap();
    }
    let old = log.read_plan(0, IMAGE_BYTES, 32).unwrap();
    for block in 0..32 {
        log.append(builder(block as u64 + 33, block * BLOCK_SIZE, 0xaa))
            .unwrap();
    }
    log.flush().unwrap();
    assert_eq!(metadata.usage().current.bytes, charged);
    assert_eq!(metadata.usage().peak.bytes, charged);
    assert_eq!(metadata.usage().admitted, 2);
    assert!(log.status().index_nodes_peak >= 3);
    drop(log);
    assert_eq!(metadata.usage().current.bytes, segment_metadata);
    let mut data = AlignedBuffer::new(IMAGE_BYTES);
    old.read_into(&mut data).unwrap();
    for (block, bytes) in data
        .as_slice()
        .as_chunks::<BLOCK_SIZE>()
        .0
        .iter()
        .enumerate()
    {
        assert!(bytes.iter().all(|byte| *byte == block as u8));
    }
    drop(old);
    assert_eq!(metadata.usage().current.bytes, 0);
    let file = path.join(segment::name(1));
    let before = fs::read(&file).unwrap();
    assert!(Log::inspect_with_metadata(&path, limits, Arc::clone(&denied)).is_err());
    assert_eq!(fs::read(&file).unwrap(), before);
    assert_eq!(denied.usage().current.bytes, 0);
    let recovery = Log::inspect_with_metadata(&path, limits, Arc::clone(&metadata)).unwrap();
    assert_eq!(recovery.status().published, 64);
    assert_eq!(metadata.usage().current.bytes, charged);
    drop(recovery);
    assert_eq!(metadata.usage().current.bytes, 0);
}
