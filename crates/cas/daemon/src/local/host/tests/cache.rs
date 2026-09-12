use super::*;

#[test]
fn private_images_share_verified_read_fills_without_write_admission() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: 2 * BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    assert_eq!(host.shared.cache.status().counters.fills, 0);
    read(&mut first, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.fills, 1);
    read(&mut second, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.hits, 1);
    write(&mut first, 2, 0, &[9; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.fills, 1);
    read(&mut first, 3, &[9; BLOCK_SIZE]);
    read(&mut second, 2, &[7; BLOCK_SIZE]);
    assert!(host.shared.cache.status().payload.peak.bytes <= 2 * BLOCK_SIZE);
    assert!(host.shared.gate.failure().is_none());
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn reads_succeed_when_evicted_readers_hold_all_cache_capacity() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    write(&mut first, 0, 0, &[1; BLOCK_SIZE]);
    write(&mut second, 0, 0, &[2; BLOCK_SIZE]);
    drained(&mut first, 1);
    drained(&mut second, 1);
    read(&mut first, 1, &[1; BLOCK_SIZE]);
    let hash = cas_core::chunk::Chunk::new(&[1; BLOCK_SIZE])
        .unwrap()
        .hash();
    let held = host.shared.cache.get(&hash).unwrap();
    read(&mut second, 1, &[2; BLOCK_SIZE]);
    let status = host.shared.cache.status();
    assert_eq!(status.capacity_bytes, BLOCK_SIZE);
    assert_eq!(status.reader_held_bytes, BLOCK_SIZE);
    assert_eq!(status.resident_bytes, 0);
    assert_eq!(status.counters.refused, 1);
    read(&mut first, 2, &[1; BLOCK_SIZE]);
    assert!(host.shared.gate.failure().is_none());
    drop(held);
    read(&mut second, 2, &[2; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().resident_bytes, BLOCK_SIZE);
    assert_eq!(host.shared.cache.status().payload.peak.bytes, BLOCK_SIZE);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}
