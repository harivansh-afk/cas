use super::*;

#[test]
fn transferred_tail_preserves_sequence_zero_and_durable_cut_on_new_tickets() {
    let mut source = Fixture::new();
    write(source.log(), 0, 7);
    source.log().flush().unwrap();
    let input = source
        .log()
        .select_compaction(memory(), memory())
        .unwrap()
        .unwrap()
        .load()
        .unwrap();
    let receipt = input
        .write(&mut source.store, source.manifest.as_mut().unwrap())
        .unwrap();
    source.log().publish_compaction(receipt).unwrap();
    write(source.log(), 1, 9);
    let mut zero = Builder::new(CONFIG.image_bytes, 0).unwrap();
    zero.zero(id(3), 2 * BLOCK_SIZE as u64, BLOCK_SIZE as u64)
        .unwrap();
    source.log().append(zero).unwrap();
    assert!(source.log().capture_transfer(memory()).is_err());
    source.log().flush().unwrap();
    let transfer = source.log().capture_transfer(memory()).unwrap();
    let mut bytes = Vec::new();
    transfer.write(&mut bytes).unwrap();
    assert_eq!(bytes.len(), 24 + 2 * 32 + BLOCK_SIZE);
    let mut destination = Fixture::new();
    destination.put(7);
    drop(destination.log.take());
    drop(destination.manifest.take());
    fs::remove_dir_all(destination.path()).unwrap();
    fs::copy(
        source.path().parent().unwrap().join("manifest.v2"),
        destination.path().parent().unwrap().join("manifest.v2"),
    )
    .unwrap();
    let manifest = Manifest::inspect(
        destination.path().parent().unwrap(),
        Fixture::identity(),
        1,
        memory(),
        |hash| {
            require(
                destination.store.plan(hash)?.is_some(),
                "missing test dependency",
            )
        },
    )
    .unwrap()
    .recover()
    .unwrap();
    let mut log = Log::create_transferred(
        destination.tickets.clone(),
        CONFIG,
        Limits::default(),
        memory(),
        manifest.view().unwrap(),
    )
    .unwrap();
    assert_eq!(log.status().published, 1);
    log.import_transfer(&mut &bytes[..], 3).unwrap();
    assert_eq!((log.status().published, log.status().durable), (3, 3));
    let mut got = AlignedBuffer::new(2 * BLOCK_SIZE);
    log.read_into(BLOCK_SIZE as u64, &mut got).unwrap();
    assert_eq!(&got.as_slice()[..BLOCK_SIZE], &[9; BLOCK_SIZE]);
    assert_eq!(&got.as_slice()[BLOCK_SIZE..], &[0; BLOCK_SIZE]);
    assert!(log.import_transfer(&mut &bytes[..], 3).is_err());
    drop(log);
    drop(manifest);
    let inspected = Manifest::inspect(
        destination.path().parent().unwrap(),
        Fixture::identity(),
        1,
        memory(),
        |hash| {
            require(
                destination.store.plan(hash)?.is_some(),
                "missing recovered dependency",
            )
        },
    )
    .unwrap();
    let replay = Log::inspect_shared(
        destination.tickets.clone(),
        &inspected,
        Limits::default(),
        memory(),
    )
    .unwrap();
    replay.require_prefix(3).unwrap();
    let manifest = inspected.recover().unwrap();
    let mut log = replay.fresh(manifest.view().unwrap(), 3).unwrap();
    log.read_into(BLOCK_SIZE as u64, &mut got).unwrap();
    assert_eq!(&got.as_slice()[..BLOCK_SIZE], &[9; BLOCK_SIZE]);
}
