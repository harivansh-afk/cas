//! Immutable fixed-chunk storage primitives.
pub mod format;

#[cfg(target_os = "linux")]
pub mod file;
