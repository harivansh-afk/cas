//! Checked tree traversal and bounded COW preparation. Preparation writes no IO.
mod editor;
mod lookup;
mod node;

use super::format::{Commit, Extent, Page};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    chunk_index::Hash,
    encoding::require,
};
use node::{Cursor, Node};
use std::{io, sync::Arc};

pub use editor::{MAX_CHANGES, Prepared, Stats};
pub use lookup::{Lookup, LookupState};

/// Read exactly one aligned page. File implementations retain their own IO lock.
pub trait PageReader {
    fn read_page(&self, offset: u64, destination: &mut [u8]) -> io::Result<()>;
}

#[cfg(target_os = "linux")]
impl PageReader for std::fs::File {
    fn read_page(&self, offset: u64, destination: &mut [u8]) -> io::Result<()> {
        require(destination.len() == BLOCK_SIZE, "manifest read page size")?;
        crate::direct::read_bytes(self, destination, offset)
    }
}

fn validate_view(commit: Commit, end: u64) -> io::Result<()> {
    require(
        end >= (2 * BLOCK_SIZE) as u64
            && end.is_multiple_of(BLOCK_SIZE as u64)
            && end <= i64::MAX as u64,
        "manifest committed end",
    )?;
    commit.validate(end - BLOCK_SIZE as u64)
}

pub struct Tree<'a, P: PageReader> {
    source: &'a P,
    commit: Commit,
    end: u64,
    scratch: AlignedBuffer<BudgetAllocator>,
    reads: u64,
}

impl<'a, P: PageReader> Tree<'a, P> {
    /// `end` is the byte after this committed transaction, not an unverified EOF.
    pub fn new(source: &'a P, commit: Commit, end: u64, metadata: Arc<Budget>) -> io::Result<Self> {
        validate_view(commit, end)?;
        Ok(Self {
            source,
            commit,
            end,
            scratch: AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(metadata))?,
            reads: 0,
        })
    }

    fn root_cursor(&self) -> Cursor {
        Cursor::root(self.commit)
    }

    fn read_node(&mut self, cursor: Cursor) -> io::Result<Node> {
        require(
            cursor.offset < self.end - BLOCK_SIZE as u64,
            "tree page beyond COMMIT",
        )?;
        self.source
            .read_page(cursor.offset, self.scratch.as_mut_slice())?;
        self.reads = self.reads.saturating_add(1);
        Node::decode(
            Page::decode(
                self.scratch.as_slice(),
                cursor.offset,
                self.commit.image_bytes,
            )?,
            cursor,
        )
    }

    pub fn page_reads(&self) -> u64 {
        self.reads
    }

    pub fn get(&mut self, block: u64) -> io::Result<Option<Hash>> {
        let mut lookup = Lookup::new(self.commit, self.end, block)?;
        loop {
            match lookup.state()? {
                LookupState::Complete(hash) => return Ok(hash),
                LookupState::Page { offset, .. } => {
                    self.source.read_page(offset, self.scratch.as_mut_slice())?;
                    self.reads = self.reads.saturating_add(1);
                    lookup.accept(offset, self.scratch.as_slice())?;
                }
            }
        }
    }

    /// Walk and validate all reachable mappings with height-bounded scratch.
    /// The visitor can check chunk availability or mark hashes during quiescent GC.
    pub fn walk(&mut self, mut visitor: impl FnMut(Extent) -> io::Result<()>) -> io::Result<()> {
        if self.commit.root.offset != 0 {
            self.visit(self.root_cursor(), &mut visitor)?;
        }
        Ok(())
    }

    fn visit(
        &mut self,
        cursor: Cursor,
        visitor: &mut impl FnMut(Extent) -> io::Result<()>,
    ) -> io::Result<()> {
        match self.read_node(cursor)? {
            Node::Leaf(entries) => {
                for entry in entries {
                    visitor(entry)?;
                }
            }
            Node::Branch(children) => {
                for index in 0..children.len() {
                    self.visit(cursor.child(&children, index), visitor)?;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
