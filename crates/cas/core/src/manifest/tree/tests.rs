use super::super::format::{self, Child, FileHeader, Root};
use super::*;
use crate::budget::Amount;
use std::cell::Cell;

const BLOCKS: u64 = 32768;

struct Image {
    bytes: Vec<u8>,
    commit: Commit,
    reads: Cell<u64>,
}

impl PageReader for Image {
    fn read_page(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        self.reads.set(self.reads.get() + 1);
        let offset = usize::try_from(offset).map_err(io::Error::other)?;
        let bytes = self
            .bytes
            .get(offset..offset + destination.len())
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "fixture page missing"))?;
        destination.copy_from_slice(bytes);
        Ok(())
    }
}

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}
fn metadata() -> Arc<Budget> {
    budget(128 * 1024 * 1024)
}
fn hash(block: u64) -> Hash {
    *blake3::hash(&block.to_le_bytes()).as_bytes()
}
fn mapping(block: u64) -> Extent {
    Extent {
        start: block,
        end: block + 1,
        hash: Some(hash(block)),
    }
}

impl Image {
    fn new() -> Self {
        let commit = Commit {
            store: [1; 16],
            image: [2; 16],
            generation: 1,
            root: Root::default(),
            durable: 0,
            image_bytes: BLOCKS * BLOCK_SIZE as u64,
        };
        let mut bytes = FileHeader {
            store: commit.store,
            image_bytes: commit.image_bytes,
        }
        .encode()
        .unwrap()
        .as_slice()
        .to_vec();
        bytes.extend_from_slice(commit.encode(4096).unwrap().as_slice());
        Self {
            bytes,
            commit,
            reads: Cell::new(0),
        }
    }

    fn apply(&mut self, edits: &[Extent], metadata: &Arc<Budget>) -> Stats {
        let prepared = Prepared::build(
            self,
            self.commit,
            self.bytes.len() as u64,
            edits,
            self.commit.durable + 1,
            Arc::clone(metadata),
        )
        .unwrap();
        let stats = prepared.stats();
        assert_eq!(metadata.usage().current.bytes, stats.allocated_bytes);
        assert_eq!(prepared.previous(), self.commit);
        assert_eq!(prepared.offset(), self.bytes.len() as u64);
        assert_eq!(prepared.bytes().as_ptr() as usize % BLOCK_SIZE, 0);
        self.bytes.extend_from_slice(prepared.bytes());
        self.commit = prepared.commit();
        drop(prepared);
        assert_eq!(metadata.usage().current.bytes, 0);
        stats
    }

    fn observe(&self, commit: Commit, end: u64, metadata: &Arc<Budget>) -> Vec<Option<Hash>> {
        let mut observed = vec![None; BLOCKS as usize];
        let mut tree = Tree::new(self, commit, end, Arc::clone(metadata)).unwrap();
        tree.walk(|extent| {
            observed[extent.start as usize..extent.end as usize].fill(extent.hash);
            Ok(())
        })
        .unwrap();
        for block in [0, 1, 62, 63, 64, 1000, 19000, 19844, BLOCKS - 1] {
            assert_eq!(tree.get(block).unwrap(), observed[block as usize]);
        }
        assert!(tree.get(BLOCKS).is_err());
        observed
    }
}

#[test]
fn cow_overwrites_and_zero_ranges_match_an_independent_flat_image() {
    let metadata = metadata();
    let mut image = Image::new();
    let mut oracle = vec![None; BLOCKS as usize];
    // Sequential inserts force leaf/root splits before the overlapping edits.
    for start in (0..2048).step_by(256) {
        let edits: Vec<_> = (start..start + 256).map(mapping).collect();
        let stats = image.apply(&edits, &metadata);
        assert!(stats.max_pages_per_change <= 24);
        for edit in edits {
            oracle[edit.start as usize] = edit.hash;
        }
    }
    assert_eq!(image.commit.root.height, 2);
    let saved = (image.commit, image.bytes.len() as u64, oracle.clone());
    let mut state = 0x013579abcdef2468u64;
    for _ in 0..16 {
        let mut edits = Vec::new();
        for _ in 0..40 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = (state >> 16) % 4096;
            let zero = state & 3 == 0;
            let end = if zero {
                (start + (state >> 32) % 257 + 1).min(BLOCKS)
            } else {
                start + 1
            };
            let edit = Extent {
                start,
                end,
                hash: (!zero).then(|| hash(state)),
            };
            oracle[start as usize..end as usize].fill(edit.hash);
            edits.push(edit);
        }
        image.apply(&edits, &metadata);
        assert_eq!(
            image.observe(image.commit, image.bytes.len() as u64, &metadata),
            oracle
        );
    }
    // All old pages are immutable; appending new roots cannot alter the saved view.
    assert_eq!(image.observe(saved.0, saved.1, &metadata), saved.2);
    assert_eq!(metadata.usage().current.bytes, 0);
    assert!(metadata.usage().peak.bytes <= 128 * 1024 * 1024);
}

fn deep_image() -> Image {
    let mut image = Image::new();
    let mut leaves = Vec::new();
    for start in (0..19845).step_by(63) {
        let entries: Vec<_> = (start..start + 63).map(mapping).collect();
        let offset = image.bytes.len() as u64;
        image.bytes.extend_from_slice(
            format::leaf(offset, image.commit.image_bytes, &entries)
                .unwrap()
                .as_slice(),
        );
        leaves.push(Child { start, offset });
    }
    let mut branches = Vec::new();
    for children in leaves.chunks(252) {
        let offset = image.bytes.len() as u64;
        image.bytes.extend_from_slice(
            format::branch(offset, image.commit.image_bytes, 1, children)
                .unwrap()
                .as_slice(),
        );
        branches.push(Child {
            start: children[0].start,
            offset,
        });
    }
    let offset = image.bytes.len() as u64;
    image.bytes.extend_from_slice(
        format::branch(offset, image.commit.image_bytes, 2, &branches)
            .unwrap()
            .as_slice(),
    );
    image.commit.root = Root { offset, height: 3 };
    image.commit.generation = 2;
    image.bytes.extend_from_slice(
        image
            .commit
            .encode(image.bytes.len() as u64)
            .unwrap()
            .as_slice(),
    );
    image
}

#[test]
fn large_erasure_reads_boundaries_and_detaches_covered_subtrees() {
    let metadata = metadata();
    let mut image = deep_image();
    let mut oracle = vec![None; BLOCKS as usize];
    for block in 0..19845 {
        oracle[block as usize] = Some(hash(block));
    }
    assert_eq!(
        image.observe(image.commit, image.bytes.len() as u64, &metadata),
        oracle
    );
    image.reads.set(0);
    let stats = image.apply(
        &[Extent {
            start: 1000,
            end: 19000,
            hash: None,
        }],
        &metadata,
    );
    assert!(
        stats.old_page_reads <= 7,
        "read {} old pages",
        stats.old_page_reads
    );
    assert!(stats.written_pages <= 8 * 3 + 8 + 1);
    oracle[1000..19000].fill(None);
    assert_eq!(
        image.observe(image.commit, image.bytes.len() as u64, &metadata),
        oracle
    );
    let stats = image.apply(
        &[Extent {
            start: 0,
            end: BLOCKS,
            hash: None,
        }],
        &metadata,
    );
    assert_eq!(stats.old_page_reads, 0);
    assert_eq!(stats.written_pages, 1); // Only a COMMIT for the empty root.
    assert_eq!(image.commit.root, Root::default());
    assert!(
        image
            .observe(image.commit, image.bytes.len() as u64, &metadata)
            .iter()
            .all(Option::is_none)
    );
}

#[test]
fn a_full_internal_root_splits_when_an_existing_leaf_grows() {
    let metadata = metadata();
    let mut image = Image::new();
    let mut children = Vec::new();
    let mut oracle = vec![None; BLOCKS as usize];
    for leaf in 0..252 {
        let start = leaf * 126;
        let entries: Vec<_> = (0..63).map(|n| mapping(start + n * 2)).collect();
        for entry in &entries {
            oracle[entry.start as usize] = entry.hash;
        }
        let offset = image.bytes.len() as u64;
        image.bytes.extend_from_slice(
            format::leaf(offset, image.commit.image_bytes, &entries)
                .unwrap()
                .as_slice(),
        );
        children.push(Child { start, offset });
    }
    let offset = image.bytes.len() as u64;
    image.bytes.extend_from_slice(
        format::branch(offset, image.commit.image_bytes, 1, &children)
            .unwrap()
            .as_slice(),
    );
    image.commit.root = Root { offset, height: 2 };
    image.bytes.extend_from_slice(
        image
            .commit
            .encode(image.bytes.len() as u64)
            .unwrap()
            .as_slice(),
    );
    let stats = image.apply(&[mapping(1)], &metadata);
    oracle[1] = Some(hash(1));
    assert_eq!(image.commit.root.height, 3);
    assert_eq!(stats.max_pages_per_change, 5); // Two leaves, two branches, one root.
    assert_eq!(
        image.observe(image.commit, image.bytes.len() as u64, &metadata),
        oracle
    );
}

#[test]
fn the_maximum_compaction_batch_fits_its_preallocated_metadata_budget() {
    let metadata = metadata();
    let mut image = Image::new();
    let mut edits: Vec<_> = (0..256).map(mapping).collect();
    edits.extend((0..62).map(|block| Extent {
        start: block,
        end: block + 1,
        hash: None,
    }));
    let stats = image.apply(&edits, &metadata);
    assert_eq!(stats.changes, MAX_CHANGES);
    assert_eq!(stats.allocated_bytes, (MAX_CHANGES * 72 + 1) * BLOCK_SIZE);
    assert_eq!(
        metadata.usage().peak.bytes,
        stats.allocated_bytes + BLOCK_SIZE
    );
    assert!(metadata.usage().peak.bytes < 128 * 1024 * 1024);
    let observed = image.observe(image.commit, image.bytes.len() as u64, &metadata);
    for block in 0..BLOCKS {
        assert_eq!(
            observed[block as usize],
            (62..256).contains(&block).then(|| hash(block))
        );
    }
}

#[test]
fn zero_extents_split_around_insertions_and_root_collapse_preserves_the_remaining_mapping() {
    let metadata = metadata();
    let mut image = Image::new();
    let offset = image.bytes.len() as u64;
    image.bytes.extend_from_slice(
        format::leaf(
            offset,
            image.commit.image_bytes,
            &[Extent {
                start: 0,
                end: BLOCKS,
                hash: None,
            }],
        )
        .unwrap()
        .as_slice(),
    );
    image.commit.root = Root { offset, height: 1 };
    image.bytes.extend_from_slice(
        image
            .commit
            .encode(image.bytes.len() as u64)
            .unwrap()
            .as_slice(),
    );
    let edits: Vec<_> = (0..200).map(|n| mapping(n * 2 + 100)).collect();
    image.apply(&edits, &metadata);
    assert!(image.commit.root.height >= 2);
    image.apply(
        &[
            Extent {
                start: 0,
                end: 498,
                hash: None,
            },
            Extent {
                start: 499,
                end: BLOCKS,
                hash: None,
            },
        ],
        &metadata,
    );
    assert_eq!(image.commit.root.height, 1);
    let observed = image.observe(image.commit, image.bytes.len() as u64, &metadata);
    assert_eq!(observed.iter().filter(|v| v.is_some()).count(), 1);
    assert_eq!(observed[498], Some(hash(498)));
}

#[test]
fn denied_plans_and_invalid_edits_leave_source_and_budget_untouched() {
    let image = Image::new();
    let low = budget(4096);
    assert!(
        Prepared::build(
            &image,
            image.commit,
            image.bytes.len() as u64,
            &[mapping(1)],
            1,
            Arc::clone(&low)
        )
        .is_err()
    );
    assert_eq!(low.usage().current.bytes, 0);
    let metadata = metadata();
    assert!(
        Prepared::build(
            &image,
            image.commit,
            image.bytes.len() as u64,
            &[mapping(1)],
            0,
            Arc::clone(&metadata)
        )
        .is_err()
    );
    for edits in [
        vec![mapping(1); MAX_CHANGES + 1],
        vec![Extent {
            start: 1,
            end: 1,
            hash: None,
        }],
        vec![Extent {
            start: 1,
            end: 3,
            hash: Some([1; 32]),
        }],
        vec![Extent {
            start: 1,
            end: BLOCKS + 1,
            hash: None,
        }],
    ] {
        assert!(
            Prepared::build(
                &image,
                image.commit,
                image.bytes.len() as u64,
                &edits,
                1,
                Arc::clone(&metadata)
            )
            .is_err()
        );
    }
    assert!(
        Prepared::build(
            &image,
            Commit {
                generation: u64::MAX,
                ..image.commit
            },
            image.bytes.len() as u64,
            &[],
            1,
            Arc::clone(&metadata)
        )
        .is_err()
    );
    assert!(
        Prepared::build(
            &image,
            image.commit,
            i64::MAX as u64,
            &[],
            1,
            Arc::clone(&metadata)
        )
        .is_err()
    );
    assert_eq!(image.reads.get(), 0);
    assert_eq!(metadata.usage().current.bytes, 0);
    assert_eq!(image.bytes.len(), 2 * BLOCK_SIZE);
}

#[test]
fn parent_child_minimum_level_and_bounds_are_checked_beyond_page_crcs() {
    for (first_end, second_start, level) in [(1, 10, 1), (20, 10, 1), (1, 10, 2)] {
        let metadata = metadata();
        let mut image = Image::new();
        let leaf_offset = image.bytes.len() as u64;
        image.bytes.extend_from_slice(
            format::leaf(
                leaf_offset,
                image.commit.image_bytes,
                &[Extent {
                    start: 0,
                    end: first_end,
                    hash: None,
                }],
            )
            .unwrap()
            .as_slice(),
        );
        let root = image.bytes.len() as u64;
        // Both keys reference one page: local page CRCs/addresses are valid,
        // but traversal must reject the inconsistent graph.
        let children = [
            Child {
                start: 0,
                offset: leaf_offset,
            },
            Child {
                start: second_start,
                offset: leaf_offset,
            },
        ];
        image.bytes.extend_from_slice(
            format::branch(root, image.commit.image_bytes, level, &children)
                .unwrap()
                .as_slice(),
        );
        image.commit.root = Root {
            offset: root,
            height: level + 1,
        };
        image.bytes.extend_from_slice(
            image
                .commit
                .encode(image.bytes.len() as u64)
                .unwrap()
                .as_slice(),
        );
        let mut tree = Tree::new(&image, image.commit, image.bytes.len() as u64, metadata).unwrap();
        assert!(tree.walk(|_| Ok(())).is_err());
    }
}
