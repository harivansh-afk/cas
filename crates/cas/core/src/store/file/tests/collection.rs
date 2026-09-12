use super::*;

mod failures;
mod physical;
mod roots;

fn collect(store: &mut Store, live: &[[u8; BLOCK_SIZE]]) -> Collected {
    let mut collection = store.begin_collection().unwrap();
    for block in live {
        collection.mark(&hash(block)).unwrap();
    }
    let mut sweep = collection.finish_marking().unwrap();
    while sweep.clean_next().unwrap().is_some() {}
    sweep.finish().unwrap()
}

fn blocks(first: u8, last: u8) -> std::vec::Vec<[u8; BLOCK_SIZE]> {
    (first..=last).map(|byte| [byte; BLOCK_SIZE]).collect()
}

fn assert_tail_free(store: &Store) {
    let state = store.shared.lock();
    for segment in &state.segments {
        assert_eq!(segment.file.metadata().unwrap().len(), segment.end);
        assert!(
            direct::next_extent(&segment.file, segment.end)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn mixed_dead_and_live_segments_preserve_oracles_and_reopen() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    let all = blocks(1, 130);
    for batch in all.chunks(MAX_CHUNKS) {
        insert(&mut store, batch).unwrap();
    }
    let live = [all[0], all[2], all[61]]
        .into_iter()
        .chain(all[63..126].iter().copied())
        .collect::<std::vec::Vec<_>>();
    let reader = store.reader().unwrap();
    let mut marking = store.begin_collection().unwrap();
    for block in &live {
        marking.mark(&hash(block)).unwrap();
        marking.mark(&hash(block)).unwrap(); // Shared roots do not duplicate live counts.
    }
    assert!(matches!(reader.plan(hash(&live[0])), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
    let mut sweep = marking.finish_marking().unwrap();
    assert_eq!(
        sweep.next_victim().unwrap().destination_bytes,
        CONFIG.segment_bytes
    );
    assert_eq!(sweep.clean_next().unwrap().unwrap().chunks_copied, 3);
    assert_eq!(sweep.next_victim().unwrap().destination_bytes, 0);
    assert_eq!(sweep.clean_next().unwrap().unwrap().segments_removed, 0);
    assert_eq!(sweep.clean_next().unwrap().unwrap().segments_removed, 1);
    assert!(sweep.next_victim().is_none());
    let total = sweep.finish().unwrap();
    assert_eq!((total.chunks_copied, total.segments_removed), (3, 2));
    assert_eq!((store.status().chunks, store.status().segments), (66, 2));
    assert!(!reader.status().collecting);
    for block in &live {
        read(&store, block);
    }
    for block in all.iter().filter(|block| !live.contains(block)) {
        assert!(reader.plan(hash(block)).unwrap().is_none());
    }
    assert_tail_free(&store);
    drop((reader, store));
    let mut store = inspect(root.path()).unwrap().recover().unwrap();
    assert_eq!(store.status().chunks, live.len());
    for block in &live {
        read(&store, block);
    }
    assert_eq!(collect(&mut store, &live).chunks_copied, 0);
}

#[test]
fn multiple_victims_copy_multiple_batches_into_one_fresh_segment_each() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    let all = blocks(1, 180);
    for batch in all.chunks(20) {
        insert(&mut store, batch).unwrap();
    }
    assert_eq!(store.status().segments, 3);
    let live = all
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 2 == 0)
        .map(|(_, block)| *block)
        .collect::<std::vec::Vec<_>>();
    let result = collect(&mut store, &live);
    assert_eq!((result.segments_removed, result.chunks_copied), (3, 90));
    assert_eq!(result.encoded_bytes_copied, (99 * BLOCK_SIZE) as u64);
    assert_eq!(store.shared.tickets.status().highest, 6);
    assert_eq!(store.status().segments, 3);
    assert_tail_free(&store);
    drop(store);
    let mut store = inspect(root.path()).unwrap().recover().unwrap();
    for block in &live {
        read(&store, block);
    }
    insert(&mut store, &[[222; BLOCK_SIZE]]).unwrap();
    assert_eq!(store.shared.tickets.status().highest, 6); // Append only at never-used EOF.
    read(&store, &[222; BLOCK_SIZE]);
}

#[test]
fn greatest_dead_ticket_keeps_a_sealed_header_until_a_newer_name_exists() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[1; BLOCK_SIZE], [2; BLOCK_SIZE]]).unwrap();
    let path = root.path().join("chunks").join(segments::name(1));
    let header = fs::read(&path).unwrap()[..BLOCK_SIZE].to_vec();
    let result = collect(&mut store, &[]);
    assert_eq!((result.headers_retained, result.segments_removed), (1, 0));
    assert_eq!(fs::read(&path).unwrap(), header);
    assert_eq!(store.status().chunks, 0);
    assert_tail_free(&store);
    insert(&mut store, &[[3; BLOCK_SIZE]]).unwrap();
    assert_eq!(
        store
            .plan(hash(&[3; BLOCK_SIZE]))
            .unwrap()
            .unwrap()
            .address()
            .segment(),
        2
    );
    assert_eq!(collect(&mut store, &[[3; BLOCK_SIZE]]).segments_removed, 1);
    assert!(!path.exists());
    drop(store);
    let mut store = inspect(root.path()).unwrap().recover().unwrap();
    assert_eq!(store.shared.tickets.status().highest, 2);
    read(&store, &[3; BLOCK_SIZE]);
    collect(&mut store, &[]);
    drop(store);
    let store = inspect(root.path()).unwrap().recover().unwrap();
    assert_eq!((store.status().chunks, store.status().segments), (0, 1));
    assert_eq!(store.shared.tickets.status().highest, 2);
    assert_tail_free(&store);
}

#[test]
fn actual_read_pins_and_memory_denial_precede_collection_changes() {
    let root = tempfile::tempdir().unwrap();
    let mut store = create(root.path());
    insert(&mut store, &[[7; BLOCK_SIZE]]).unwrap();
    let read = store.plan(hash(&[7; BLOCK_SIZE])).unwrap().unwrap();
    assert!(matches!(store.begin_collection(), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
    let mut page = AlignedBuffer::new(BLOCK_SIZE);
    direct::read_bytes(read.file(), page.as_mut_slice(), read.header_offset()).unwrap();
    let payload = read.payload(page.as_slice()).unwrap();
    drop(read);
    assert!(matches!(store.begin_collection(), Err(e) if e.kind() == io::ErrorKind::WouldBlock));
    drop(payload);
    let file = root.path().join("chunks").join(segments::name(1));
    let original = fs::read(&file).unwrap();
    for budget in [
        Arc::clone(&store.shared.metadata),
        Arc::clone(&store.io_memory),
    ] {
        let capacity = if Arc::ptr_eq(&budget, &store.io_memory) {
            1024 * 1024
        } else {
            128 * 1024 * 1024
        };
        let held = budget
            .reserve(Amount {
                bytes: capacity - budget.usage().current.bytes,
                requests: 0,
            })
            .unwrap();
        assert!(
            matches!(store.begin_collection(), Err(e) if e.kind() == io::ErrorKind::OutOfMemory)
        );
        assert!(!store.status().failed && !store.status().collecting);
        assert_eq!(fs::read(&file).unwrap(), original);
        drop(held);
    }
    collect(&mut store, &[[7; BLOCK_SIZE]]);
    super::read(&store, &[7; BLOCK_SIZE]);
}
