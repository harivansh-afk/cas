//! Preallocated per-member GET connections; routing allocates no request memory.
use super::{
    Client, Config, Ticket,
    metrics::Metrics,
    race::{self, Armed, Kind, LocalSource, Racer},
};
use allocator_api2::vec::Vec;
use cas_core::{
    budget::{Budget, BudgetAllocator, BudgetArc},
    store::file::Read as Plan,
};
use std::{io, sync::Arc, time::Duration};

pub(crate) struct Shared {
    placement: Option<crate::fleet::Placement>,
    pub(crate) clients: Vec<(u8, Client), BudgetAllocator>,
    pub(crate) local: Option<LocalSource>,
}

pub struct Router {
    shared: BudgetArc<Shared>,
    metrics: BudgetArc<Metrics>,
    racer: Option<Racer>,
    budget: Option<Duration>,
    budget_us: u32,
}
impl Router {
    /// `local` supplies an IO scheduler demand port when the force-remote
    /// experiment races the local copy (`--peer-race-local`).
    pub fn new(
        config: &Config,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
        local: Option<cas_core::scheduler::Port>,
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
                Client::with_inline(address, budget, metrics.clone(), config.peer_inline_us)?,
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
        let local = match (config.peer_race_us, local) {
            (Some(_), Some(scheduling)) if config.peer_race_local => {
                Some(LocalSource::new(scheduling, budget, metrics.clone())?)
            }
            _ => None,
        };
        let shared = BudgetArc::try_new(
            Shared {
                placement,
                clients,
                local,
            },
            budget,
        )?;
        let budget_us = config.peer_race_us.unwrap_or(0);
        let racer = match config.peer_race_us {
            Some(us) if us > 0 => Some(Racer::new(budget, shared.clone(), metrics.clone())?),
            _ => None,
        };
        Ok(Self {
            shared,
            metrics,
            racer,
            budget: config
                .peer_race_us
                .map(|us| Duration::from_micros(u64::from(us))),
            budget_us,
        })
    }
    fn owners(&self, hash: &[u8; 32]) -> impl Iterator<Item = usize> + '_ {
        let placement = self.shared.placement;
        let hash = *hash;
        self.shared
            .clients
            .iter()
            .enumerate()
            .filter(move |(_, (node, _))| placement.is_none_or(|p| p.owns(&hash, *node)))
            .map(|(index, _)| index)
    }
    pub fn fetch(&self, hash: [u8; 32]) -> io::Result<Ticket> {
        let index = self.owners(&hash).next().ok_or(io::ErrorKind::NotFound)?;
        self.shared.clients[index].1.fetch(hash)
    }
    /// A primary GET that also signals `notify` on completion (frontend wake).
    pub fn fetch_with_notify(
        &self,
        hash: [u8; 32],
        notify: vmm_sys_util::eventfd::EventFd,
    ) -> io::Result<Ticket> {
        let index = self.owners(&hash).next().ok_or(io::ErrorKind::NotFound)?;
        self.shared.clients[index]
            .1
            .fetch_with(hash, Some(notify), true, false)
    }
    /// True when a verified local plan should be captured for the race; the
    /// reader otherwise never consults the local store in force-remote mode.
    pub fn wants_local_plan(&self) -> bool {
        self.budget.is_some() && self.shared.local.is_some()
    }
    /// Arm the race for `ticket`: the next owning member if the fleet has
    /// one, else the local copy when a plan was captured. No second source
    /// leaves the read unraced. A zero budget starts the challenger now.
    pub fn race(&self, ticket: &Ticket, hash: [u8; 32], local: Option<Plan>) {
        let Some(budget) = self.budget else {
            return;
        };
        let kind = match self.owners(&hash).nth(1) {
            Some(index) => Kind::Remote(index),
            None => match (local, &self.shared.local) {
                (Some(plan), Some(_)) => Kind::Local(plan),
                _ => return,
            },
        };
        let armed = Armed {
            deadline: std::time::Instant::now() + budget,
            hash,
            first: ticket.slot_ref(),
            kind,
            budget_us: self.budget_us,
        };
        match &self.racer {
            Some(racer) if !budget.is_zero() => racer.arm(armed),
            _ => race::start(
                &self.shared.clients,
                self.shared.local.as_ref(),
                &self.metrics,
                armed,
            ),
        }
    }
    pub fn shutdown(&self) -> io::Result<()> {
        let mut result = Ok(());
        if let Some(racer) = &self.racer
            && let Err(error) = racer.shutdown()
        {
            result = Err(error);
        }
        if let Some(local) = &self.shared.local
            && let Err(error) = local.shutdown()
        {
            result = Err(error);
        }
        for (_, client) in &self.shared.clients {
            if let Err(error) = client.shutdown() {
                result = Err(error);
            }
        }
        result
    }
}
