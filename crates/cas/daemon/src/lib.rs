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
pub mod host_service;
mod local;
mod read_trace;
mod request;
pub mod service;
pub mod storage;
pub use local::host::fault::{Point as CompactionPoint, Selection as CompactionPause};
pub use local::host::{
    CollectionHandle, CollectionReport, Host, Quiescence, Resources, Roots, SnapshotHandle,
    SnapshotReport,
};
pub use local::host::{initialize, recovery};

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum BackendKind {
    Raw,
    Staging,
    Local,
    LocalAsync,
}
