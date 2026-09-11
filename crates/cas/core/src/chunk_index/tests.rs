use super::*;
use crate::budget::Amount;

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

fn address(segment: u64) -> Address {
    Address::new(segment, BLOCK_SIZE as u64).unwrap()
}

#[test]
fn address_boundaries_do_not_alias_on_overflow_or_unaligned_offsets() {
    for (segment, offset) in [
        (0, 4096),
        (MAX_SEGMENT + 1, 4096),
        (1, 0),
        (1, 4097),
        (1, MAX_SEGMENT_BYTES),
    ] {
        assert!(Address::new(segment, offset).is_err());
    }
    let last = Address::new(MAX_SEGMENT, MAX_SEGMENT_BYTES - BLOCK_SIZE as u64).unwrap();
    assert_eq!(last.segment(), MAX_SEGMENT);
    assert_eq!(last.offset(), MAX_SEGMENT_BYTES - BLOCK_SIZE as u64);
    assert_eq!(std::mem::size_of::<Address>(), 8);
}

#[test]
fn denied_growth_keeps_entries_and_charges_both_tables_when_permitted() {
    let mut probe = Index::new(budget(usize::MAX));
    probe.reserve(1).unwrap();
    let old_bytes = probe.allocated_bytes();
    let capacity = probe.table.capacity();
    probe.reserve(capacity + 1).unwrap();
    let new_bytes = probe.allocated_bytes();
    assert!(new_bytes > old_bytes);
    drop(probe);

    for (limit, succeeds) in [(new_bytes, false), (new_bytes + old_bytes, true)] {
        let credits = budget(limit);
        let mut index = Index::new(Arc::clone(&credits));
        index.insert([7; 32], address(1)).unwrap();
        assert_eq!(index.allocated_bytes(), old_bytes);
        assert_eq!(index.reserve(capacity + 1).is_ok(), succeeds);
        assert_eq!(index.get(&[7; 32]), Some(address(1)));
        assert_eq!(index.len(), 1);
        assert_eq!(credits.usage().current.bytes, index.allocated_bytes());
        assert_eq!(
            credits.usage().peak.bytes,
            if succeeds { limit } else { old_bytes }
        );
        if !succeeds {
            assert_eq!(credits.usage().rejected, 1);
        }
        drop(index);
        assert_eq!(credits.usage().current, Amount::default());
        assert_eq!(credits.usage().admitted, credits.usage().released);
    }
}

#[test]
fn full_hash_equality_zero_hash_and_gc_marks_survive_growth_and_relocation() {
    let credits = budget(128 * 1024);
    let mut index = Index::new(Arc::clone(&credits));
    // All keys deliberately share the same table hash, including the zero key.
    for value in 0..128u64 {
        let mut hash = [0; 32];
        hash[24..].copy_from_slice(&value.to_le_bytes());
        assert_eq!(
            index.insert(hash, address(value + 1)).unwrap(),
            address(value + 1)
        );
    }
    assert_eq!(index.len(), 128);
    let allocated = index.allocated_bytes();
    assert_eq!(credits.usage().current.bytes, allocated);
    assert!(allocated > 128 * (32 + 8));
    assert_eq!(index.insert([0; 32], address(1000)).unwrap(), address(1));
    index.mark(&[0; 32]).unwrap();
    assert!(
        index
            .relocate(&[0; 32], address(999), address(1000))
            .is_err()
    );
    index.relocate(&[0; 32], address(1), address(1000)).unwrap();
    index.remove_unmarked();
    assert_eq!(index.len(), 1);
    assert_eq!(index.get(&[0; 32]), Some(address(1000)));
    assert!(index.mark(&[255; 32]).is_err());
    assert_eq!(index.allocated_bytes(), allocated);
    index.clear_marks();
    index.remove_unmarked();
    assert!(index.is_empty());
    drop(index);
    assert_eq!(credits.usage().current.bytes, 0);
}

#[test]
fn separate_indexes_share_the_same_hard_metadata_pool() {
    let credits = budget(4096);
    let mut first = Index::new(Arc::clone(&credits));
    let mut second = Index::new(Arc::clone(&credits));
    for n in 1..1000u64 {
        let hash = *blake3::hash(&n.to_le_bytes()).as_bytes();
        if first.insert(hash, address(n)).is_err() {
            break;
        }
        let _ = second.insert(hash, address(n));
        assert_eq!(
            credits.usage().current.bytes,
            first.allocated_bytes() + second.allocated_bytes()
        );
    }
    assert!(credits.usage().rejected > 0);
    assert!(credits.usage().peak.bytes <= 4096);
    assert!(!first.is_empty());
    assert!(!second.is_empty());
    drop((first, second));
    assert_eq!(credits.usage().current.bytes, 0);
}
