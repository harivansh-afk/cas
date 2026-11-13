//! Preallocated per-member GET connections; routing allocates no request memory.
use super::{Client, Config, Ticket, metrics::Metrics};
use allocator_api2::vec::Vec;
use cas_core::budget::{Budget, BudgetAllocator, BudgetArc};
use std::{io, sync::Arc};

struct Members {
    placement: Option<crate::fleet::Placement>,
    clients: Vec<(u8, Client), BudgetAllocator>,
}
/// Shared member connections seen from one image slot. The host owns the
/// unattributed router; each image gets a `for_image` view at creation, so a
/// GET verified by the transport is attributed to the issuing image exactly
/// once, whatever later readers coalesce on it.
pub struct Router {
    members: BudgetArc<Members>,
    image: usize,
}
impl Router {
    pub fn new(
        config: &Config,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
    ) -> io::Result<Self> {
        let placement = config.fleet.placement()?;
        // Check the aggregate requirement before allocating any client.
        super::client::reserve_descriptors(if config.peer_address.is_some() {
            1
        } else {
            config
                .peer_get_member
                .len()
                .saturating_sub(usize::from(placement.is_some()))
        })?;
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
        Ok(Self {
            members: BudgetArc::try_new(Members { placement, clients }, budget)?,
            image: super::metrics::UNATTRIBUTED,
        })
    }
    /// The same connections, attributing verified GETs to `image`'s slot.
    pub fn for_image(&self, image: usize) -> Self {
        Self {
            members: self.members.clone(),
            image,
        }
    }
    pub fn fetch(&self, hash: [u8; 32]) -> io::Result<Ticket> {
        let members = &*self.members;
        let (_, client) = members
            .clients
            .iter()
            .find(|(node, _)| members.placement.is_none_or(|p| p.owns(&hash, *node)))
            .ok_or(io::ErrorKind::NotFound)?;
        client.fetch_for(hash, self.image)
    }
    pub fn shutdown(&self) -> io::Result<()> {
        let mut result = Ok(());
        for (_, client) in &self.members.clients {
            if let Err(error) = client.shutdown() {
                result = Err(error);
            }
        }
        result
    }
}
