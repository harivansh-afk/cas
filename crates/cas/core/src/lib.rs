//! Host storage for guest block images: a packed staging WAL with recovery,
//! a content-addressed chunk store, copy-on-write manifests, a catalog of
//! images, shared memory budgets, disk accounting, and Linux direct IO.

pub const BLOCK_SIZE: usize = 4096;

/// Maximum byte payload accepted by staging and VM adapter
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

pub mod aligned;
pub mod budget;
pub mod cache;
pub mod census;
pub mod chunk;
pub mod chunk_index;
mod encoding;
pub mod io_metrics;
pub mod manifest;
pub mod space;
pub mod store;

#[cfg(target_os = "linux")]
pub mod append;
#[cfg(target_os = "linux")]
pub mod catalog;
#[cfg(target_os = "linux")]
mod direct;
#[cfg(target_os = "linux")]
mod directory;
#[cfg(target_os = "linux")]
mod eventfd;
#[cfg(target_os = "linux")]
pub mod scheduler;
#[cfg(target_os = "linux")]
pub mod segments;
#[cfg(target_os = "linux")]
pub mod staging;

#[cfg(target_os = "linux")]
pub use direct::Alignment;
