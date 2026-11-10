use super::*;
use crate::{append::Publication, chunk::ChunkSize};

fn zero(f: &mut Fixture, start: u64, blocks: u64) {
    let mut batch = Builder::new(f.config.image_bytes, 0).unwrap();
    batch
        .zero(
            id(f.log().status().issued + 1),
            start * BLOCK_SIZE as u64,
            blocks * BLOCK_SIZE as u64,
        )
        .unwrap();
    f.log().append(batch).unwrap();
}

#[test]
fn partial_extent_preserves_base_excludes_later_wal_and_zeroes_tail() {
    let mut f = Fixture::sized(
        Config {
            image_bytes: 7 * BLOCK_SIZE as u64,
            ..CONFIG
        },
        ChunkSize::K16,
    );
    for block in 0..7 {
        write(f.log(), block, block as u8 + 1);
    }
    f.log().flush().unwrap();
    f.compact();
    f.reclaim(None);
    write(f.log(), 1, 11);
    zero(&mut f, 2, 3);
    f.log().flush().unwrap();
    let selected = f
        .log()
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap();
    write(f.log(), 0, 99); // P > captured E. Must never leak into the new D.
    let input = selected
        .load_with_store(&f.store.reader().unwrap())
        .unwrap();
    assert!(input.read_bytes() >= (2 * 16384) as u64);
    let output = f.output(input);
    f.log().publish_compaction(output).unwrap();
    let base = f.manifest.as_ref().unwrap().view().unwrap();
    let hash = base.tree().unwrap().get(0).unwrap().unwrap();
    let mut bytes = AlignedBuffer::new(16384);
    f.store
        .plan(hash)
        .unwrap()
        .unwrap()
        .load(bytes.as_mut_slice())
        .unwrap();
    assert_eq!(
        bytes.as_slice(),
        [
            vec![1; BLOCK_SIZE],
            vec![11; BLOCK_SIZE],
            vec![0; 2 * BLOCK_SIZE]
        ]
        .concat()
    );
    let hash = base.tree().unwrap().get(6).unwrap().unwrap();
    f.store
        .plan(hash)
        .unwrap()
        .unwrap()
        .load(bytes.as_mut_slice())
        .unwrap();
    assert_eq!(
        bytes.as_slice(),
        [
            vec![0; BLOCK_SIZE],
            vec![6; BLOCK_SIZE],
            vec![7; BLOCK_SIZE],
            vec![0; BLOCK_SIZE]
        ]
        .concat()
    );
    drop(base);
    f.fresh(10);
    let expected = [
        vec![99; BLOCK_SIZE],
        vec![11; BLOCK_SIZE],
        vec![0; 3 * BLOCK_SIZE],
        vec![6; BLOCK_SIZE],
        vec![7; BLOCK_SIZE],
    ]
    .concat();
    assert_eq!(
        f.read(
            &f.log
                .as_ref()
                .unwrap()
                .read_plan(0, expected.len(), 10)
                .unwrap()
        ),
        expected
    );
}

#[test]
fn both_arms_recover_every_compaction_publication_cut() {
    for size in [ChunkSize::K4, ChunkSize::K16] {
        for cut in [
            Publication::BeforeChunks,
            Publication::AfterChunks,
            Publication::AfterManifest,
        ] {
            let mut f = Fixture::sized(CONFIG, size);
            for block in 0..8 {
                write(f.log(), block, block as u8 + 1);
            }
            f.log().flush().unwrap();
            f.compact();
            write(f.log(), 1, 44);
            zero(&mut f, 3, 3);
            f.log().flush().unwrap();
            let input = f.input();
            let output = input
                .prepare(f.manifest.as_ref().unwrap())
                .unwrap()
                .write_with(&mut f.store, f.manifest.as_mut().unwrap(), |point, _| {
                    if point == cut {
                        Err(io::Error::other("simulated crash cut"))
                    } else {
                        Ok(())
                    }
                });
            assert!(output.is_err());
            drop(input);
            f.fresh(10);
            assert_eq!(
                f.image(),
                [
                    vec![1; BLOCK_SIZE],
                    vec![44; BLOCK_SIZE],
                    vec![3; BLOCK_SIZE],
                    vec![0; 3 * BLOCK_SIZE],
                    vec![7; BLOCK_SIZE],
                    vec![8; BLOCK_SIZE]
                ]
                .concat()
            );
            if f.log().status().compacted < f.log().status().durable {
                f.compact();
            }
            f.reclaim(None);
            f.fresh(10);
            assert_eq!(f.image()[BLOCK_SIZE], 44);
        }
    }
}

#[test]
fn both_arms_collection_keeps_old_roots_and_reads_then_rewrites() {
    for size in [ChunkSize::K4, ChunkSize::K16] {
        let mut f = Fixture::sized(CONFIG, size);
        for block in 0..8 {
            write(f.log(), block, block as u8 + 1);
        }
        f.log().flush().unwrap();
        f.compact();
        let old = f.manifest.as_ref().unwrap().view().unwrap();
        write(f.log(), 1, 55);
        f.log().flush().unwrap();
        f.compact();
        let current = f.manifest.as_ref().unwrap().view().unwrap();
        let hash = old.tree().unwrap().get(0).unwrap().unwrap();
        let pin = f.store.plan(hash).unwrap().unwrap();
        assert!(f.store.begin_collection().is_err());
        drop(pin);
        let mut collection = f.store.begin_collection().unwrap();
        for view in [&old, &current] {
            view.tree()
                .unwrap()
                .walk(|e| {
                    if let Some(h) = e.hash {
                        collection.mark(&h)?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        let mut sweep = collection.finish_marking().unwrap();
        while sweep.clean_next().unwrap().is_some() {}
        sweep.finish().unwrap();
        assert!(f.store.plan(hash).unwrap().is_some());
        drop(old);
        let mut collection = f.store.begin_collection().unwrap();
        current
            .tree()
            .unwrap()
            .walk(|e| {
                if let Some(h) = e.hash {
                    collection.mark(&h)?;
                }
                Ok(())
            })
            .unwrap();
        let mut sweep = collection.finish_marking().unwrap();
        while sweep.clean_next().unwrap().is_some() {}
        let collected = sweep.finish().unwrap();
        assert!(collected.chunks_copied > 0);
        assert!(f.store.status().collection_rewrite_bytes > 0);
        assert_eq!(f.image()[BLOCK_SIZE], 55);
        drop(current);
        f.fresh(9);
        assert_eq!(f.image()[BLOCK_SIZE], 55);
    }
}

#[test]
fn huge_zero_only_reconstructs_boundaries_and_maximum_batch_stays_bounded() {
    let mut f = Fixture::sized(
        Config {
            image_bytes: 1 << 34,
            ..CONFIG
        },
        ChunkSize::K16,
    );
    write(f.log(), 0, 7);
    f.log().flush().unwrap();
    f.compact();
    let mut batch = Builder::new(f.config.image_bytes, MAX_REQUEST_BYTES).unwrap();
    batch
        .write(id(2), 0, MAX_REQUEST_BYTES, |bytes| {
            bytes.fill(9);
            Ok(())
        })
        .unwrap();
    for n in 0..62 {
        batch
            .zero(
                id(n + 3),
                BLOCK_SIZE as u64,
                f.config.image_bytes - 2 * BLOCK_SIZE as u64,
            )
            .unwrap();
    }
    f.log().append(batch).unwrap();
    f.log().flush().unwrap();
    let input = f.input();
    assert_eq!(input.through(), 64);
    assert!(input.edits() <= 128);
    assert!(input.read_bytes() < 2 * MAX_REQUEST_BYTES as u64);
    let output = f.output(input);
    f.log().publish_compaction(output).unwrap();
    let plan = f.log().read_plan(0, 8 * BLOCK_SIZE, 64).unwrap();
    let bytes = f.read(&plan);
    assert!(bytes[..BLOCK_SIZE].iter().all(|b| *b == 9));
    assert!(bytes[BLOCK_SIZE..].iter().all(|b| *b == 0));
}

#[test]
fn both_arms_account_wal_assembly_store_and_collection_bytes_exactly() {
    let block = BLOCK_SIZE as u64;
    for size in [ChunkSize::K4, ChunkSize::K16] {
        let mut f = Fixture::sized(CONFIG, size);
        assert_eq!(f.log().status().guest_bytes_written, 0);
        assert_eq!(f.log().status().wal_bytes, block); // New segment header.
        for n in 0..4 {
            write(f.log(), n, n as u8 + 1);
        }
        f.log().flush().unwrap();
        assert_eq!(f.log().status().guest_bytes_written, 4 * block);
        assert_eq!(f.log().status().wal_bytes, 10 * block);
        let input = f.input();
        assert_eq!(input.read_bytes(), 8 * block); // Four headers and payloads.
        let output = f.output(input);
        f.log().publish_compaction(output).unwrap();
        assert_eq!(f.store.status().compaction_read_bytes, 8 * block);
        assert_eq!(f.store.status().store_bytes_appended, 6 * block);

        write(f.log(), 1, 44);
        zero(&mut f, 2, 1);
        f.log().flush().unwrap();
        assert_eq!(f.log().status().guest_bytes_written, 5 * block);
        assert_eq!(f.log().status().wal_bytes, 14 * block);
        let before = f.store.status();
        let input = f.input();
        // Previous fence + WRITE header/payload + ZERO header. 16 KiB adds
        // two manifest lookups and one full base chunk header/payload read.
        let loaded = if size == ChunkSize::K16 { 11 } else { 4 } * block;
        assert_eq!(input.read_bytes(), loaded);
        assert_eq!(
            f.store.status().compaction_read_bytes - before.compaction_read_bytes,
            loaded
        );
        let prepared = input.prepare(f.manifest.as_ref().unwrap()).unwrap();
        let preparation_reads = prepared.manifest_stats().old_page_reads * block;
        let output = prepared
            .write(&mut f.store, f.manifest.as_mut().unwrap())
            .unwrap();
        f.log().publish_compaction(output).unwrap();
        assert_eq!(
            f.store.status().compaction_read_bytes - before.compaction_read_bytes,
            loaded + preparation_reads
        );
        assert_eq!(
            f.store.status().store_bytes_appended - before.store_bytes_appended,
            block + size.bytes() as u64
        );
        drop(input);

        let before = f.store.status();
        let mut collection = f.store.begin_collection().unwrap();
        f.manifest
            .as_ref()
            .unwrap()
            .tree()
            .unwrap()
            .walk(|extent| {
                if let Some(hash) = extent.hash {
                    collection.mark(&hash)?;
                }
                Ok(())
            })
            .unwrap();
        let mut sweep = collection.finish_marking().unwrap();
        while sweep.clean_next().unwrap().is_some() {}
        let collected = sweep.finish().unwrap();
        let rewrite = collected.encoded_bytes_copied + block; // One new segment.
        let after = f.store.status();
        assert_eq!(after.collection_rewrite_bytes, rewrite);
        assert_eq!(
            after.store_bytes_appended - before.store_bytes_appended,
            rewrite
        );
        assert_eq!(after.compaction_read_bytes, before.compaction_read_bytes);
        f.fresh(6);
        assert_eq!(f.log().status().guest_bytes_written, 0); // Old replay is excluded.
        assert_eq!(f.log().status().wal_bytes, 2 * block); // Fresh header and fence.
    }
}
