//! Static membership placement and the separate, bounded bulk/control transport.
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
    // Administrative snapshots include the bounded remote-read timing ring.
    let mut bytes = vec![0; wire::MAX_BODY];
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

/// Test-only owner delay: holds a PUT before its durable insert so the shipping
/// window ring can be checked against a known acknowledgement delay.
#[cfg(test)]
pub(crate) fn delay_point(point: &str) {
    if std::env::var("CAS_FLEET_DELAY").as_deref() == Ok(point) {
        let millis: u64 = std::env::var("CAS_FLEET_DELAY_MS")
            .expect("delay harness duration")
            .parse()
            .expect("delay milliseconds");
        std::thread::sleep(std::time::Duration::from_millis(millis));
    }
}

#[derive(Clone, Debug, clap::Args)]
#[group(id = "FleetConfig")]
pub struct Config {
    /// Stable member identity: index in the ordered membership list.
    #[arg(long, value_parser = clap::value_parser!(u8))]
    pub peer_node: Option<u8>,
    /// Owners per chunk: 1 partitions; N replicates on every member.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u8).range(1..))]
    pub peer_k: u8,
    /// Dedicated bulk/control listener, separate from latency-sensitive GET.
    #[arg(long)]
    pub peer_bulk_listen: Option<SocketAddr>,
    /// The other member's bulk/control listener.
    #[arg(long)]
    pub peer_bulk_address: Option<SocketAddr>,
    /// Ordered bulk endpoints for all members, including this host (repeat per member).
    #[arg(long)]
    pub peer_bulk_member: Vec<SocketAddr>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            peer_node: None,
            peer_k: 2,
            peer_bulk_listen: None,
            peer_bulk_address: None,
            peer_bulk_member: Vec::new(),
        }
    }
}
impl Config {
    pub fn addresses(&self) -> io::Result<Vec<SocketAddr>> {
        let node = self.peer_node.ok_or(io::ErrorKind::InvalidInput)?;
        let addresses = if self.peer_bulk_member.is_empty() {
            if node > 1 {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            let listen = self.peer_bulk_listen.ok_or(io::ErrorKind::InvalidInput)?;
            let peer = self.peer_bulk_address.ok_or(io::ErrorKind::InvalidInput)?;
            if node == 0 {
                vec![listen, peer]
            } else {
                vec![peer, listen]
            }
        } else {
            if self.peer_bulk_address.is_some() {
                return Err(io::ErrorKind::InvalidInput.into());
            }
            self.peer_bulk_member.clone()
        };
        if !(2..=255).contains(&addresses.len())
            || usize::from(node) >= addresses.len()
            || addresses.iter().enumerate().any(|(i, a)| {
                a.port() == 0 || a.ip().is_unspecified() || addresses[..i].contains(a)
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid or duplicate fleet member endpoint",
            ));
        }
        Ok(addresses)
    }
    pub fn peers(&self) -> io::Result<Vec<(u8, SocketAddr)>> {
        Ok(self
            .addresses()?
            .into_iter()
            .enumerate()
            .filter(|(i, _)| Some(*i as u8) != self.peer_node)
            .map(|(i, a)| (i as u8, a))
            .collect())
    }
    pub fn address(&self, node: u8) -> io::Result<SocketAddr> {
        self.addresses()?
            .get(usize::from(node))
            .copied()
            .ok_or_else(|| io::ErrorKind::InvalidInput.into())
    }

    pub fn placement(&self) -> io::Result<Option<Placement>> {
        if let Some(node) = self.peer_node {
            let addresses = self.addresses()?;
            let placement = Placement::with_members(node, self.peer_k, addresses.len())?;
            if self.peer_bulk_listen != Some(addresses[usize::from(node)]) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "local bulk address differs from membership",
                ));
            }
            Ok(Some(placement))
        } else if self.peer_bulk_listen.is_some()
            || self.peer_bulk_address.is_some()
            || !self.peer_bulk_member.is_empty()
        {
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
    #[serde(default = "two_members")]
    pub members: u8,
}
fn two_members() -> u8 {
    2
}
impl Placement {
    pub fn new(node: u8, k: u8) -> io::Result<Self> {
        Self::with_members(node, k, 2)
    }
    pub fn with_members(node: u8, k: u8, members: usize) -> io::Result<Self> {
        if !(2..=255).contains(&members)
            || usize::from(node) >= members
            || k == 0
            || usize::from(k) > members
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(Self {
            node,
            k,
            members: members as u8,
        })
    }
    fn score(hash: &[u8; 32], member: u8) -> ([u8; 32], u8) {
        let mut h = blake3::Hasher::new();
        h.update(b"CAS rendezvous v1\0");
        h.update(hash);
        h.update(&[member]);
        (*h.finalize().as_bytes(), member)
    }
    pub fn owns(self, hash: &[u8; 32], node: u8) -> bool {
        if node >= self.members {
            return false;
        }
        if self.k == self.members {
            return true;
        }
        let score = Self::score(hash, node);
        (0..self.members)
            .filter(|member| Self::score(hash, *member) > score)
            .take(usize::from(self.k))
            .count()
            < usize::from(self.k)
    }
    pub fn local(self, hash: &[u8; 32]) -> bool {
        self.owns(hash, self.node)
    }
    pub fn remote(self, hash: &[u8; 32]) -> bool {
        (0..self.members).any(|node| node != self.node && self.owns(hash, node))
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
    fn four_member_placement_has_exactly_k_stable_owners() {
        for k in [1, 2, 4] {
            let mut counts = [0; 4];
            for n in 0u64..4096 {
                let hash = *blake3::hash(&n.to_le_bytes()).as_bytes();
                let p = Placement::with_members(0, k, 4).unwrap();
                let owners: Vec<_> = (0..4).filter(|node| p.owns(&hash, *node)).collect();
                assert_eq!(owners.len(), usize::from(k));
                for node in owners {
                    counts[usize::from(node)] += 1;
                    assert!(Placement::with_members(node, k, 4).unwrap().local(&hash));
                }
            }
            assert!(
                counts
                    .iter()
                    .all(|n| (*n - 1024 * i32::from(k)).abs() < 150)
            );
        }
        assert!(Placement::with_members(4, 1, 4).is_err());
        assert!(Placement::with_members(0, 5, 4).is_err());
    }
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
