//! Shared namespace and the stable manifest boundary for WAL repair.
use super::{Config, Limits, LivePlan, LiveRecovery, Log, Mutation, Recovery, Result};
use crate::{
    budget::Budget,
    encoding::require,
    manifest::file::{Inspection, View},
    manifest::format::Commit,
    segments::Tickets,
};
use std::{fs::File, io, path::PathBuf, sync::Arc};

fn staging(tickets: &Tickets, image: [u8; 16]) -> PathBuf {
    let image: String = image.iter().map(|byte| format!("{byte:02x}")).collect();
    tickets.root().join("images").join(image).join("staging")
}

fn identity(config: Config, commit: Commit) -> io::Result<()> {
    require(
        config.store == commit.store
            && config.image == commit.image
            && config.image_bytes == commit.image_bytes,
        "staging and manifest identities differ",
    )
}

impl Log {
    /// The lifecycle owner has created/synced the image directory and manifest.
    /// A new sequence namespace starts at D=0, including a nonempty clone root.
    pub fn create_shared(
        tickets: Arc<Tickets>,
        config: Config,
        limits: Limits,
        metadata: Arc<Budget>,
        base: View,
    ) -> Result<Self> {
        identity(config, base.commit())?;
        require(
            base.commit().durable == 0,
            "new staging namespace requires D=0",
        )?;
        Self::create_in(
            &staging(&tickets, config.image),
            config,
            limits,
            metadata,
            Some(tickets),
            Some(base),
        )
    }

    /// Read-only WAL inspection against a selected, possibly unsynced manifest.
    /// A matching recovered View is required before this can repair anything.
    pub fn inspect_shared(
        tickets: Arc<Tickets>,
        manifest: &Inspection,
        limits: Limits,
        metadata: Arc<Budget>,
    ) -> Result<SharedRecovery> {
        let selected = manifest.selected();
        let recovery = Self::inspect_in(
            &staging(&tickets, selected.commit.image),
            limits,
            metadata,
            selected.commit.durable,
            Some(tickets),
        )?;
        identity(recovery.config(), selected.commit)?;
        Ok(SharedRecovery {
            recovery,
            base: Candidate {
                commit: selected.commit,
                end: selected.end,
                file: manifest.pin(),
            },
        })
    }
}

/// The candidate pin prevents inode/lock reuse while its dependencies stabilize.
/// No ordinary Recovery handle is exposed before the exact base is durable.
pub struct SharedRecovery {
    recovery: Recovery,
    base: Candidate,
}

struct Candidate {
    commit: Commit,
    end: u64,
    file: Arc<File>,
}

impl Candidate {
    fn stabilize(self, log: &mut Log, base: View) -> Result<()> {
        require(
            base.commit() == self.commit && base.end() == self.end && base.owns(&self.file),
            "WAL repair requires its exact stabilized manifest candidate",
        )?;
        log.base = Some(base);
        Ok(())
    }
}

impl SharedRecovery {
    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> Result<()> {
        self.recovery.validate_recovery(repair)
    }

    pub fn config(&self) -> Config {
        self.recovery.config()
    }

    pub fn status(&self) -> super::Status {
        self.recovery.status()
    }

    /// Check saved P before stabilizing any inspected dependency or repairing WAL.
    pub fn require_prefix(&self, required: u64) -> Result<()> {
        self.recovery.require_prefix(required)
    }

    fn stabilized(mut self, base: View) -> Result<Recovery> {
        self.base.stabilize(&mut self.recovery.log, base)?;
        Ok(self.recovery)
    }

    pub fn fresh(self, base: View, required: u64) -> Result<Log> {
        self.fresh_with(base, required, crate::space::Recovery::default())
    }

    pub fn fresh_with(
        self,
        base: View,
        required: u64,
        repair: crate::space::Recovery<'_>,
    ) -> Result<Log> {
        self.stabilized(base)?.fresh_with(required, repair)
    }

    pub fn live(
        self,
        base: View,
        required: u64,
        epoch: u64,
        highest_issued: u64,
        mutations: Vec<Mutation>,
    ) -> Result<LiveRecovery> {
        self.prepare_live(required, epoch, highest_issued, mutations)?
            .start(base, crate::space::Recovery::default())
    }

    pub fn prepare_live(
        self,
        required: u64,
        epoch: u64,
        highest_issued: u64,
        mutations: impl IntoIterator<Item = Mutation>,
    ) -> Result<SharedLivePlan> {
        Ok(SharedLivePlan {
            plan: self
                .recovery
                .prepare_live(required, epoch, highest_issued, mutations)?,
            base: self.base,
        })
    }
}

pub struct SharedLivePlan {
    plan: LivePlan,
    base: Candidate,
}

impl SharedLivePlan {
    pub fn validate_recovery(&self, repair: crate::space::Recovery<'_>) -> Result<()> {
        self.plan.validate_recovery(repair)
    }

    pub fn start(mut self, base: View, repair: crate::space::Recovery<'_>) -> Result<LiveRecovery> {
        self.base.stabilize(&mut self.plan.recovery.log, base)?;
        self.plan.start(repair)
    }
}

#[cfg(test)]
mod tests;
