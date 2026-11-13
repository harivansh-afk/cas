//! Preallocated per-member GET connections; routing allocates no request memory.
use super::{
    Client, Config, Ticket,
    metrics::Metrics,
    race::{Armed, LocalSource, PendingKind, PendingRace, Racer},
};
use allocator_api2::vec::Vec;
use cas_core::{
    budget::{Budget, BudgetAllocator, BudgetArc},
    store::file::Read as Plan,
};
use std::{io, sync::Arc, time::Duration};

/// Member connections, race sources and the racer, shared by every view.
pub(crate) struct Shared {
    placement: Option<crate::fleet::Placement>,
    pub(crate) clients: Vec<(u8, Client), BudgetAllocator>,
    pub(crate) local: Option<LocalSource>,
    racer: Option<Racer>,
}

/// Shared member connections seen from one image slot. The host owns the
/// unattributed router; each image gets a `for_image` view at creation, so a
/// GET verified by the transport is attributed to the issuing image exactly
/// once, whatever later readers coalesce on it and whichever source wins a
/// race. Views share the connections, the racer and the local race source.
pub struct Router {
    shared: BudgetArc<Shared>,
    /// The race budget in microseconds; `None` leaves every read unraced.
    budget_us: Option<u32>,
    image: usize,
}
impl Router {
    /// `local` supplies the host IO scheduler and a demand port index when the
    /// force-remote experiment races the local copy (`--peer-race-local`).
    pub fn new(
        config: &Config,
        budget: &Arc<Budget>,
        metrics: BudgetArc<Metrics>,
        local: Option<(BudgetArc<cas_core::scheduler::Scheduler>, usize)>,
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
            (Some(_), Some(scheduler)) if config.peer_race_local => {
                Some(LocalSource::new(scheduler, budget, metrics.clone())?)
            }
            _ => None,
        };
        let racer = match config.peer_race_us {
            Some(us) if us > 0 => Some(Racer::new(budget, metrics)?),
            _ => None,
        };
        let shared = BudgetArc::try_new(
            Shared {
                placement,
                clients,
                local,
                racer,
            },
            budget,
        )?;
        Ok(Self {
            shared,
            budget_us: config.peer_race_us,
            image: super::metrics::UNATTRIBUTED,
        })
    }
    /// The same connections and race sources, attributing verified GETs to
    /// `image`'s slot.
    pub fn for_image(&self, image: usize) -> Self {
        Self {
            shared: self.shared.clone(),
            budget_us: self.budget_us,
            image,
        }
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
        self.shared.clients[index]
            .1
            .fetch_with(hash, None, true, false, self.image)
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
            .fetch_with(hash, Some(notify), true, false, self.image)
    }
    /// True when a verified local plan should be captured for the race; the
    /// reader otherwise never consults the local store in force-remote mode.
    pub fn wants_local_plan(&self) -> bool {
        self.budget_us.is_some() && self.shared.local.is_some()
    }
    /// Arm the race for `ticket`: the next owning member if the fleet has
    /// one, else the local copy when a plan was captured. No second source
    /// leaves the read unraced. The challenger starts at the deadline, at
    /// once for a zero budget, or as soon as the primary fails.
    pub fn race(&self, ticket: &Ticket, hash: [u8; 32], local: Option<Plan>) {
        let Some(budget_us) = self.budget_us else {
            return;
        };
        let kind = match self.owners(&hash).nth(1) {
            Some(index) => PendingKind::Remote(self.shared.clients[index].1.handle()),
            None => match (local, &self.shared.local) {
                (Some(plan), Some(source)) => PendingKind::Local(plan, source.handle()),
                _ => return,
            },
        };
        let first = ticket.slot_ref();
        let pending = PendingRace { kind, budget_us };
        match &self.shared.racer {
            Some(racer) if budget_us > 0 => {
                first.arm(pending, false);
                racer.arm(Armed {
                    deadline: std::time::Instant::now()
                        + Duration::from_micros(u64::from(budget_us)),
                    first,
                });
            }
            _ => first.arm(pending, true),
        }
    }
    pub fn shutdown(&self) -> io::Result<()> {
        let mut result = Ok(());
        if let Some(racer) = &self.shared.racer
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
