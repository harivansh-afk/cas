use super::*;

fn batch(count: usize) -> Batch {
    let mut builder = Builder::new(count).unwrap();
    for value in 1..=count {
        builder
            .push(Chunk::new(&[value as u8; BLOCK_SIZE]).unwrap())
            .unwrap();
    }
    builder.seal(17, 3, 100).unwrap()
}

fn repair_crc(bytes: &mut [u8], offset: usize) {
    bytes[offset..offset + 4].fill(0);
    let crc = crc32fast::hash(bytes);
    bytes[offset..offset + 4].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn fixed_layout_full_batch_matches_independently_assembled_bytes() {
    let batch = batch(MAX_CHUNKS);
    assert_eq!(batch.bytes().len(), MAX_BATCH_BYTES);
    assert_eq!(batch.allocated_bytes(), MAX_BATCH_BYTES);
    assert_eq!(batch.bytes().as_ptr() as usize % BLOCK_SIZE, 0);
    let mut expected = vec![0; MAX_BATCH_BYTES];
    expected[..8].copy_from_slice(b"CASCHB02");
    expected[8..10].copy_from_slice(&2u16.to_le_bytes());
    expected[10..12].copy_from_slice(&1u16.to_le_bytes());
    expected[12..14].copy_from_slice(&63u16.to_le_bytes());
    for (offset, value) in [(16, 17u64), (24, 3), (40, 100), (48, 162)] {
        expected[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    expected[32..36].copy_from_slice(&262144u32.to_le_bytes());
    expected[36..40].copy_from_slice(&258048u32.to_le_bytes());
    for n in 0..63 {
        let payload = [n as u8 + 1; BLOCK_SIZE];
        let slot = 64 + n * 64;
        expected[slot..slot + 32].copy_from_slice(blake3::hash(&payload).as_bytes());
        expected[slot + 32..slot + 36].copy_from_slice(&(n as u32 * 4096).to_le_bytes());
        expected[slot + 36..slot + 40].copy_from_slice(&4096u32.to_le_bytes());
        expected[slot + 40..slot + 44].copy_from_slice(&crc32fast::hash(&payload).to_le_bytes());
        expected[(n + 1) * BLOCK_SIZE..(n + 2) * BLOCK_SIZE].copy_from_slice(&payload);
    }
    repair_crc(&mut expected[..BLOCK_SIZE], 56);
    assert_eq!(batch.bytes(), expected);
    let header = Header::decode(&expected[..BLOCK_SIZE]).unwrap();
    assert_eq!(
        (
            header.segment(),
            header.batch(),
            header.first(),
            header.last()
        ),
        (17, 3, 100, 162)
    );
    header.verify_payload(&expected[BLOCK_SIZE..]).unwrap();
}

#[test]
fn metadata_semantics_are_checked_even_with_a_valid_crc() {
    let batch = batch(1);
    for (offset, value) in [
        (0, 0),
        (8, 3),
        (10, 2),
        (12, 0),
        (12, 64),
        (14, 1),
        (16, 0),
        (24, 0),
        (32, 1),
        (36, 1),
        (40, 0),
        (48, 99),
        (60, 1),
        (96, 1),
        (100, 1),
        (108, 1),
        (128, 1),
    ] {
        let mut bytes = batch.bytes()[..BLOCK_SIZE].to_vec();
        bytes[offset] = value;
        repair_crc(&mut bytes, 56);
        assert!(Header::decode(&bytes).is_err(), "offset {offset}");
    }
    for length in 0..BLOCK_SIZE {
        assert!(Header::decode(&batch.bytes()[..length]).is_err());
    }
    let mut bytes = batch.bytes().to_vec();
    bytes[BLOCK_SIZE..BLOCK_SIZE + 8].copy_from_slice(b"CASCHB02");
    let header = Header::decode(&bytes[..BLOCK_SIZE]).unwrap();
    assert!(header.verify_payload(&bytes[BLOCK_SIZE..]).is_err());
    assert!(
        header
            .verify_payload(&bytes[BLOCK_SIZE..bytes.len() - 1])
            .is_err()
    );
}

#[test]
fn chunk_and_wal_headers_cannot_be_interchanged_and_segment_fields_are_strict() {
    let value = SegmentHeader {
        store: [5; 16],
        number: 17,
        capacity: 64 * 1024 * 1024,
    };
    let encoded = value.encode().unwrap();
    assert_eq!(SegmentHeader::decode(encoded.as_slice()).unwrap(), value);
    #[cfg(target_os = "linux")]
    assert!(crate::append::format::SegmentHeader::decode(encoded.as_slice()).is_err());
    for offset in [8, 12, 32, 48, 72, 4091] {
        let mut bytes = encoded.as_slice().to_vec();
        bytes[offset] ^= 1;
        repair_crc(&mut bytes, 4092);
        assert!(SegmentHeader::decode(&bytes).is_err(), "offset {offset}");
    }
    for number in [0, MAX_SEGMENT + 1] {
        assert!(SegmentHeader { number, ..value }.encode().is_err());
    }
    for capacity in [0, 8192, 12289, MAX_SEGMENT_BYTES + 4096] {
        assert!(SegmentHeader { capacity, ..value }.encode().is_err());
    }
    #[cfg(target_os = "linux")]
    {
        let batch = batch(1);
        assert!(
            crate::append::format::Header::decode(&batch.bytes()[..BLOCK_SIZE], 1 << 20).is_err()
        );
    }
}

#[test]
fn packing_is_bounded_zero_content_is_absent_and_sealing_never_moves_payload() {
    assert!(Chunk::new(&[0; BLOCK_SIZE]).is_none());
    assert!(Builder::new(0).is_err());
    assert!(Builder::new(64).is_err());
    let mut builder = Builder::new(1).unwrap();
    let value = [9; BLOCK_SIZE];
    builder.push(Chunk::new(&value).unwrap()).unwrap();
    assert!(builder.push(Chunk::new(&value).unwrap()).is_err());
    let pointer = builder.buffer.as_slice().as_ptr();
    let sealed = builder.seal(MAX_SEGMENT, u64::MAX, u64::MAX).unwrap();
    assert_eq!(pointer, sealed.bytes().as_ptr());
    assert_eq!(&sealed.bytes()[BLOCK_SIZE..], &value);
    let mut builder = Builder::new(2).unwrap();
    for _ in 0..2 {
        builder.push(Chunk::new(&value).unwrap()).unwrap();
    }
    assert!(builder.seal(1, 1, u64::MAX).is_err());
}
