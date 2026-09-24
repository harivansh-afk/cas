//! Shared experiment runners, process ownership and evidence readers.
/// Loopback port guests forward SSH to; lab guest N listens on BASE + N.
pub const GUEST_SSH_PORT_BASE: u16 = 23479;

pub mod evidence;
pub mod filesystem;
pub mod fixture;
pub mod fleet;
pub mod host;
pub mod lab;
pub mod native;
pub mod persistence;
pub mod pressure;
pub mod process;
pub mod qemu;
pub mod shared;
pub mod source;
pub mod suite;
pub mod vm;
