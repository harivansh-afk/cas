// Staging log, durability tracking, and shared IO buffers.

pub mod budget;
pub mod census;
pub mod chunk;
pub mod chunk_index;
pub mod watermark;

#[cfg(target_os = "linux")]
mod direct;

#[cfg(target_os = "linux")]
pub mod staging;

#[cfg(target_os = "linux")]
pub mod append;

pub const BLOCK_SIZE: usize = 4096;

pub mod aligned;

/// Maximum byte payload accepted by staging and VM adapter
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

mod encoding;
pub mod manifest;
pub mod store;
