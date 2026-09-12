use super::*;

fn metadata() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    })
}

fn block(value: u8) -> (Hash, [u8; BLOCK_SIZE]) {
    let bytes = [value; BLOCK_SIZE];
    (*blake3::hash(&bytes).as_bytes(), bytes)
}

#[test]
fn promotion_and_repeated_eviction_preserve_the_recent_block() {
    let metadata = metadata();
    let cache = Cache::new(2 * BLOCK_SIZE, &metadata).unwrap();
    let (hot, bytes) = block(1);
    drop(cache.fill(hot, &bytes).unwrap());
    let table_bytes = cache.status().table_bytes;
    for value in 2..=200 {
        assert_eq!(cache.get(&hot).unwrap().as_slice(), bytes);
        let (hash, bytes) = block(value);
        drop(cache.fill(hash, &bytes).unwrap());
        assert_eq!(cache.get(&hash).unwrap().as_slice(), bytes);
        let status = cache.status();
        assert_eq!(status.table_bytes, table_bytes);
        assert!(status.payload.peak.bytes <= 2 * BLOCK_SIZE);
    }
    assert!(cache.get(&block(199).0).is_none());
    assert!(cache.get(&hot).is_some());
    drop(cache);
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn eviction_and_shutdown_retain_actual_reader_held_bytes() {
    let metadata = metadata();
    let cache = Cache::new(2 * BLOCK_SIZE, &metadata).unwrap();
    let (a, first) = block(1);
    let (b, second) = block(2);
    let (c, third) = block(3);
    let held_a = cache.fill(a, &first).unwrap().unwrap();
    let held_b = cache.fill(b, &second).unwrap().unwrap();
    assert!(cache.fill(c, &third).unwrap().is_none());
    let status = cache.status();
    assert_eq!(status.resident_bytes, 0);
    assert_eq!(status.reader_held_bytes, 2 * BLOCK_SIZE);
    assert_eq!(status.payload.current.bytes, 2 * BLOCK_SIZE);
    assert_eq!(held_a.as_slice(), first);
    drop(held_a);
    let held_c = cache.fill(c, &third).unwrap().unwrap();
    assert_eq!(cache.status().reader_held_bytes, BLOCK_SIZE);
    let payload = Arc::clone(&cache.payload);
    drop(cache);
    assert_eq!(payload.usage().current.bytes, 2 * BLOCK_SIZE);
    assert!(metadata.usage().current.bytes > 0);
    assert_eq!(held_b.as_slice(), second);
    assert_eq!(held_c.as_slice(), third);
    drop((held_b, held_c));
    assert_eq!(payload.usage().current, Amount::default());
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn invalid_fills_and_metadata_refusal_do_not_replace_or_leak_owners() {
    let metadata = metadata();
    let cache = Cache::new(BLOCK_SIZE, &metadata).unwrap();
    let (hash, bytes) = block(4);
    drop(cache.fill(hash, &bytes).unwrap());
    assert_eq!(
        cache.fill(hash, &[0; BLOCK_SIZE]).err().unwrap().kind(),
        io::ErrorKind::InvalidData
    );
    assert_eq!(cache.status().counters.evictions, 0);
    assert_eq!(cache.get(&hash).unwrap().as_slice(), bytes);
    cache.clear();
    let held = metadata
        .reserve(Amount {
            bytes: 1024 * 1024 - metadata.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    assert_eq!(
        cache.fill(hash, &bytes).err().unwrap().kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(cache.status().payload.current, Amount::default());
    assert_eq!(cache.status().resident_bytes, 0);
    drop((held, cache));
    assert_eq!(metadata.usage().current, Amount::default());
    let denied = Budget::new(Amount::default());
    assert_eq!(
        Cache::new(BLOCK_SIZE, &denied).err().unwrap().kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(denied.usage().current, Amount::default());
}
