//! Checked tree traversal and bounded COW preparation. Preparation writes no IO.
mod editor;
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
        require(
            end >= (2 * BLOCK_SIZE) as u64
                && end.is_multiple_of(BLOCK_SIZE as u64)
                && end <= i64::MAX as u64,
            "manifest committed end",
        )?;
        commit.validate(end - BLOCK_SIZE as u64)?;
        Ok(Self {
            source,
            commit,
            end,
            scratch: AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(metadata))?,
            reads: 0,
        })
    }

    fn root_cursor(&self) -> Cursor {
        Cursor {
            offset: self.commit.root.offset,
            level: self.commit.root.height.saturating_sub(1),
            minimum: None,
            lower: 0,
            upper: self.commit.image_bytes / BLOCK_SIZE as u64,
        }
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
        require(
            block < self.commit.image_bytes / BLOCK_SIZE as u64,
            "manifest lookup outside image",
        )?;
        if self.commit.root.offset == 0 {
            return Ok(None);
        }
        let mut cursor = self.root_cursor();
        loop {
            match self.read_node(cursor)? {
                Node::Leaf(entries) => {
                    return Ok(entries
                        .iter()
                        .find(|e| e.start <= block && block < e.end)
                        .and_then(|e| e.hash));
                }
                Node::Branch(children) => {
                    let Some(index) = children.iter().rposition(|child| child.start <= block)
                    else {
                        return Ok(None);
                    };
                    cursor = cursor.child(&children, index);
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
