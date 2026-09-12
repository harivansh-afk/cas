use super::*;

#[test]
fn planning_omits_covered_chunks_but_preserves_partial_zero_order_and_prefix() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    write(f.log(), 1, 20);
    let mut zero = Builder::new(CONFIG.image_bytes, 0).unwrap();
    zero.zero(id(3), 0, 2 * BLOCK_SIZE as u64).unwrap();
    f.log().append(zero).unwrap();
    write(f.log(), 0, 30);
    f.log().flush().unwrap();
    let input = f.input();
    let prepared = input.prepare(f.manifest.as_ref().unwrap()).unwrap();
    assert_eq!(prepared.through(), 4);
    assert_eq!(prepared.chunk_count(), 1);
    assert!(prepared.manifest_bytes().is_multiple_of(BLOCK_SIZE));
    assert_eq!(f.store.status().chunks, 0);
    assert_eq!(f.manifest.as_ref().unwrap().current().durable, 0);
    // Outside the captured prefix: it remains staged after D=4 publication.
    write(f.log(), 0, 40);
    f.log().flush().unwrap();
    let output = prepared
        .write(&mut f.store, f.manifest.as_mut().unwrap())
        .unwrap();
    f.log().publish_compaction(output).unwrap();
    assert_eq!(f.store.status().chunks, 1);
    for value in [10, 20, 40] {
        let bytes = [value; BLOCK_SIZE];
        assert!(
            f.store
                .plan(Chunk::new(&bytes).unwrap().hash())
                .unwrap()
                .is_none()
        );
    }
    let expected = [vec![40; BLOCK_SIZE], vec![0; 7 * BLOCK_SIZE]].concat();
    assert_eq!(f.image(), expected);
    drop(input);
    f.fresh(5);
    assert_eq!(f.image(), expected);
}

#[test]
fn metadata_denial_and_manifest_read_failure_precede_all_chunk_output() {
    for read_failure in [false, true] {
        let mut f = Fixture::new();
        if read_failure {
            write(f.log(), 0, 10);
            f.log().flush().unwrap();
            f.compact();
        }
        write(f.log(), 1, 20);
        f.log().flush().unwrap();
        let metadata = if read_failure {
            memory()
        } else {
            Budget::new(Amount {
                bytes: 64 * 1024,
                requests: 0,
            })
        };
        let input = f
            .log()
            .select_compaction(Arc::clone(&metadata), memory())
            .unwrap()
            .unwrap()
            .load()
            .unwrap();
        let before = f.store.status();
        let prior = f.manifest.as_ref().unwrap().current();
        if read_failure {
            faults::inject(Fault::Read);
        }
        assert!(
            input
                .write(&mut f.store, f.manifest.as_mut().unwrap())
                .is_err()
        );
        let after = f.store.status();
        assert_eq!(
            (after.chunks, after.encoded_bytes),
            (before.chunks, before.encoded_bytes)
        );
        assert!(!after.failed);
        assert_eq!(f.manifest.as_ref().unwrap().current(), prior);
        assert!(!f.manifest.as_ref().unwrap().failed());
        assert_eq!(metadata.usage().current.bytes, 0);
        let prefix = f.log().status().published;
        f.fresh(prefix);
        assert_eq!(&f.image()[BLOCK_SIZE..2 * BLOCK_SIZE], &[20; BLOCK_SIZE]);
    }
}

#[test]
fn prepared_output_rejects_another_file_or_new_root_before_chunk_publication() {
    let mut f = Fixture::new();
    write(f.log(), 0, 10);
    write(f.log(), 1, 20);
    f.log().flush().unwrap();
    let input = f.input();
    let mut other = Fixture::new();
    let prepared = input.prepare(f.manifest.as_ref().unwrap()).unwrap();
    assert!(
        prepared
            .write(&mut f.store, other.manifest.as_mut().unwrap())
            .is_err()
    );
    assert_eq!(f.store.status().chunks, 0);
    let prepared = input.prepare(f.manifest.as_ref().unwrap()).unwrap();
    let hash = f.put(10);
    f.commit(
        1,
        &[Extent {
            start: 0,
            end: 1,
            hash: Some(hash),
        }],
    );
    assert!(
        prepared
            .write(&mut f.store, f.manifest.as_mut().unwrap())
            .is_err()
    );
    assert_eq!(f.store.status().chunks, 1);
    drop(input);
    f.fresh(2);
    assert_eq!(
        f.image(),
        [
            vec![10; BLOCK_SIZE],
            vec![20; BLOCK_SIZE],
            vec![0; 6 * BLOCK_SIZE]
        ]
        .concat()
    );
}
