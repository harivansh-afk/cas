//! Research storage primitives. The synchronous staging log is a correctness
//! reference; the QEMU/io_uring daemon and compaction pipeline are not built yet.

pub mod watermark;

#[cfg(target_os = "linux")]
mod direct;
#[cfg(target_os = "linux")]
pub mod staging;

pub const BLOCK_SIZE: usize = 4096;
