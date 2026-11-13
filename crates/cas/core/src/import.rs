//! Offline ingestion of a raw image through the normal durable write path.
//! Defaults are platform-free so command-line parsers build everywhere; the
//! implementation needs Linux direct IO.

/// WAL and chunk segment capacity for a new root.
pub const DEFAULT_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;
/// Input bytes staged between FLUSH/compaction/reclamation passes.
pub const DEFAULT_COMPACTION_INTERVAL: u64 = 64 * 1024 * 1024;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;
