//! Staging log, durability tracking, and shared IO buffers.

pub mod watermark;

#[cfg(target_os = "linux")]
mod direct;

#[cfg(target_os = "linux")]
pub mod staging;

pub const BLOCK_SIZE: usize = 4096;

pub mod aligned;

/// Maximum byte payload accepted by staging and the VM adapter.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;
