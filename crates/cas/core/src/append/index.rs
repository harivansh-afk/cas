//! A disjoint interval map. Entries own metadata and pin immutable payloads.
mod nodes;

use crate::budget::Budget;
use arena_btreemap::BTreeMap;
use nodes::TreeNodes;
use std::io;
use std::sync::Arc;

use super::segment::Segment;

#[derive(Debug, Clone)]
pub(super) struct Payload {
    pub segment: Arc<Segment>,
    pub offset: u64,
    pub bytes: usize,
    pub sequence: u64,
    pub crc: u32,
}

#[derive(Debug, Clone)]
pub(super) struct Mapping {
    pub end: u64,
    pub sequence: u64,
    pub source: Option<(Payload, u64)>,
}

impl Mapping {
    fn suffix(&self, skip: u64) -> Self {
        Self {
            end: self.end,
            sequence: self.sequence,
            source: self
                .source
                .as_ref()
                .map(|(payload, offset)| (payload.clone(), offset + skip)),
        }
    }
}

// The pinned B=6 leaf/internal nodes fit the pool's 1 KiB/64-aligned slots.
const _: () = assert!(size_of::<Mapping>() <= 64 && align_of::<Mapping>() <= 8);

pub(super) struct Index {
    map: BTreeMap<u64, Mapping, TreeNodes>,
    nodes: TreeNodes,
    limit: usize,
}

impl Index {
    pub fn new(limit: usize, metadata: Arc<Budget>) -> io::Result<Self> {
        let nodes = TreeNodes::new(limit, metadata)?;
        Ok(Self {
            map: BTreeMap::new_in(nodes.clone()),
            nodes,
            limit,
        })
    }
    pub fn allocated_bytes(&self) -> usize {
        self.nodes.allocated_bytes()
    }
    pub fn nodes(&self) -> (usize, usize) {
        let usage = self.nodes.usage();
        (usage.current, usage.peak)
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn replace(&mut self, start: u64, mapping: Mapping) {
        assert!(
            self.len()
                .checked_add(2)
                .is_some_and(|count| count <= self.limit),
            "interval admission must precede publication"
        );
        let end = mapping.end;
        // The predecessor can straddle either or both boundaries.
        if let Some((&old_start, old)) = self.map.range(..start).next_back()
            && old.end > start
        {
            if old.end > end {
                let right = old.suffix(end - old_start);
                self.map.insert(end, right);
            }
            self.map.get_mut(&old_start).unwrap().end = start;
        }
        // Remove in place: zeroing a large range does not allocate a key list.
        while let Some((&old_start, _)) = self.map.range(start..end).next() {
            let old = self.map.remove(&old_start).unwrap();
            if old.end > end {
                self.map.insert(end, old.suffix(end - old_start));
            }
        }
        self.map.insert(start, mapping);
    }

    pub fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &Mapping)> {
        let begin = self
            .map
            .range(..=start)
            .next_back()
            .map_or(start, |(&key, _)| key);
        self.map
            .range(begin..end)
            .filter(move |(_, mapping)| mapping.end > start)
            .map(|(&offset, mapping)| (offset, mapping))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_overwrites_and_zeroes_match_an_independent_block_array() {
        let mut index = Index::new(34, super::super::default_metadata()).unwrap();
        let mut expected = [0; 32];
        let mut seed = 31u64;
        for sequence in 1..=2000 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            let start = (seed >> 32) as usize % expected.len();
            let length = 1 + seed as usize % (expected.len() - start);
            expected[start..start + length].fill(sequence);
            index.replace(
                start as u64,
                Mapping {
                    end: (start + length) as u64,
                    sequence,
                    source: None,
                },
            );
            let mut actual = [0; 32];
            let mut previous_end = 0;
            for (offset, mapping) in index.overlapping(0, 32) {
                assert!(offset >= previous_end);
                actual[offset as usize..mapping.end as usize].fill(mapping.sequence);
                previous_end = mapping.end;
            }
            assert_eq!(actual, expected);
            assert!(index.len() <= expected.len());
        }
    }
}
