//! Image operations carry metadata and a verified logical WAL tail. Chunk
//! payloads travel independently through placement PUT, never provisioning.
use super::{Client, Flow, Metrics};
use std::{io, net::SocketAddr, sync::Arc};
pub(crate) const ROOT: u8 = 4;
pub(crate) const CLAIM: u8 = 5;
pub(crate) const EXPORT: u8 = 20;
pub(crate) const IMPORT: u8 = 23;
pub(crate) const IMPORT_WRITE: u8 = 24;
pub(crate) const IMPORT_FINISH: u8 = 25;
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    Provision,
    Migration,
}
impl Mode {
    pub(crate) fn flow(self) -> Flow {
        match self {
            Self::Provision => Flow::Provision,
            Self::Migration => Flow::Migration,
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Request {
    pub source: [u8; 16],
    pub target: [u8; 16],
    pub mode: Mode,
    pub destination: SocketAddr,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Metadata {
    pub store: [u8; 16],
    pub source: [u8; 16],
    pub image: [u8; 16],
    pub image_bytes: u64,
    pub segment_bytes: u64,
    pub d: u64,
    pub e: u64,
    pub mode: Mode,
    pub generation: u64,
    pub writer: u8,
    pub manifest_bytes: u64,
    pub manifest_hash: [u8; 32],
    pub tail_bytes: u64,
    pub tail_hash: [u8; 32],
}
impl Metadata {
    pub fn digest(&self) -> io::Result<[u8; 32]> {
        Ok(*blake3::hash(&serde_json::to_vec(self)?).as_bytes())
    }
    pub fn root(&self, phase: super::state::Phase) -> io::Result<super::state::Root> {
        Ok(super::state::Root {
            image: self.image,
            generation: self.generation,
            writer: self.writer,
            phase,
            durable: if self.mode == Mode::Migration {
                self.e
            } else {
                0
            },
            manifest: self.digest()?,
        })
    }
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ImageReport {
    pub image: [u8; 16],
    pub mode: Mode,
    pub manifest_bytes: u64,
    pub tail_bytes: u64,
    pub freeze_micros: u64,
    pub prepare_micros: u64,
    pub fence_micros: u64,
    pub transfer_micros: u64,
    pub install_micros: u64,
    pub finalize_micros: u64,
    pub total_micros: u64,
}
/// The destination must not already have a writable image with this identity.
/// Provision uses a fresh ID; migration preserves the ID and advances its fence.
pub fn transfer(
    source: SocketAddr,
    destination: SocketAddr,
    image: [u8; 16],
    target: [u8; 16],
    mode: Mode,
    metrics: Arc<Metrics>,
) -> io::Result<ImageReport> {
    let mut from = Client::connect(source, Flow::Control, metrics)?;
    let mut request = serde_json::to_vec(&Request {
        source: image,
        target,
        mode,
        destination,
    })?;
    let mut encoded = [0; 4096];
    let n = from.exchange(EXPORT, &mut request, &mut encoded)?;
    Ok(serde_json::from_slice(&encoded[..n])?)
}
