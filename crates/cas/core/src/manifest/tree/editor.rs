use super::super::format::{
    self, BRANCH_CAPACITY, Child, Commit, Extent, LEAF_CAPACITY, MAX_HEIGHT, Page, Root,
};
use super::{
    PageReader, Tree,
    node::{Children, Cursor, Leaves, Links, Node, one},
};
use crate::{
    BLOCK_SIZE,
    aligned::AlignedPages,
    budget::{Budget, BudgetAllocator},
    encoding::require,
};
use std::{io, sync::Arc};

pub const MAX_CHANGES: usize = 256 + 62;
pub const MAX_TRANSACTION_BYTES: usize = 128 * 1024 * 1024;

#[derive(Debug, Default, Clone, Copy, serde::Serialize)]
pub struct Stats {
    pub changes: usize,
    pub written_pages: usize,
    pub max_pages_per_change: usize,
    pub old_page_reads: u64,
    pub allocated_bytes: usize,
}

/// Owns every output byte until the caller has completed IO. It publishes
/// nothing itself; the manifest owner must compare previous(), write and sync.
/// The input tree must have passed recovery validation before editing it.
pub struct Prepared {
    buffer: AlignedPages<BudgetAllocator>,
    previous: Commit,
    commit: Commit,
    offset: u64,
    stats: Stats,
}

impl Prepared {
    pub fn build<P: PageReader>(
        source: &P,
        current: Commit,
        end: u64,
        edits: &[Extent],
        durable: u64,
        metadata: Arc<Budget>,
    ) -> io::Result<Self> {
        require(
            edits.len() <= MAX_CHANGES
                && durable >= current.durable
                && (edits.is_empty() || durable > current.durable),
            "manifest transaction bounds",
        )?;
        let blocks = current.image_bytes / BLOCK_SIZE as u64;
        for edit in edits {
            require(
                edit.start < edit.end
                    && edit.end <= blocks
                    && (edit.hash.is_none() || edit.end - edit.start == 1),
                "manifest edit range",
            )?;
        }
        let generation = current
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("manifest generations exhausted"))?;
        let pages = edits.len() * (8 * usize::from(MAX_HEIGHT) + 8) + 1;
        let bytes = pages * BLOCK_SIZE;
        require(
            bytes <= MAX_TRANSACTION_BYTES
                && end
                    .checked_add(bytes as u64)
                    .is_some_and(|n| n <= i64::MAX as u64),
            "manifest output bound",
        )?;
        let buffer = AlignedPages::new(BudgetAllocator::new(Arc::clone(&metadata)), pages);
        let tree = Tree::new(source, current, end, Arc::clone(&metadata))?;
        let mut editor = Editor {
            tree,
            buffer,
            root: current.root,
            pages: 0,
            max_pages_per_change: 0,
        };
        for edit in edits {
            let before = editor.pages;
            let bound = 8 * usize::from(editor.root.height.max(1)) + 8;
            editor.apply(*edit)?;
            let emitted = editor.pages - before;
            require(emitted <= bound, "COW edit exceeded 8H+8 pages")?;
            editor.max_pages_per_change = editor.max_pages_per_change.max(emitted);
        }
        // Draft paths may have been replaced by later edits. Emit only final
        // reachable pages into a separate bounded owner, rebasing child offsets.
        // Old on-disk pages remain immutable and are referenced directly.
        let mut buffer = AlignedPages::new(BudgetAllocator::new(metadata), pages);
        let root = if editor.root.offset == 0 {
            editor.root
        } else {
            let cursor = Cursor::root(Commit {
                root: editor.root,
                ..current
            });
            Root {
                offset: editor.emit(cursor, &mut buffer)?,
                height: editor.root.height,
            }
        };
        let commit = Commit {
            root,
            generation,
            durable,
            ..current
        };
        let offset = end + buffer.as_slice().len() as u64;
        commit.encode_into(buffer.push_zeroed()?, offset)?;
        let stats = Stats {
            changes: edits.len(),
            written_pages: buffer.as_slice().len() / BLOCK_SIZE,
            max_pages_per_change: editor.max_pages_per_change,
            old_page_reads: editor.tree.reads,
            allocated_bytes: buffer.allocated_bytes(),
        };
        Ok(Self {
            buffer,
            previous: current,
            commit,
            offset: end,
            stats,
        })
    }

    pub fn previous(&self) -> Commit {
        self.previous
    }
    pub fn commit(&self) -> Commit {
        self.commit
    }
    pub fn offset(&self) -> u64 {
        self.offset
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buffer.as_slice()[..self.stats.written_pages * BLOCK_SIZE]
    }
    pub fn stats(&self) -> Stats {
        self.stats
    }
}

struct Editor<'a, P: PageReader> {
    tree: Tree<'a, P>,
    buffer: AlignedPages<BudgetAllocator>,
    root: Root,
    pages: usize,
    max_pages_per_change: usize,
}

impl<P: PageReader> Editor<'_, P> {
    fn next_offset(&self) -> u64 {
        self.tree.end + (self.pages * BLOCK_SIZE) as u64
    }
    fn next_page(&mut self) -> io::Result<&mut [u8]> {
        self.buffer.push_zeroed()
    }

    fn emit(
        &mut self,
        cursor: Cursor,
        output: &mut AlignedPages<BudgetAllocator>,
    ) -> io::Result<u64> {
        if cursor.offset < self.tree.end {
            return Ok(cursor.offset);
        }
        let node = self.read(cursor)?;
        let image_bytes = self.tree.commit.image_bytes;
        match node {
            Node::Leaf(entries) => {
                let offset = self.tree.end + output.as_slice().len() as u64;
                format::leaf_into(output.push_zeroed()?, offset, image_bytes, &entries)?;
                Ok(offset)
            }
            Node::Branch(mut children) => {
                for index in 0..children.len() {
                    let next = cursor.child(&children, index);
                    children[index].offset = self.emit(next, output)?;
                }
                let offset = self.tree.end + output.as_slice().len() as u64;
                format::branch_into(
                    output.push_zeroed()?,
                    offset,
                    image_bytes,
                    cursor.level,
                    &children,
                )?;
                Ok(offset)
            }
        }
    }

    fn read(&mut self, cursor: Cursor) -> io::Result<Node> {
        if cursor.offset < self.tree.end {
            return self.tree.read_node(cursor);
        }
        let start = usize::try_from(cursor.offset - self.tree.end).map_err(io::Error::other)?;
        require(
            start.is_multiple_of(BLOCK_SIZE) && start < self.pages * BLOCK_SIZE,
            "manifest points to an unwritten transaction page",
        )?;
        let bytes = &self.buffer.as_slice()[start..start + BLOCK_SIZE];
        Node::decode(
            Page::decode(bytes, cursor.offset, self.tree.commit.image_bytes)?,
            cursor,
        )
    }

    fn leaves(&mut self, entries: &[Extent]) -> io::Result<Links> {
        let mut links = Links::new();
        // Split near the middle, leaving capacity for future inserts in either node.
        let width = if entries.len() > LEAF_CAPACITY {
            entries.len().div_ceil(2)
        } else {
            entries.len().max(1)
        };
        for part in entries.chunks(width) {
            let offset = self.next_offset();
            let image_bytes = self.tree.commit.image_bytes;
            format::leaf_into(self.next_page()?, offset, image_bytes, part)?;
            self.pages += 1;
            links
                .try_push(Child {
                    start: part[0].start,
                    offset,
                })
                .map_err(io::Error::other)?;
        }
        Ok(links)
    }

    fn branches(&mut self, level: u16, children: &[Child]) -> io::Result<Links> {
        let mut links = Links::new();
        let width = if children.len() > BRANCH_CAPACITY {
            children.len().div_ceil(2)
        } else {
            children.len().max(1)
        };
        for part in children.chunks(width) {
            let offset = self.next_offset();
            let image_bytes = self.tree.commit.image_bytes;
            format::branch_into(self.next_page()?, offset, image_bytes, level, part)?;
            self.pages += 1;
            links
                .try_push(Child {
                    start: part[0].start,
                    offset,
                })
                .map_err(io::Error::other)?;
        }
        Ok(links)
    }

    fn apply(&mut self, edit: Extent) -> io::Result<()> {
        let blocks = self.tree.commit.image_bytes / BLOCK_SIZE as u64;
        if edit.hash.is_none() && edit.start == 0 && edit.end == blocks {
            self.root = Root::default();
            return Ok(());
        }
        if self.root.offset == 0 {
            if edit.hash.is_some() {
                self.root = Root {
                    offset: self.leaves(&[edit])?[0].offset,
                    height: 1,
                };
            }
            return Ok(());
        }
        let cursor = Cursor {
            offset: self.root.offset,
            level: self.root.height - 1,
            minimum: None,
            lower: 0,
            upper: blocks,
        };
        let node = self.read(cursor)?;
        let links = self.modify(node, cursor, edit)?;
        self.root = match links.len() {
            0 => Root::default(),
            1 => Root {
                offset: links[0].offset,
                height: self.root.height,
            },
            _ => {
                require(self.root.height < MAX_HEIGHT, "manifest height exhausted")?;
                Root {
                    offset: self.branches(self.root.height, &links)?[0].offset,
                    height: self.root.height + 1,
                }
            }
        };
        while self.root.height > 1 {
            let cursor = Cursor {
                offset: self.root.offset,
                level: self.root.height - 1,
                minimum: None,
                lower: 0,
                upper: blocks,
            };
            let Node::Branch(children) = self.read(cursor)? else {
                unreachable!()
            };
            if children.len() != 1 {
                break;
            }
            self.root.offset = children[0].offset;
            self.root.height -= 1;
        }
        Ok(())
    }

    fn modify(&mut self, node: Node, cursor: Cursor, edit: Extent) -> io::Result<Links> {
        let unchanged = Child {
            start: node.minimum(),
            offset: cursor.offset,
        };
        match node {
            Node::Leaf(original) => {
                let mut entries = Leaves::new();
                for entry in &original {
                    if entry.end <= edit.start || entry.start >= edit.end {
                        entries.try_push(*entry).map_err(io::Error::other)?;
                    } else {
                        if entry.start < edit.start {
                            entries
                                .try_push(Extent {
                                    end: edit.start,
                                    ..*entry
                                })
                                .map_err(io::Error::other)?;
                        }
                        if entry.end > edit.end {
                            entries
                                .try_push(Extent {
                                    start: edit.end,
                                    ..*entry
                                })
                                .map_err(io::Error::other)?;
                        }
                    }
                }
                if edit.hash.is_some() {
                    entries.try_push(edit).map_err(io::Error::other)?;
                }
                entries.sort_unstable_by_key(|entry| entry.start);
                if entries == original {
                    Ok(one(unchanged))
                } else {
                    self.leaves(&entries)
                }
            }
            Node::Branch(original) => {
                let mut children = Children::new();
                for (index, child) in original.iter().enumerate() {
                    let next = cursor.child(&original, index);
                    if edit.end <= next.lower || edit.start >= next.upper {
                        children.try_push(*child).map_err(io::Error::other)?;
                    } else if !(edit.hash.is_none()
                        && edit.start <= next.lower
                        && edit.end >= next.upper)
                    {
                        let node = self.read(next)?;
                        let replacement = self.modify(node, next, edit)?;
                        children
                            .try_extend_from_slice(&replacement)
                            .map_err(io::Error::other)?;
                    }
                }
                if children == original {
                    Ok(one(unchanged))
                } else {
                    self.branches(cursor.level, &children)
                }
            }
        }
    }
}
