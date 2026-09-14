// Block-aligned buffers for direct IO.
use crate::BLOCK_SIZE;
use allocator_api2::{
    alloc::{Allocator, Global},
    boxed::Box,
};
use std::io;

// Rust requires a literal in repr(align)
#[repr(C, align(4096))]
#[derive(Clone)]
struct Block([u8; BLOCK_SIZE]);

const _: () = assert!(size_of::<Block>() == BLOCK_SIZE && align_of::<Block>() == BLOCK_SIZE);

/// Contiguous initialized blocks; byte views borrow this allocation.
pub struct AlignedBuffer<A: Allocator = Global>(Box<[Block], A>);

impl AlignedBuffer {
    /// Allocates a zero-filled buffer.
    ///
    /// Panics if `length` is zero or not multiple of BLOCK_SIZE
    pub fn new(length: usize) -> Self {
        Self::try_new_in(length, Global).expect("aligned buffer allocation failed")
    }
}

impl<A: Allocator> AlignedBuffer<A> {
    /// Allocate through the supplied allocator; budgeted callers are charged
    /// for the exact slice layout before Global obtains any storage.
    pub fn try_new_in(length: usize, allocator: A) -> io::Result<Self> {
        if length == 0 || !length.is_multiple_of(BLOCK_SIZE) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unaligned buffer length",
            ));
        }
        let allocation = crate::io_metrics::measure(length as u64, |c| &mut c.buffer_allocate);
        let mut blocks = Box::<[Block], A>::try_new_uninit_slice_in(length / BLOCK_SIZE, allocator)
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::OutOfMemory,
                    "aligned buffer allocation denied",
                )
            })?;
        drop(allocation);
        let _zeroing = crate::io_metrics::measure(length as u64, |c| &mut c.buffer_zero);
        for block in blocks.iter_mut() {
            block.write(Block([0; BLOCK_SIZE]));
        }
        // SAFETY: every element was initialized above, and Block contains no
        // padding or invalid bit patterns. The allocator and layout stay intact.
        Ok(Self(unsafe { blocks.assume_init() }))
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: Block is exactly 4096 initialized bytes with no padding
        //
        // The boxed slice stores its blocks contiguously.
        // The slice borrows the allocation
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * BLOCK_SIZE) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the blocks contain initialized bytes with no padding
        //
        // &mut self gives this slice exclusive access to the entire allocation.
        unsafe {
            std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), self.0.len() * BLOCK_SIZE)
        }
    }
}

/// Bounded preparation scratch. Grows before IO and exposes initialized pages only.
/// Submitted IO continues to own fixed buffers; this type never resizes in flight.
pub(crate) struct AlignedPages<A: Allocator> {
    blocks: allocator_api2::vec::Vec<Block, A>,
    limit: usize,
}

impl<A: Allocator> AlignedPages<A> {
    pub fn new(allocator: A, limit: usize) -> Self {
        Self {
            blocks: allocator_api2::vec::Vec::new_in(allocator),
            limit,
        }
    }

    pub fn push_zeroed(&mut self) -> io::Result<&mut [u8]> {
        if self.blocks.len() == self.limit {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "page plan exhausted",
            ));
        }
        if self.blocks.len() == self.blocks.capacity() {
            let capacity = self
                .blocks
                .capacity()
                .max(1)
                .saturating_mul(2)
                .min(self.limit);
            let _allocation = crate::io_metrics::measure((capacity * BLOCK_SIZE) as u64, |c| {
                &mut c.buffer_allocate
            });
            self.blocks
                .try_reserve_exact(capacity - self.blocks.len())
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::OutOfMemory, "page allocation denied")
                })?;
        }
        let _zeroing = crate::io_metrics::measure(BLOCK_SIZE as u64, |c| &mut c.buffer_zero);
        self.blocks.push(Block([0; BLOCK_SIZE]));
        Ok(&mut self.blocks.last_mut().unwrap().0)
    }

    pub fn allocated_bytes(&self) -> usize {
        self.blocks.capacity() * BLOCK_SIZE
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: contiguous, initialized Block values have no padding; the
        // borrow prevents growth or mutation while the returned slice is live.
        unsafe {
            std::slice::from_raw_parts(self.blocks.as_ptr().cast(), self.blocks.len() * BLOCK_SIZE)
        }
    }
}
