use super::*;

const CAPACITY: u64 = 1024 * BLOCK_SIZE as u64;

fn commit() -> Commit {
    Commit {
        store: [1; 16],
        image: [2; 16],
        generation: 3,
        root: Root {
            offset: 4096,
            height: 1,
        },
        durable: 7,
        image_bytes: CAPACITY,
    }
}

fn repair_crc(bytes: &mut [u8]) {
    bytes[56..60].fill(0);
    let crc = crc32fast::hash(bytes);
    bytes[56..60].copy_from_slice(&crc.to_le_bytes());
}

#[test]
fn leaf_full_capacity_has_exact_offsets_and_distinguishes_zero_hash_from_hole() {
    let extents: Vec<_> = (0..63)
        .map(|n| Extent {
            start: n * 2,
            end: n * 2 + 1,
            hash: Some([n as u8; 32]),
        })
        .collect();
    let encoded = leaf(4096, CAPACITY, &extents).unwrap();
    let mut expected = [0; BLOCK_SIZE];
    expected[..8].copy_from_slice(b"CASMAN02");
    expected[8..10].copy_from_slice(&2u16.to_le_bytes());
    expected[10..12].copy_from_slice(&1u16.to_le_bytes());
    expected[14..16].copy_from_slice(&63u16.to_le_bytes());
    expected[16..24].copy_from_slice(&4096u64.to_le_bytes());
    for (n, extent) in extents.iter().enumerate() {
        let slot = 64 + n * 64;
        expected[slot..slot + 8].copy_from_slice(&extent.start.to_le_bytes());
        expected[slot + 8..slot + 16].copy_from_slice(&extent.end.to_le_bytes());
        expected[slot + 16..slot + 48].copy_from_slice(&extent.hash.unwrap());
        expected[slot + 52..slot + 54].copy_from_slice(&1u16.to_le_bytes());
    }
    repair_crc(&mut expected);
    assert_eq!(encoded.as_slice(), expected);
    let page = Page::decode(encoded.as_slice(), 4096, CAPACITY).unwrap();
    assert_eq!(page.extents().unwrap().collect::<Vec<_>>(), extents);
    assert!(page.children().is_err());
    let zero = Extent {
        start: 0,
        end: 1024,
        hash: None,
    };
    let encoded = leaf(4096, CAPACITY, &[zero]).unwrap();
    assert_eq!(
        Page::decode(encoded.as_slice(), 4096, CAPACITY)
            .unwrap()
            .extents()
            .unwrap()
            .next(),
        Some(zero)
    );
}

#[test]
fn branch_capacity_order_addresses_and_height_are_bounded() {
    let children: Vec<_> = (0..252)
        .map(|n| Child {
            start: n,
            offset: (n + 1) * 4096,
        })
        .collect();
    let offset = 253 * 4096;
    let encoded = branch(offset, CAPACITY, 7, &children).unwrap();
    assert_eq!(
        Page::decode(encoded.as_slice(), offset, CAPACITY)
            .unwrap()
            .children()
            .unwrap()
            .collect::<Vec<_>>(),
        children
    );
    for level in [0, 8, u16::MAX] {
        assert!(branch(offset, CAPACITY, level, &children).is_err());
    }
    for child in [
        Child {
            start: 0,
            offset: 0,
        },
        Child { start: 0, offset },
        Child {
            start: 0,
            offset: 4097,
        },
        Child {
            start: 1024,
            offset: 4096,
        },
    ] {
        assert!(branch(offset, CAPACITY, 1, &[child]).is_err());
    }
    assert!(branch(offset, CAPACITY, 1, &[children[1], children[0]]).is_err());
    assert!(branch(offset, CAPACITY, 1, &vec![children[0]; 253]).is_err());
}

#[test]
fn file_and_commit_identity_survive_clone_without_changing_tree_addresses() {
    let header = FileHeader {
        store: [1; 16],
        image_bytes: CAPACITY,
    };
    let encoded = header.encode().unwrap();
    assert_eq!(FileHeader::decode(encoded.as_slice()).unwrap(), header);
    let original = commit();
    let encoded = original.encode(8192).unwrap();
    assert_eq!(
        Page::decode(encoded.as_slice(), 8192, CAPACITY)
            .unwrap()
            .commit()
            .unwrap(),
        original
    );
    let cloned = Commit {
        image: [8; 16],
        durable: 0,
        generation: 4,
        ..original
    };
    let encoded = cloned.encode(12288).unwrap();
    let observed = Page::decode(encoded.as_slice(), 12288, CAPACITY)
        .unwrap()
        .commit()
        .unwrap();
    assert_eq!(observed.root, original.root);
    assert_eq!(observed.image, [8; 16]);
    assert_eq!(observed.durable, 0);
    for root in [
        Root {
            offset: 0,
            height: 1,
        },
        Root {
            offset: 4096,
            height: 0,
        },
        Root {
            offset: 4097,
            height: 1,
        },
        Root {
            offset: 8192,
            height: 1,
        },
        Root {
            offset: 4096,
            height: 9,
        },
    ] {
        assert!(Commit { root, ..original }.encode(8192).is_err());
    }
    assert!(
        Commit {
            generation: 0,
            ..original
        }
        .encode(8192)
        .is_err()
    );
    assert!(
        Commit {
            root: Root::default(),
            ..original
        }
        .encode(8192)
        .is_ok()
    );
}

#[test]
fn damaged_and_rechecksummed_malformed_pages_fail_before_iteration() {
    let extent = Extent {
        start: 1,
        end: 2,
        hash: Some([7; 32]),
    };
    let encoded = leaf(4096, CAPACITY, &[extent]).unwrap();
    for length in 0..BLOCK_SIZE {
        assert!(Page::decode(&encoded.as_slice()[..length], 4096, CAPACITY).is_err());
    }
    for offset in 0..BLOCK_SIZE {
        let mut bytes = encoded.as_slice().to_vec();
        bytes[offset] ^= 1;
        assert!(
            Page::decode(&bytes, 4096, CAPACITY).is_err(),
            "byte {offset}"
        );
    }
    for (offset, value) in [
        (8, 1),
        (10, 0),
        (12, 1),
        (14, 0),
        (14, 64),
        (16, 1),
        (24, 1),
        (60, 1),
        (64, 3),
        (72, 0),
        (112, 1),
        (116, 0),
        (118, 1),
        (128, 1),
    ] {
        let mut bytes = encoded.as_slice().to_vec();
        bytes[offset] = value;
        repair_crc(&mut bytes);
        assert!(
            Page::decode(&bytes, 4096, CAPACITY).is_err(),
            "field {offset}"
        );
    }
    assert!(leaf(4096, CAPACITY, &[extent, extent]).is_err());
    assert!(leaf(4096, CAPACITY, &[Extent { end: 3, ..extent }]).is_err());
    assert!(
        leaf(
            4096,
            CAPACITY,
            &[Extent {
                end: 1025,
                hash: None,
                ..extent
            }]
        )
        .is_err()
    );
    assert!(leaf(4096, CAPACITY, &[]).is_err());
}
