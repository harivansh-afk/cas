use super::super::format::{BRANCH_CAPACITY, Child, Extent, Kind, LEAF_CAPACITY, Page};
use crate::encoding::require;
use arrayvec::ArrayVec;
use std::io;

pub(super) type Leaves = ArrayVec<Extent, { LEAF_CAPACITY + 2 }>;
pub(super) type Children = ArrayVec<Child, { BRANCH_CAPACITY + 2 }>;
pub(super) type Links = ArrayVec<Child, 2>;

#[derive(Clone, Copy)]
pub(super) struct Cursor {
    pub offset: u64,
    pub level: u16,
    pub minimum: Option<u64>,
    pub lower: u64,
    pub upper: u64,
}

impl Cursor {
    pub fn child(self, children: &[Child], index: usize) -> Self {
        let child = children[index];
        Self {
            offset: child.offset,
            level: self.level - 1,
            minimum: Some(child.start),
            lower: if index == 0 { self.lower } else { child.start },
            upper: children
                .get(index + 1)
                .map_or(self.upper, |next| next.start),
        }
    }
}

#[expect(
    clippy::large_enum_variant,
    reason = "Both variants use one page of bounded stack scratch; boxing would allocate per node"
)]
pub(super) enum Node {
    Leaf(Leaves),
    Branch(Children),
}

const _: () = assert!(size_of::<Node>() <= crate::BLOCK_SIZE);

impl Node {
    pub fn decode(page: Page<'_>, cursor: Cursor) -> io::Result<Self> {
        require(page.level() == cursor.level, "manifest child level")?;
        let node = match page.kind() {
            Kind::Leaf => {
                let entries: Leaves = page.extents()?.collect();
                require(
                    entries
                        .iter()
                        .all(|e| e.start >= cursor.lower && e.end <= cursor.upper),
                    "manifest leaf escapes parent bounds",
                )?;
                Self::Leaf(entries)
            }
            Kind::Branch => {
                let children: Children = page.children()?.collect();
                require(
                    children
                        .iter()
                        .all(|c| c.start >= cursor.lower && c.start < cursor.upper),
                    "manifest branch escapes parent bounds",
                )?;
                Self::Branch(children)
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "tree points to a non-tree page",
                ));
            }
        };
        require(
            cursor
                .minimum
                .is_none_or(|minimum| minimum == node.minimum()),
            "manifest child minimum differs from parent",
        )?;
        Ok(node)
    }

    pub fn minimum(&self) -> u64 {
        match self {
            Self::Leaf(entries) => entries[0].start,
            Self::Branch(children) => children[0].start,
        }
    }
}

pub(super) fn one(link: Child) -> Links {
    let mut links = Links::new();
    links.push(link);
    links
}
