//! One checked descent, driven either by synchronous reads or page CQEs.
use super::{Commit, Cursor, Node, Page, validate_view};
use crate::{BLOCK_SIZE, chunk_index::Hash, encoding::require};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LookupState {
    Page { offset: u64, level: u16 },
    Complete(Option<Hash>),
}

pub struct Lookup {
    image_bytes: u64,
    end: u64,
    block: u64,
    cursor: Option<Cursor>,
    result: Option<Hash>,
    failed: bool,
}

const _: () = assert!(size_of::<Lookup>() <= 128);

impl Lookup {
    pub fn new(commit: Commit, end: u64, block: u64) -> io::Result<Self> {
        validate_view(commit, end)?;
        require(
            block < commit.image_bytes / BLOCK_SIZE as u64,
            "manifest lookup outside image",
        )?;
        Ok(Self {
            image_bytes: commit.image_bytes,
            end,
            block,
            cursor: (commit.root.offset != 0).then(|| Cursor::root(commit)),
            result: None,
            failed: false,
        })
    }

    pub fn state(&self) -> io::Result<LookupState> {
        require(!self.failed, "manifest lookup failed")?;
        match self.cursor {
            Some(cursor) => {
                super::require_before_commit(cursor.offset, self.end)?;
                Ok(LookupState::Page {
                    offset: cursor.offset,
                    level: cursor.level,
                })
            }
            None => Ok(LookupState::Complete(self.result)),
        }
    }

    /// Accept only the requested full page. Invalid input poisons this lookup;
    /// a completed lookup cannot accept another page.
    pub fn accept(&mut self, offset: u64, bytes: &[u8]) -> io::Result<LookupState> {
        self.state()?;
        let cursor = self
            .cursor
            .ok_or_else(|| io::Error::other("manifest lookup already complete"))?;
        self.failed = true;
        require(offset == cursor.offset, "manifest lookup completion offset")?;
        let node = Node::decode(Page::decode(bytes, offset, self.image_bytes)?, cursor)?;
        self.cursor = match node {
            Node::Leaf(entries) => {
                self.result = entries
                    .iter()
                    .find(|entry| entry.start <= self.block && self.block < entry.end)
                    .and_then(|entry| entry.hash);
                None
            }
            Node::Branch(children) => children
                .iter()
                .rposition(|child| child.start <= self.block)
                .map(|index| cursor.child(&children, index)),
        };
        self.failed = false;
        self.state()
    }
}
