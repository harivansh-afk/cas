use super::*;
use crate::append::{Compacted, Input, ReclaimStats};
mod planning;

impl Fixture {
    fn input(&self) -> Input {
        self.log
            .as_ref()
            .unwrap()
            .select_compaction(memory(), memory())
            .unwrap()
            .unwrap()
            .load()
            .unwrap()
    }
    fn output(&mut self, input: Input) -> Compacted {
        input
            .write(&mut self.store, self.manifest.as_mut().unwrap())
            .unwrap()
    }
    fn compact(&mut self) {
        let input = self.input();
        let output = self.output(input);
        self.log().publish_compaction(output).unwrap();
    }
    fn reclaim(&mut self, oldest: Option<u64>) -> ReclaimStats {
        let work = self.log().select_reclamation(oldest, memory()).unwrap();
        let result = work.run().unwrap();
        self.log().apply_reclamation(result).unwrap()
    }
    fn image(&self) -> Vec<u8> {
        let log = self.log.as_ref().unwrap();
        self.read(
            &log.read_plan(0, CONFIG.image_bytes as usize, log.status().published)
                .unwrap(),
        )
    }
}

#[test]
fn reclamation_accepts_an_admitted_identity_before_log_assignment() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    f.compact();
    assert!(f.log().select_reclamation(Some(0), memory()).is_err());
    let selected = f.log().select_reclamation(Some(2), memory()).unwrap();
    // The carrier has admitted mutation 2, but it was not in the selected spans.
    write(f.log(), 0, 20);
    f.log().flush().unwrap();
    let reclaimed = selected.run().unwrap();
    let stats = f.log().apply_reclamation(reclaimed).unwrap();
    assert!(stats.punch_requested_bytes > 0);
    f.fresh(2);
    assert_eq!(
        f.image(),
        [vec![20; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
    );
}

#[test]
fn publication_keeps_newer_overwrites_and_reclaim_waits_for_original_readers() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    write(f.log(), 1, 20);
    f.log().flush().unwrap();
    let old = f.log().read_plan(0, BLOCK_SIZE, 2).unwrap();
    let selection = f
        .log()
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap();
    assert_eq!(f.log().status().read_pins, 2); // One reader plus the scan pin.
    write(f.log(), 0, 30);
    f.log().flush().unwrap();
    let input = selection.load().unwrap();
    assert_eq!(
        (input.through(), input.edits(), input.payload_bytes()),
        (2, 2, 2 * BLOCK_SIZE)
    );
    assert_eq!(f.log().status().read_pins, 1);
    let mut boundaries = Vec::new();
    let output = input
        .prepare(f.manifest.as_ref().unwrap())
        .unwrap()
        .write_with(
            &mut f.store,
            f.manifest.as_mut().unwrap(),
            |point, durable| {
                boundaries.push((point, durable));
                Ok(())
            },
        )
        .unwrap();
    drop(input);
    use crate::append::Publication;
    assert_eq!(
        boundaries,
        [
            (Publication::BeforeChunks, 0),
            (Publication::AfterChunks, 0),
            (Publication::AfterManifest, 2),
        ]
    );
    assert_eq!(f.log().status().compacted, 0); // Synced file is not yet image publication.
    assert_eq!(output.durable(), 2);
    f.log().publish_compaction(output).unwrap();
    assert_eq!(
        (f.log().status().compacted, f.log().status().intervals),
        (2, 1)
    );
    let stats = f.reclaim(None);
    assert_eq!(stats.pinned_batches, 1);
    assert_eq!(f.read(&old), vec![10; BLOCK_SIZE]);
    assert_eq!(
        f.image(),
        [
            vec![30; BLOCK_SIZE],
            vec![20; BLOCK_SIZE],
            vec![0; 6 * BLOCK_SIZE]
        ]
        .concat()
    );
    drop(old);
    assert_eq!(f.log().status().read_pins, 0);
    let mut operations = Vec::new();
    let reclaimed = f
        .log()
        .select_reclamation(None, memory())
        .unwrap()
        .run_with(|operation| {
            operations.push(operation);
            Ok(())
        })
        .unwrap();
    f.log().apply_reclamation(reclaimed).unwrap();
    assert!(operations.iter().any(|operation| matches!(operation,
        crate::append::ReclaimOperation::Punch { bytes, .. } if *bytes > 0)));
    f.fresh(3);
    assert_eq!(
        f.image(),
        [
            vec![30; BLOCK_SIZE],
            vec![20; BLOCK_SIZE],
            vec![0; 6 * BLOCK_SIZE]
        ]
        .concat()
    );
}

#[test]
fn selection_uses_e_and_a_failed_image_cannot_publish_completed_output() {
    let mut f = Fixture::new();
    assert!(
        f.log()
            .select_compaction(memory(), memory())
            .unwrap()
            .is_none()
    );
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    write(f.log(), 0, 20);
    let input = f.input();
    assert_eq!(input.through(), 1);
    let output = f.output(input);
    f.log().fail();
    assert!(f.log().publish_compaction(output).is_err());
    assert_eq!(f.log().status().compacted, 0);
    f.fresh(2);
    assert_eq!(f.log().status().compacted, 1); // Explicit recovery adopts the synced root.
    assert_eq!(
        f.image(),
        [vec![20; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
    );
}

#[test]
fn whole_batch_selection_obeys_payload_and_edit_limits() {
    let mut f = Fixture::new();
    for sequence in 1..=257 {
        write(f.log(), sequence % 8, (1 + sequence % 250) as u8);
    }
    f.log().flush().unwrap();
    let expected = f.image();
    let input = f.input();
    assert_eq!(
        (input.through(), input.edits(), input.payload_bytes()),
        (256, 256, MAX_REQUEST_BYTES)
    );
    let output = f.output(input);
    f.log().publish_compaction(output).unwrap();
    assert_eq!(f.image(), expected);
    let input = f.input();
    assert_eq!((input.through(), input.edits()), (257, 1));
    let output = f.output(input);
    f.log().publish_compaction(output).unwrap();
    assert_eq!(f.image(), expected);
    drop(f);

    let mut f = Fixture::new();
    let mut sequence = 0;
    for count in [63, 63, 63, 63, 63, 3, 1] {
        let mut batch = Builder::new(CONFIG.image_bytes, 0).unwrap();
        for _ in 0..count {
            sequence += 1;
            batch.zero(id(sequence), 0, CONFIG.image_bytes).unwrap();
        }
        f.log().append(batch).unwrap();
    }
    f.log().flush().unwrap();
    let input = f.input();
    assert_eq!(
        (input.through(), input.edits(), input.payload_bytes()),
        (318, 318, 0)
    );
    let output = f.output(input);
    f.log().publish_compaction(output).unwrap();
    assert_eq!(f.log().status().intervals, 1); // Newer ZERO still owns the interval.
    f.compact();
    assert_eq!(f.log().status().compacted, 319);
    assert_eq!(f.store.status().chunks, 0);
}

#[test]
fn retired_segments_wait_for_live_identities_and_held_scan_owners() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    f.log().flush().unwrap();
    let held = f
        .log()
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap();
    f.compact();
    f.log().rollover().unwrap();
    assert_eq!(f.reclaim(None).removed_segments, 0);
    drop(held);
    assert_eq!(f.reclaim(Some(1)).removed_segments, 0);
    assert!(f.path().join(segment::name(1)).exists());
    assert_eq!(f.reclaim(None).removed_segments, 1);
    assert!(!f.path().join(segment::name(1)).exists());
    assert_eq!(f.log().status().segments, 1);
    f.fresh(1);
    assert_eq!(
        f.image(),
        [vec![10; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
    );
}

#[test]
fn repeated_overwrite_zero_rotation_and_reopen_match_an_independent_image() {
    let mut f = Fixture::new();
    let mut oracle = [0u8; CONFIG.image_bytes as usize];
    let mut seed = 19u64;
    for cycle in 0..24 {
        for _ in 0..17 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let block = (seed >> 32) as usize % 8;
            if seed.is_multiple_of(5) {
                let length = 8 - block;
                let sequence = f.log().status().issued + 1;
                let mut batch = Builder::new(CONFIG.image_bytes, 0).unwrap();
                batch
                    .zero(
                        id(sequence),
                        (block * BLOCK_SIZE) as u64,
                        (length * BLOCK_SIZE) as u64,
                    )
                    .unwrap();
                f.log().append(batch).unwrap();
                oracle[block * BLOCK_SIZE..].fill(0);
            } else {
                let value = seed as u8;
                write(f.log(), block as u64, value);
                oracle[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE].fill(value);
            }
        }
        f.log().flush().unwrap();
        f.compact();
        assert_eq!(f.image(), oracle);
        f.log().rollover().unwrap();
        f.reclaim(None);
        assert_eq!(f.log().status().segments, 1);
        assert!(f.log().status().allocated_bytes <= CONFIG.segment_bytes);
        if cycle % 6 == 0 {
            let required = f.log().status().published;
            f.fresh(required);
            assert_eq!(f.image(), oracle);
        }
    }
}

#[test]
fn failed_chunk_or_manifest_sync_never_produces_a_publishable_receipt() {
    for manifest_failure in [false, true] {
        let mut f = Fixture::new();
        write(f.log(), 0, 10);
        f.log().flush().unwrap();
        if manifest_failure {
            f.put(10);
        } // Existing durable chunk isolates manifest sync.
        let input = f.input();
        faults::inject(Fault::Sync);
        assert!(
            input
                .write(&mut f.store, f.manifest.as_mut().unwrap())
                .is_err()
        );
        assert_eq!(f.log().status().compacted, 0);
        assert_eq!(
            f.image(),
            [vec![10; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
        );
        assert!(f.path().join(segment::name(1)).exists());
        if manifest_failure {
            assert!(f.manifest.as_ref().unwrap().failed());
            f.fresh(1); // Survived complete COMMIT is validated/synced before WAL repair.
            assert_eq!(f.log().status().compacted, 1);
        } else {
            assert!(f.store.status().failed);
            assert_eq!(f.manifest.as_ref().unwrap().current().durable, 0);
        }
    }
}

#[test]
fn restart_at_each_native_publication_boundary_preserves_the_image() {
    for boundary in 0..6 {
        let mut f = Fixture::new();
        write(f.log(), 0, 71);
        f.log().flush().unwrap();
        let input = f.input();
        if boundary == 0 {
            drop(input);
        } else if boundary == 1 {
            f.put(71);
            drop(input);
        } else {
            let output = f.output(input);
            if boundary == 2 {
                drop(output);
            } else {
                f.log().publish_compaction(output).unwrap();
                if boundary >= 4 {
                    if boundary == 5 {
                        f.log().rollover().unwrap();
                    }
                    let reclaimed = f
                        .log()
                        .select_reclamation(None, memory())
                        .unwrap()
                        .run()
                        .unwrap();
                    drop(reclaimed); // Replacement before in-memory result application.
                }
            }
        }
        f.fresh(1);
        assert_eq!(f.log().status().compacted, u64::from(boundary >= 2));
        assert_eq!(
            f.image(),
            [vec![71; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
        );
    }
}

#[test]
fn enospc_and_short_output_leave_the_prior_root_and_original_wal_available() {
    for manifest_failure in [false, true] {
        for fault in [Fault::Allocate, Fault::ShortWrite] {
            let mut f = Fixture::new();
            f.put(if manifest_failure { 71 } else { 99 }); // Avoid targeting a one-page creation header.
            write(f.log(), 0, 71);
            f.log().flush().unwrap();
            let before = snapshot(&f.path());
            let input = f.input();
            faults::inject(fault);
            assert!(
                input
                    .write(&mut f.store, f.manifest.as_mut().unwrap())
                    .is_err()
            );
            assert_eq!(f.log().status().compacted, 0);
            assert_eq!(f.manifest.as_ref().unwrap().current().durable, 0);
            assert_eq!(snapshot(&f.path()), before);
            if manifest_failure {
                f.fresh(1);
                assert_eq!(f.log().status().compacted, 0);
                assert_eq!(
                    f.image(),
                    [vec![71; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
                );
            } else {
                assert!(f.store.status().failed);
            }
        }
    }
}

#[test]
fn read_and_metadata_denial_release_scan_pins_without_changing_d() {
    let mut f = Fixture::new();
    write(f.log(), 0, 71);
    f.log().flush().unwrap();
    let before = snapshot(&f.path());
    let empty = Budget::new(Amount::default());
    assert!(
        f.log()
            .select_compaction(Arc::clone(&empty), memory())
            .is_err()
    );
    let selected = f
        .log()
        .select_compaction(memory(), Arc::clone(&empty))
        .unwrap()
        .unwrap();
    assert!(selected.load().is_err());
    assert_eq!(f.log().status().read_pins, 0);
    let selected = f
        .log()
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap();
    faults::inject(Fault::Read);
    assert!(selected.load().is_err());
    assert_eq!(f.log().status().read_pins, 0);
    let small = Budget::new(Amount {
        bytes: 16 * 1024,
        requests: 0,
    });
    let input = f
        .log()
        .select_compaction(small, memory())
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    assert!(
        input
            .write(&mut f.store, f.manifest.as_mut().unwrap())
            .is_err()
    );
    assert_eq!(f.log().status().compacted, 0);
    assert_eq!(f.manifest.as_ref().unwrap().current().durable, 0);
    assert!(!f.manifest.as_ref().unwrap().failed());
    assert_eq!(snapshot(&f.path()), before);
}

#[test]
fn reclaim_errors_leave_durable_root_recovery_safe() {
    for fault in [Fault::Punch, Fault::Sync] {
        let mut f = Fixture::new();
        write(f.log(), 0, 71);
        f.log().flush().unwrap();
        f.compact();
        let reclaim = f.log().select_reclamation(None, memory()).unwrap();
        faults::inject(fault);
        assert!(reclaim.run().is_err());
        f.log().fail(); // The background error must enter the image completion gate.
        f.fresh(1);
        assert_eq!(f.log().status().compacted, 1);
        assert_eq!(
            f.image(),
            [vec![71; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat()
        );
    }
}

#[test]
fn a_reader_on_another_thread_pins_the_whole_original_batch() {
    let mut f = Fixture::new();
    let mut batch = Builder::new(CONFIG.image_bytes, CONFIG.image_bytes as usize).unwrap();
    batch
        .write(id(1), 0, CONFIG.image_bytes as usize, |bytes| {
            for (block, bytes) in bytes.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
                bytes.fill((block + 1) as u8);
            }
            Ok(())
        })
        .unwrap();
    f.log().append(batch).unwrap();
    f.log().flush().unwrap();
    let plan = f
        .log()
        .read_plan(4 * BLOCK_SIZE as u64, BLOCK_SIZE, 1)
        .unwrap();
    let (release, wait) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        wait.recv().unwrap();
        let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
        plan.read_into(&mut bytes).unwrap();
        assert_eq!(bytes.as_slice(), &[5; BLOCK_SIZE]);
    });
    f.compact();
    assert_eq!(f.reclaim(None).pinned_batches, 1);
    release.send(()).unwrap();
    reader.join().unwrap();
    assert_eq!(f.log().status().read_pins, 0);
    assert_eq!(f.reclaim(None).pinned_batches, 0);
    f.fresh(1);
    let actual = f.image();
    for (block, bytes) in actual.as_chunks::<BLOCK_SIZE>().0.iter().enumerate() {
        assert_eq!(bytes, &[(block + 1) as u8; BLOCK_SIZE]);
    }
}

#[test]
fn unique_live_data_crosses_automatic_wal_rotations_and_reclamation() {
    const CYCLES: usize = 20;
    let mut f = Fixture::with_config(Config {
        image_bytes: (CYCLES * MAX_REQUEST_BYTES) as u64,
        ..CONFIG
    });
    let mut oracle = vec![0u8; CYCLES * MAX_REQUEST_BYTES];
    let mut removed = 0;
    for cycle in 0..CYCLES {
        for request in 0..16 {
            let offset = cycle * MAX_REQUEST_BYTES + request * 16 * BLOCK_SIZE;
            let sequence = f.log().status().issued + 1;
            let mut batch = Builder::new(f.config.image_bytes, 16 * BLOCK_SIZE).unwrap();
            let expected = &mut oracle[offset..offset + 16 * BLOCK_SIZE];
            for (relative, block) in expected
                .as_chunks_mut::<BLOCK_SIZE>()
                .0
                .iter_mut()
                .enumerate()
            {
                let absolute = offset / BLOCK_SIZE + relative;
                block.fill((absolute % 251) as u8);
                block[..8].copy_from_slice(&(absolute as u64 + 1).to_le_bytes());
            }
            batch
                .write(id(sequence), offset as u64, expected.len(), |bytes| {
                    bytes.copy_from_slice(expected);
                    Ok(())
                })
                .unwrap();
            f.log().append(batch).unwrap();
        }
        f.log().flush().unwrap();
        f.compact();
        removed += f.reclaim(None).removed_segments;
        assert_eq!(f.log().status().segments, 1);
        assert!(f.log().status().allocated_bytes <= CONFIG.segment_bytes);
        let plan = f
            .log()
            .read_plan(
                (cycle * MAX_REQUEST_BYTES) as u64,
                MAX_REQUEST_BYTES,
                (cycle as u64 + 1) * 16,
            )
            .unwrap();
        assert_eq!(
            f.read(&plan),
            oracle[cycle * MAX_REQUEST_BYTES..(cycle + 1) * MAX_REQUEST_BYTES]
        );
    }
    assert!(removed >= 10);
    assert_eq!(
        f.store.status().chunks,
        CYCLES * MAX_REQUEST_BYTES / BLOCK_SIZE
    );
    f.fresh((CYCLES * 16) as u64);
    for offset in (0..oracle.len()).step_by(MAX_REQUEST_BYTES) {
        let plan = f
            .log()
            .read_plan(offset as u64, MAX_REQUEST_BYTES, (CYCLES * 16) as u64)
            .unwrap();
        assert_eq!(f.read(&plan), oracle[offset..offset + MAX_REQUEST_BYTES]);
    }
}

#[test]
fn two_private_manifests_share_chunks_and_zeroing_one_preserves_the_other() {
    let mut f = Fixture::new();
    write(f.log(), 0, 71);
    f.log().flush().unwrap();
    f.compact();
    let config = Config {
        image: [3; 16],
        ..CONFIG
    };
    let path = staging(&f.tickets, config.image)
        .parent()
        .unwrap()
        .to_owned();
    fs::create_dir(&path).unwrap();
    File::open(path.parent().unwrap())
        .unwrap()
        .sync_all()
        .unwrap();
    let mut manifest = Manifest::create(
        &path,
        Identity {
            image: config.image,
            ..Fixture::identity()
        },
        memory(),
    )
    .unwrap();
    let mut second = Log::create_shared(
        Arc::clone(&f.tickets),
        config,
        Limits::default(),
        memory(),
        manifest.view().unwrap(),
    )
    .unwrap();
    write(&mut second, 0, 71);
    second.flush().unwrap();
    let input = second
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    let output = input.write(&mut f.store, &mut manifest).unwrap();
    second.publish_compaction(output).unwrap();
    assert_eq!(f.store.status().chunks, 1);
    let mut zero = Builder::new(CONFIG.image_bytes, 0).unwrap();
    zero.zero(id(2), 0, CONFIG.image_bytes).unwrap();
    f.log().append(zero).unwrap();
    f.log().flush().unwrap();
    f.compact();
    f.reclaim(None);
    assert_eq!(f.image(), vec![0; CONFIG.image_bytes as usize]);
    assert_eq!(
        f.read(&second.read_plan(0, BLOCK_SIZE, 1).unwrap()),
        vec![71; BLOCK_SIZE]
    );
    assert_eq!(f.store.status().chunks, 1);
}
