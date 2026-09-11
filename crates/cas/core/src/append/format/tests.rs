use super::*;

const IMAGE_BYTES: u64 = 8 * MAX_REQUEST_BYTES as u64;

fn id(serial: u64) -> RequestId {
    RequestId {
        serial,
        attachment: 7,
        queue: 2,
        head: 17,
    }
}

fn packed(blocks: usize) -> Batch {
    let mut builder = Builder::new(IMAGE_BYTES, blocks * BLOCK_SIZE).unwrap();
    let allocation = builder.allocation_address();
    assert_eq!(allocation % BLOCK_SIZE, 0);
    for index in 0..blocks {
        builder
            .write(
                id(index as u64 + 1),
                (index * BLOCK_SIZE) as u64,
                BLOCK_SIZE,
                |destination| {
                    assert_eq!(
                        destination.as_ptr() as usize,
                        allocation + BLOCK_SIZE * (index + 1)
                    );
                    destination.fill(index as u8);
                    Ok(())
                },
            )
            .unwrap();
    }
    let batch = builder.seal(3, 9, 11).unwrap();
    assert_eq!(batch.allocation_address(), allocation);
    assert_eq!(batch.allocation_bytes(), (blocks + 1) * BLOCK_SIZE);
    batch
}

#[test]
fn payload_address_and_format_byte_counts() {
    for blocks in [1, 32, MAX_DESCRIPTORS] {
        let batch = packed(blocks);
        let header = Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE_BYTES).unwrap();
        header.verify_payload(&batch.bytes()[BLOCK_SIZE..]).unwrap();
        assert_eq!(header.envelope().last, 10 + blocks as u64);
        let fence = Batch::fence(3, 10, header.envelope().last).unwrap();
        assert_eq!(fence.bytes().len(), BLOCK_SIZE);
        assert_eq!(
            batch.bytes().len() + fence.bytes().len(),
            (blocks + 2) * BLOCK_SIZE
        );
    }
    assert_eq!(packed(1).bytes().len() + BLOCK_SIZE, 12 * 1024);
    assert_eq!(packed(32).bytes().len() + BLOCK_SIZE, 136 * 1024);
}

#[test]
fn full_request_and_zero_ranges_share_bounded_batch() {
    let mut builder = Builder::new(IMAGE_BYTES, MAX_REQUEST_BYTES).unwrap();
    builder
        .write(id(1), 0, MAX_REQUEST_BYTES, |bytes| {
            bytes.fill(19);
            Ok(())
        })
        .unwrap();
    assert!(
        builder
            .write(id(2), 0, BLOCK_SIZE, |_| panic!(
                "must reject before gathering"
            ))
            .is_err()
    );
    for serial in 2..=MAX_DESCRIPTORS as u64 {
        builder
            .zero(
                id(serial),
                BLOCK_SIZE as u64,
                IMAGE_BYTES - BLOCK_SIZE as u64,
            )
            .unwrap();
    }
    assert!(builder.zero(id(64), 0, BLOCK_SIZE as u64).is_err());
    let batch = builder.seal(1, 1, 1).unwrap();
    assert_eq!(batch.bytes().len(), MAX_BATCH_BYTES);
    let header = Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE_BYTES).unwrap();
    header.verify_payload(&batch.bytes()[BLOCK_SIZE..]).unwrap();
    assert_eq!(header.descriptors().count(), MAX_DESCRIPTORS);
}

#[test]
fn failed_gather_and_invalid_admission_do_not_consume_a_descriptor() {
    let mut builder = Builder::new(IMAGE_BYTES, BLOCK_SIZE).unwrap();
    assert!(builder.zero(id(1), 0, 0).is_err());
    assert!(
        builder
            .zero(id(1), u64::MAX - 4095, BLOCK_SIZE as u64)
            .is_err()
    );
    assert!(
        builder
            .write(id(1), 512, BLOCK_SIZE, |_| panic!("unaligned range"))
            .is_err()
    );
    assert!(
        builder
            .write(id(1), 0, BLOCK_SIZE, |bytes| {
                bytes[..100].fill(1);
                Err(io::Error::other("guest read failed"))
            })
            .is_err()
    );
    assert!(builder.is_empty());
    builder
        .write(id(1), 0, BLOCK_SIZE, |bytes| {
            bytes.fill(2);
            Ok(())
        })
        .unwrap();
    let batch = builder.seal(1, 1, u64::MAX).unwrap();
    Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE_BYTES)
        .unwrap()
        .verify_payload(&batch.bytes()[BLOCK_SIZE..])
        .unwrap();
    let mut builder = Builder::new(IMAGE_BYTES, 0).unwrap();
    builder.zero(id(1), 0, BLOCK_SIZE as u64).unwrap();
    builder.zero(id(2), 0, BLOCK_SIZE as u64).unwrap();
    assert!(builder.seal(1, 1, u64::MAX).is_err());
}

#[test]
fn every_header_byte_is_checksummed_and_short_headers_are_rejected() {
    let batch = packed(2);
    for index in 0..BLOCK_SIZE {
        let mut bytes = batch.bytes()[..BLOCK_SIZE].to_vec();
        bytes[index] ^= 1;
        assert!(Header::decode(&bytes, IMAGE_BYTES).is_err(), "byte {index}");
        assert!(
            Header::decode(&bytes[..index], IMAGE_BYTES).is_err(),
            "length {index}"
        );
    }
    let header = Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE_BYTES).unwrap();
    let mut payload = batch.bytes()[BLOCK_SIZE..].to_vec();
    payload[BLOCK_SIZE] ^= 1;
    assert!(header.verify_payload(&payload).is_err());
    assert!(header.verify_payload(&payload[..BLOCK_SIZE]).is_err());
}

#[test]
fn checksummed_but_inconsistent_metadata_is_rejected() {
    let batch = packed(2);
    // Each edit retains a valid header CRC. These are framing/range checks,
    // independent of detecting torn or corrupt bytes with a checksum.
    for (offset, replacement) in [
        (12, 64u64.to_le_bytes().to_vec()[..2].to_vec()),
        (32, 4096u32.to_le_bytes().to_vec()),
        (40, u64::MAX.to_le_bytes().to_vec()),
        (64 + 24, u64::MAX.to_le_bytes().to_vec()),
        (64 + 32, 512u64.to_le_bytes().to_vec()),
        (128 + 40, 0u32.to_le_bytes().to_vec()),
        (128, 1u64.to_le_bytes().to_vec()),
        (64 + 52, 2u16.to_le_bytes().to_vec()),
        (64 + 58, vec![1]),
        (4095, vec![1]),
    ] {
        let mut bytes = batch.bytes()[..BLOCK_SIZE].to_vec();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        let crc = checksum(&bytes, 56);
        put32(&mut bytes, 56, crc);
        assert!(
            Header::decode(&bytes, IMAGE_BYTES).is_err(),
            "offset {offset}"
        );
    }
}

#[test]
fn payload_that_looks_like_framing_is_ordinary_guest_data() {
    let forged = Batch::fence(1, 1, 900).unwrap();
    let mut builder = Builder::new(IMAGE_BYTES, BLOCK_SIZE).unwrap();
    builder
        .write(id(1), 0, BLOCK_SIZE, |bytes| {
            bytes.copy_from_slice(forged.bytes());
            Ok(())
        })
        .unwrap();
    let batch = builder.seal(1, 1, 1).unwrap();
    let header = Header::decode(&batch.bytes()[..BLOCK_SIZE], IMAGE_BYTES).unwrap();
    assert!(!header.envelope().fence);
    assert_eq!(header.envelope().last, 1);
    header.verify_payload(&batch.bytes()[BLOCK_SIZE..]).unwrap();
}

#[test]
fn segment_identity_and_reserved_bytes_are_validated() {
    let expected = SegmentHeader {
        store: [1; 16],
        image: [2; 16],
        epoch: 3,
        number: 4,
        capacity: 64 * MAX_REQUEST_BYTES as u64,
        image_bytes: IMAGE_BYTES,
        preceding_sequence: 17,
    };
    let buffer = expected.encode().unwrap();
    assert_eq!(SegmentHeader::decode(buffer.as_slice()).unwrap(), expected);
    for index in [0, 8, 16, 32, 48, 56, 64, 72, 80, 88, 4095] {
        let mut bytes = buffer.as_slice().to_vec();
        bytes[index] ^= 1;
        assert!(SegmentHeader::decode(&bytes).is_err());
    }
    let mut bytes = buffer.as_slice().to_vec();
    bytes[100] = 1;
    let crc = checksum(&bytes, 4092);
    put32(&mut bytes, 4092, crc);
    assert!(SegmentHeader::decode(&bytes).is_err());
    bytes[..8].copy_from_slice(b"CASLOG01");
    assert!(SegmentHeader::decode(&bytes).is_err());
}
