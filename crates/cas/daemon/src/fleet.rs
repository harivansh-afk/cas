//! Static two-member placement and the separate, bounded bulk/control transport.
pub(crate) mod chunks;
pub(crate) mod collection;
pub use collection::collect;
pub(crate) mod image;
pub(crate) mod state;
mod wire;
pub use image::{ImageReport, Mode, transfer};
use std::{io, net::SocketAddr};
pub use wire::{Client, Flow, Handler, Metrics, Server};

/// Application TCP bytes per flow, excluding TCP/IP framing and retransmits.
pub fn report(address: SocketAddr) -> io::Result<serde_json::Value> {
    let mut client = Client::connect(
        address,
        Flow::Control,
        std::sync::Arc::new(Metrics::default()),
    )?;
    let mut bytes = [0; 64 * 1024];
    let n = client.exchange(7, &mut [], &mut bytes)?;
    Ok(serde_json::from_slice(&bytes[..n])?)
}

#[cfg(test)]
pub(crate) fn crash_point(point: &str) {
    if std::env::var("CAS_FLEET_PAUSE").as_deref() == Ok(point) {
        let marker = std::env::var_os("CAS_FLEET_MARKER").expect("crash harness marker");
        std::fs::write(marker, point).unwrap();
        loop {
            std::thread::park();
        }
    }
}

#[derive(Clone, Debug, clap::Args)]
#[group(id = "FleetConfig")]
pub struct Config {
    /// Stable member identity, 0 or 1. Enables two-host placement.
    #[arg(long, value_parser = clap::value_parser!(u8).range(0..=1))]
    pub peer_node: Option<u8>,
    /// Owners per chunk: 1 partitions; 2 replicates on both static members.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub peer_k: u8,
    /// Dedicated bulk/control listener, separate from latency-sensitive GET.
    #[arg(long)]
    pub peer_bulk_listen: Option<SocketAddr>,
    /// The other member's bulk/control listener.
    #[arg(long)]
    pub peer_bulk_address: Option<SocketAddr>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            peer_node: None,
            peer_k: 2,
            peer_bulk_listen: None,
            peer_bulk_address: None,
        }
    }
}
impl Config {
    pub fn placement(&self) -> io::Result<Option<Placement>> {
        if let Some(node) = self.peer_node {
            let placement = Placement::new(node, self.peer_k)?;
            let (Some(listen), Some(peer)) = (self.peer_bulk_listen, self.peer_bulk_address) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "fleet requires both bulk addresses",
                ));
            };
            if listen == peer || peer.port() == 0 || peer.ip().is_unspecified() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid fleet peer address",
                ));
            }
            Ok(Some(placement))
        } else if self.peer_bulk_listen.is_some() || self.peer_bulk_address.is_some() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "bulk service requires a stable member identity",
            ))
        } else {
            Ok(None)
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Placement {
    pub node: u8,
    pub k: u8,
}
impl Placement {
    pub fn new(node: u8, k: u8) -> io::Result<Self> {
        if node > 1 || !(1..=2).contains(&k) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self { node, k })
    }
    pub fn owns(self, hash: &[u8; 32], node: u8) -> bool {
        if node > 1 {
            return false;
        }
        if self.k == 2 {
            return true;
        }
        let score = |member: u8| {
            let mut h = blake3::Hasher::new();
            h.update(b"CAS rendezvous v1\0");
            h.update(hash);
            h.update(&[member]);
            *h.finalize().as_bytes()
        };
        // Lexicographic digest score; stable member ID breaks even a full collision.
        let winner = u8::from((score(1), 1u8) > (score(0), 0u8));
        node == winner
    }
    pub fn local(self, hash: &[u8; 32]) -> bool {
        self.owns(hash, self.node)
    }
    pub fn remote(self, hash: &[u8; 32]) -> bool {
        self.owns(hash, 1 - self.node)
    }
}

/// Reconcile this member's durable chunks with its peer, sending only missing
/// owner content. Each host runs its own pass after independently drifting.
pub fn synchronize(address: SocketAddr, metrics: std::sync::Arc<Metrics>) -> io::Result<u64> {
    let mut client = Client::connect(address, Flow::Control, metrics)?;
    let mut reply = [0; 8];
    if client.exchange(chunks::SYNC, &mut [], &mut reply)? != 8 {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(u64::from_le_bytes(reply))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rendezvous_is_identical_on_both_members_and_k_selects_exactly_k() {
        let a = Placement::new(0, 1).unwrap();
        let b = Placement::new(1, 1).unwrap();
        let mut counts = [0; 2];
        for n in 0u64..4096 {
            let hash = *blake3::hash(&n.to_le_bytes()).as_bytes();
            assert_eq!(a.local(&hash), b.remote(&hash));
            assert_ne!(a.local(&hash), b.local(&hash));
            counts[usize::from(b.local(&hash))] += 1;
            let replicated = Placement::new(0, 2).unwrap();
            assert!(replicated.local(&hash) && replicated.remote(&hash));
        }
        assert!(counts.iter().all(|n| (1800..2300).contains(n)));
        assert!(Placement::new(2, 1).is_err());
        assert!(Placement::new(0, 0).is_err());
    }
}
