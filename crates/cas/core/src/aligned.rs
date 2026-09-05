//! Block-aligned buffers for direct IO.
use crate::BLOCK_SIZE;

// Rust requires a literal in repr(align); the assertion ties it to BLOCK_SIZE.
#[repr(C, align(4096))]
#[derive(Clone)]
struct Block([u8; BLOCK_SIZE]);

const _: () = assert!(size_of::<Block>() == BLOCK_SIZE && align_of::<Block>() == BLOCK_SIZE);

/// Contiguous initialized blocks; byte views borrow this allocation.
pub struct AlignedBuffer(Vec<Block>);

impl AlignedBuffer {
    /// Allocates a zero-filled buffer.
    ///
    /// # Panics
    ///
    /// Panics if `length` is zero or not a multiple of [`BLOCK_SIZE`].
    pub fn new(length: usize) -> Self {
        assert!(length > 0 && length.is_multiple_of(BLOCK_SIZE));
        Self(vec![Block([0; BLOCK_SIZE]); length / BLOCK_SIZE])
    }

    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: Block is exactly 4096 initialized bytes with no padding, and
        // Vec stores its blocks contiguously. The slice borrows the allocation.
        unsafe { std::slice::from_raw_parts(self.0.as_ptr().cast(), self.0.len() * BLOCK_SIZE) }
    }

    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: the blocks contain initialized bytes with no padding, and
        // &mut self gives this slice exclusive access to the entire allocation.
        unsafe {
            std::slice::from_raw_parts_mut(self.0.as_mut_ptr().cast(), self.0.len() * BLOCK_SIZE)
        }
    }
}
