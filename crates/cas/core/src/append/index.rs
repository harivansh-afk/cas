//! A disjoint interval map. Entries own metadata and pin immutable payloads.
use std::collections::BTreeMap;
use std::sync::Arc;

use super::segment::Segment;

#[derive(Debug)]
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
    pub source: Option<(Arc<Payload>, u64)>,
}

impl Mapping {
    fn suffix(&self, skip: u64) -> Self {
        Self {
            end: self.end,
            sequence: self.sequence,
            source: self
                .source
                .as_ref()
                .map(|(payload, offset)| (Arc::clone(payload), offset + skip)),
        }
    }
}

#[derive(Default)]
pub(super) struct Index(BTreeMap<u64, Mapping>);

impl Index {
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn replace(&mut self, start: u64, mapping: Mapping) {
        let end = mapping.end;
        // The predecessor can straddle either or both boundaries.
        if let Some((&old_start, old)) = self.0.range(..start).next_back()
            && old.end > start
        {
            if old.end > end {
                let right = old.suffix(end - old_start);
                self.0.insert(end, right);
            }
            self.0.get_mut(&old_start).unwrap().end = start;
        }
        // Remove in place: zeroing a large range does not allocate a key list.
        while let Some((&old_start, _)) = self.0.range(start..end).next() {
            let old = self.0.remove(&old_start).unwrap();
            if old.end > end {
                self.0.insert(end, old.suffix(end - old_start));
            }
        }
        self.0.insert(start, mapping);
    }

    pub fn overlapping(&self, start: u64, end: u64) -> impl Iterator<Item = (u64, &Mapping)> {
        let begin = self
            .0
            .range(..=start)
            .next_back()
            .map_or(start, |(&key, _)| key);
        self.0
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
        let mut index = Index::default();
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
