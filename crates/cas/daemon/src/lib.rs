//! Host adapter components shared with protocol and recovery tests.

#[cfg(all(
    target_os = "linux",
    target_endian = "little",
    target_has_atomic = "64"
))]
pub mod inflight;
