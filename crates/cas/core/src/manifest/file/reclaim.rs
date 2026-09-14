//! Bounded marking over exact retained roots; counters describe logical work,
//! not physical filesystem free space. The host supplies quiescence and permits.
use super::*;

/// Pages punched per window; each window spans 128 MiB of manifest file.
const PUNCH_WINDOW_PAGES: u64 = 32 * 1024;

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
    live: allocator_api2::vec::Vec<u64, BudgetAllocator>,
    scratch: AlignedBuffer<BudgetAllocator>,
}

impl<'a> Prepared<'a> {
    pub(super) fn new(roots: Roots<'a>) -> io::Result<Self> {
        let mut live =
            allocator_api2::vec::Vec::new_in(BudgetAllocator::new(Arc::clone(&roots.metadata)));
        live.try_reserve_exact(BLOCK_SIZE / size_of::<u64>())
            .map_err(|_| io::ErrorKind::OutOfMemory)?;
        Ok(Self {
            live,
            scratch: AlignedBuffer::try_new_in(
                BLOCK_SIZE,
                BudgetAllocator::new(Arc::clone(&roots.metadata)),
            )?,
            roots,
        })
    }

    pub(super) fn run(self) -> io::Result<ReclaimedPages> {
        let Self {
            roots,
            mut live,
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
        // Memory tracks retained pages, not historical file length. Every
        // tree is validated once before the first destructive operation. Growth
        // stays charged to metadata and refusal cannot justify a hole punch.
        let mut mark = |offset| -> io::Result<()> {
            live.try_reserve(1)
                .map_err(|_| io::ErrorKind::OutOfMemory)?;
            live.push(offset);
            Ok(())
        };
        mark(0)?;
        for key in &roots.keys {
            mark(key.end - BLOCK_SIZE as u64)?;
            let mut tree = Tree::with_scratch(roots.file, key.commit, key.end, scratch)?;
            tree.walk_pages(&mut mark, |_| Ok(()))?;
            result.tree_page_reads = result
                .tree_page_reads
                .checked_add(tree.page_reads())
                .ok_or_else(|| io::Error::other("manifest reclamation page counter exhausted"))?;
            scratch = tree.into_scratch();
        }
        live.sort_unstable();
        live.dedup();
        result.retained_pages = live.len() as u64;
        // Preserve the historical logical-window counter for report readers;
        // windows no longer trigger repeated tree traversals.
        result.windows = (end / BLOCK_SIZE as u64).div_ceil(PUNCH_WINDOW_PAGES);
        let mut first = 0;
        for &offset in &live {
            while first < offset {
                // Keep the existing maximum punch size even across huge holes.
                let next = offset.min(first + PUNCH_WINDOW_PAGES * BLOCK_SIZE as u64);
                direct::punch(roots.file, first, next - first)?;
                result.punch_calls += 1;
                result.punched_logical_bytes += next - first;
                first = next;
            }
            first = offset + BLOCK_SIZE as u64;
        }
        require(first == end, "reclamation omitted the current COMMIT")?;
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
