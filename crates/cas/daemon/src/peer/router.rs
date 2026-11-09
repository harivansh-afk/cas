//! Preallocated per-member GET connections; routing allocates no request memory.
use super::{Client, Config, Ticket, metrics::Metrics};
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator, BudgetArc};
use std::{io, sync::Arc};

pub struct Router {
    placement: Option<crate::fleet::Placement>,
    clients: Vec<(u8, Client), BudgetAllocator>,
}
impl Router {
    pub fn new(
        config: &Config,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
    ) -> io::Result<Self> {
        let placement = config.fleet.placement()?;
        let mut clients = crate::local::reserved_vec(config.peer_get_member.len().max(1), budget)?;
        if let Some(address) = config.peer_address {
            clients.push((
                placement.map_or(0, |p| 1 - p.node),
                Client::with_inline(address, budget, metrics, config.peer_inline_us)?,
            ));
        } else {
            for (node, address) in config.peer_get_member.iter().enumerate() {
                if placement.is_some_and(|p| usize::from(p.node) == node) {
                    continue;
                }
                clients.push((
                    node as u8,
                    Client::with_inline(*address, budget, metrics.clone(), config.peer_inline_us)?,
                ));
            }
        }
        Ok(Self { placement, clients })
    }
    pub fn fetch(&self, hash: [u8; 32]) -> io::Result<Ticket> {
        let (_, client) = self
            .clients
            .iter()
            .find(|(node, _)| self.placement.is_none_or(|p| p.owns(&hash, *node)))
            .ok_or(io::ErrorKind::NotFound)?;
        client.fetch(hash)
    }
    pub fn shutdown(&self) -> io::Result<()> {
        let mut result = Ok(());
        for (_, client) in &self.clients {
            if let Err(error) = client.shutdown() {
                result = Err(error);
            }
        }
        result
    }
}
