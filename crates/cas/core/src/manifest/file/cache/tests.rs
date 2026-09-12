use super::*;

#[test]
fn evicted_pages_keep_their_metadata_and_page_credits_until_last_release() {
    let metadata = Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    });
    let cache = PageCache::new(BLOCK_SIZE, &metadata).unwrap();
    let key = PageKey {
        incarnation: 1,
        end: 3 * BLOCK_SIZE as u64,
        offset: BLOCK_SIZE as u64,
    };
    cache.fill(key, &[7; BLOCK_SIZE]).unwrap();
    let held = cache.get(&key).unwrap();
    let second = PageKey {
        incarnation: 2,
        ..key
    };
    cache.fill(second, &[8; BLOCK_SIZE]).unwrap();
    assert!(cache.get(&second).is_none());
    let status = cache.status();
    assert_eq!(status.reader_held_bytes, BLOCK_SIZE);
    assert_eq!(status.resident_bytes, 0);
    assert_eq!(status.counters.refused, 1);
    let pages = Arc::clone(&cache.pages);
    drop(cache);
    assert_eq!(held.bytes, [7; BLOCK_SIZE]);
    assert!(metadata.usage().current.bytes > BLOCK_SIZE);
    assert_eq!(pages.usage().current.bytes, BLOCK_SIZE);
    drop(held);
    assert_eq!(metadata.usage().current, Amount::default());
    assert_eq!(pages.usage().current, Amount::default());
}

#[test]
fn metadata_refusal_refunds_page_admission_and_preserves_existing_bytes() {
    let metadata = Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    });
    let cache = PageCache::new(2 * BLOCK_SIZE, &metadata).unwrap();
    let key = PageKey {
        incarnation: 1,
        end: 3 * BLOCK_SIZE as u64,
        offset: BLOCK_SIZE as u64,
    };
    cache.fill(key, &[7; BLOCK_SIZE]).unwrap();
    let held = metadata
        .reserve(Amount {
            bytes: 1024 * 1024 - metadata.usage().current.bytes,
            requests: 0,
        })
        .unwrap();
    assert_eq!(
        cache
            .fill(
                PageKey {
                    incarnation: 2,
                    ..key
                },
                &[8; BLOCK_SIZE]
            )
            .unwrap_err()
            .kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(cache.get(&key).unwrap().bytes, [7; BLOCK_SIZE]);
    assert_eq!(cache.status().payload.current.bytes, BLOCK_SIZE);
    drop((held, cache));
    assert_eq!(metadata.usage().current, Amount::default());
}
