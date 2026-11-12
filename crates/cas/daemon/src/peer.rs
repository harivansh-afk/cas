//! Static member GET transport, independent of the bulk/control connections.
mod client;
mod race;
mod router;
pub use router::Router;
pub mod metrics;
mod server;
mod wire;
pub use client::{Client, Ticket};
pub(crate) use server::CONNECTIONS;
#[cfg(test)]
pub(crate) use server::TEST_REPLY_DELAY_US;
pub use server::{Server, has};
use std::{io, net::SocketAddr, time::Duration};

#[derive(Clone, Debug, Default, clap::Args)]
pub struct Config {
    #[command(flatten)]
    pub fleet: crate::fleet::Config,
    /// Opt-in TCP chunk service; use a private interface on an isolated testbed.
    #[arg(long)]
    pub peer_listen: Option<SocketAddr>,
    /// The other member of this static two-host configuration.
    #[arg(long)]
    pub peer_address: Option<SocketAddr>,
    /// Ordered GET endpoints for every fleet member (repeat in bulk-member order).
    #[arg(long)]
    pub peer_get_member: Vec<SocketAddr>,
    /// Disable the local settled-chunk copy for remote-read experiments; WAL wins.
    #[arg(long)]
    pub peer_force_remote: bool,
    /// Cache verified remote chunks at the reader (off for remote latency runs).
    #[arg(long)]
    pub peer_cache: bool,
    /// Bounded reactor-local TCP polling before falling back to the IO worker.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u32).range(0..=1000))]
    pub peer_inline_us: u32,
    /// Race a settled GET against a second source after this many microseconds
    /// (0 races immediately); off when absent. The first verified reply wins.
    #[arg(long, value_parser = clap::value_parser!(u32).range(0..=1_000_000))]
    pub peer_race_us: Option<u32>,
    /// With force-remote, let the race use the local chunk copy explicitly.
    #[arg(long, requires = "peer_race_us")]
    pub peer_race_local: bool,
    /// Complete fully settled reads inline on the frontend (clean cache or a
    /// verified remote reply) while the image has no staged or in-flight
    /// mutation and no pending FLUSH; the reactor path remains the default.
    #[arg(long)]
    pub fast_settled_reads: bool,
}
impl Config {
    pub fn validate(&self) -> io::Result<()> {
        if self.fleet.placement()?.is_some()
            && (self.peer_listen.is_none()
                || (self.peer_address.is_none() && self.peer_get_member.is_empty()))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "fleet requires GET addresses",
            ));
        }
        if self.peer_inline_us > 1000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "peer inline polling exceeds 1000 microseconds",
            ));
        }
        if (self.peer_force_remote || self.peer_cache || self.peer_race_us.is_some())
            && self.peer_address.is_none()
            && self.peer_get_member.is_empty()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "remote mode requires a peer",
            ));
        }
        if self.peer_race_local && (self.peer_race_us.is_none() || !self.peer_force_remote) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "racing the local copy requires --peer-race-us and --peer-force-remote",
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
        if !self.peer_get_member.is_empty() {
            let placement = self.fleet.placement()?.ok_or(io::ErrorKind::InvalidInput)?;
            if self.peer_address.is_some()
                || self.peer_get_member.len() != usize::from(placement.members)
                || self.peer_listen
                    != self
                        .peer_get_member
                        .get(usize::from(placement.node))
                        .copied()
                || self.peer_get_member.iter().enumerate().any(|(i, a)| {
                    a.port() == 0
                        || a.ip().is_unspecified()
                        || self.peer_get_member[..i].contains(a)
                })
            {
                return Err(io::ErrorKind::InvalidInput.into());
            }
        } else if self.fleet.placement()?.is_some_and(|p| p.members != 2) {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(())
    }
}
pub const TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(test)]
mod tests;
