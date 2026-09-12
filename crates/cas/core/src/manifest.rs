//! Durable copy-on-write image mappings with file-local page addresses.
pub mod format;
pub mod tree;

#[cfg(target_os = "linux")]
pub mod file;
