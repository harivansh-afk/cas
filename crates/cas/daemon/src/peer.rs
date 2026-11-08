//! Static two-host read transport. No durability or placement protocol.
mod client;
pub mod metrics;
mod server;
mod wire;
pub use client::{Client, Ticket};
pub(crate) use server::CONNECTIONS;
pub use server::{Server, has};
use std::{io, net::SocketAddr, time::Duration};

#[derive(Clone, Debug, Default, clap::Args)]
pub struct Config {
    /// Opt-in TCP chunk service; use a private interface on an isolated testbed.
    #[arg(long)]
    pub peer_listen: Option<SocketAddr>,
    /// The other member of this static two-host configuration.
    #[arg(long)]
    pub peer_address: Option<SocketAddr>,
    /// Disable the local settled-chunk copy for remote-read experiments; WAL wins.
    #[arg(long, requires = "peer_address")]
    pub peer_force_remote: bool,
    /// Cache verified remote chunks at the reader (off for remote latency runs).
    #[arg(long, requires = "peer_address")]
    pub peer_cache: bool,
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if (self.peer_force_remote || self.peer_cache) && self.peer_address.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote mode requires a peer",
            ));
        }
        if self
            .peer_address
            .is_some_and(|a| a.port() == 0 || a.ip().is_unspecified())
            || self.peer_address.is_some() && self.peer_address == self.peer_listen
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid peer address",
            ));
        }
        Ok(())
    }
}
pub const TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests;
