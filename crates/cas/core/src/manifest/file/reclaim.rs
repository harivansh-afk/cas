//! Bounded marking over exact retained roots; counters describe logical work,
//! not physical filesystem free space. The host supplies quiescence and permits.
use super::*;

const WINDOW_PAGES: u64 = (BLOCK_SIZE * 8) as u64;

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct ReclaimedPages {
    pub roots: usize,
    pub file_bytes: u64,
    pub windows: u64,
    pub tree_page_reads: u64,
    pub retained_pages: u64,
    pub punch_calls: u64,
    pub punched_logical_bytes: u64,
    pub removed_tail_mapping_bytes: u64,
}

pub(super) struct Prepared<'a> {
    roots: Roots<'a>,
    bitmap: AlignedBuffer<BudgetAllocator>,
    scratch: AlignedBuffer<BudgetAllocator>,
}

impl<'a> Prepared<'a> {
    pub(super) fn new(roots: Roots<'a>) -> io::Result<Self> {
        let allocate = || {
            AlignedBuffer::try_new_in(
                BLOCK_SIZE,
                BudgetAllocator::new(Arc::clone(&roots.metadata)),
            )
        };
        Ok(Self {
            bitmap: allocate()?,
            scratch: allocate()?,
            roots,
        })
    }

    pub(super) fn run(self) -> io::Result<ReclaimedPages> {
        let Self {
            roots,
            mut bitmap,
            mut scratch,
        } = self;
        let end = roots
            .keys
            .last()
            .expect("owner retains its current root")
            .end;
        require(
            roots.file.metadata()?.len() == end,
            "manifest reclamation requires exact owned EOF",
        )?;
        direct::read_bytes(roots.file, scratch.as_mut_slice(), 0)?;
        let header = FileHeader::decode(scratch.as_slice())?;
        for key in &roots.keys {
            require(
                header.store == key.commit.store && header.image_bytes == key.commit.image_bytes,
                "retained root differs from FILE identity",
            )?;
            let offset = key.end - BLOCK_SIZE as u64;
            direct::read_bytes(roots.file, scratch.as_mut_slice(), offset)?;
            require(
                Page::decode(scratch.as_slice(), offset, key.commit.image_bytes)?.commit()?
                    == key.commit,
                "retained COMMIT differs from its root pin",
            )?;
        }
        direct::sync_data(roots.file)?;
        let mut cursor = end;
        let mut tail_bytes = 0;
        while let Some(range) = direct::next_extent(roots.file, cursor)? {
            tail_bytes += range.end - range.start;
            cursor = range.end;
        }
        let mut result = ReclaimedPages {
            roots: roots.keys.len(),
            file_bytes: end,
            removed_tail_mapping_bytes: tail_bytes,
            ..ReclaimedPages::default()
        };
        let pages = end / BLOCK_SIZE as u64;
        for first in (0..pages).step_by(WINDOW_PAGES as usize) {
            let last = (first + WINDOW_PAGES).min(pages);
            bitmap.as_mut_slice().fill(0);
            let mut marked = Marked {
                bytes: bitmap.as_mut_slice(),
                first,
                last,
            };
            marked.page(0);
            for key in &roots.keys {
                marked.page(key.end - BLOCK_SIZE as u64);
                let mut tree = Tree::with_scratch(roots.file, key.commit, key.end, scratch)?;
                tree.walk_pages(
                    |offset| {
                        marked.page(offset);
                        Ok(())
                    },
                    |_| Ok(()),
                )?;
                result.tree_page_reads = result
                    .tree_page_reads
                    .checked_add(tree.page_reads())
                    .ok_or_else(|| {
                        io::Error::other("manifest reclamation page counter exhausted")
                    })?;
                scratch = tree.into_scratch();
            }
            result.retained_pages += marked
                .bytes
                .iter()
                .map(|byte| u64::from(byte.count_ones()))
                .sum::<u64>();
            let mut page = first;
            while page < last {
                if marked.contains(page) {
                    page += 1;
                    continue;
                }
                let start = page;
                while page < last && !marked.contains(page) {
                    page += 1;
                }
                let length = (page - start) * BLOCK_SIZE as u64;
                direct::punch(roots.file, start * BLOCK_SIZE as u64, length)?;
                result.punch_calls += 1;
                result.punched_logical_bytes += length;
            }
            result.windows += 1;
        }
        if tail_bytes != 0 {
            // Unlike a hole punch beyond EOF, same-size truncate removes
            // preallocation on the supported filesystems. Verify it below.
            direct::truncate(roots.file, end)?;
        }
        direct::sync_data(roots.file)?;
        require(
            direct::next_extent(roots.file, end)?.is_none(),
            "manifest still has allocated extents beyond EOF",
        )?;
        Ok(result)
    }
}

struct Marked<'a> {
    bytes: &'a mut [u8],
    first: u64,
    last: u64,
}

impl Marked<'_> {
    fn page(&mut self, offset: u64) {
        let page = offset / BLOCK_SIZE as u64;
        if (self.first..self.last).contains(&page) {
            let bit = (page - self.first) as usize;
            self.bytes[bit / 8] |= 1 << (bit % 8);
        }
    }

    fn contains(&self, page: u64) -> bool {
        let bit = (page - self.first) as usize;
        self.bytes[bit / 8] & (1 << (bit % 8)) != 0
    }
}
