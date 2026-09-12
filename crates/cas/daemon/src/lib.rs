//! Host adapter components shared with protocol and recovery tests.

#[cfg(all(
    target_os = "linux",
    target_endian = "little",
    target_has_atomic = "64"
))]
pub mod inflight;

pub mod backend;
mod deadline;
pub mod fault;
mod local;
mod request;
pub mod storage;
pub use local::host::recovery;
pub use local::host::{CollectionHandle, CollectionReport, Host, Quiescence, Resources, Roots};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum BackendKind {
    Raw,
    Staging,
    Local,
    LocalAsync,
}
