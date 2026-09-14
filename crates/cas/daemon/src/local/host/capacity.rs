//! Short admission checks and conservative bounds; IO remains on the owner.
use super::*;
use cas_core::{
    budget::BudgetArc,
    space::{Governor, Staging},
};

pub(super) use cas_core::space::METADATA_MARGIN;

pub struct Admission {
    pub staging: BudgetArc<Staging>,
    pub physical: Option<Arc<Governor>>,
    pub image: usize,
}

impl Admission {
    pub fn check(&self) -> Result<(), pressure::Reason> {
        if !self.staging.admits(self.image) {
            return Err(pressure::Reason::Staging);
        }
        if self.physical.as_ref().is_some_and(|physical| {
            let status = physical.status();
            status.failed || status.pressured
        }) {
            return Err(pressure::Reason::Physical);
        }
        Ok(())
    }

    pub fn pressure(&self) -> bool {
        let (host, image) = self
            .staging
            .status(self.image)
            .expect("registered staging image");
        host.compaction
            || image.compaction
            || self
                .physical
                .as_ref()
                .is_some_and(|physical| physical.status().pressured)
    }
}

impl SharedHost {
    pub fn collection_required(&self) -> bool {
        self.physical
            .as_ref()
            .is_some_and(|physical| physical.status().pressured)
    }

    pub fn account_failed(&self) -> bool {
        self.admission.status().failed
            || self.staging.failed()
            || self
                .physical
                .as_ref()
                .is_some_and(|physical| physical.status().failed)
    }
}

pub(super) fn validate(
    store: &Store,
    images: &[(Log, Manifest)],
    physical: &Governor,
) -> io::Result<()> {
    let segment = store.config().segment_bytes;
    validate_geometry(segment, store.tickets(), physical)?;
    if images
        .iter()
        .any(|(log, _)| log.config().segment_bytes != segment)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "WAL and store segment geometry differ",
        ));
    }
    for (_, manifest) in images {
        physical.validate_file(manifest.view()?.file())?;
    }
    Ok(())
}

pub(super) fn validate_geometry(
    segment: u64,
    tickets: &Arc<cas_core::segments::Tickets>,
    physical: &Governor,
) -> io::Result<()> {
    let reserve = cas_core::space::Limits::new(
        physical.limits().capacity,
        segment,
        cas_core::manifest::tree::MAX_TRANSACTION_BYTES as u64,
    )?
    .reserve;
    if segment < 2 * MAX_REQUEST_BYTES as u64
        || !physical.uses_tickets(tickets)
        || physical.limits().reserve < reserve
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "governed host allocation geometry or owner differs",
        ));
    }
    Ok(())
}

pub(super) fn staging_geometry(images: &[(Log, Manifest)], capacity: u64) -> io::Result<()> {
    let mut current = 0u128;
    for (log, _) in images {
        let segment = u128::from(log.config().segment_bytes);
        if segment * 100 >= u128::from(log.limits().staging_bytes) * 60 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "image current segment prevents staging resumption",
            ));
        }
        current += segment;
    }
    if current * 100 >= u128::from(capacity) * 60 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "host current segments prevent staging resumption",
        ));
    }
    Ok(())
}

pub(super) fn compaction_bytes(prepared: &append::PreparedCompaction<'_>, segment: u64) -> u64 {
    let chunks = prepared.chunk_count();
    let output = if chunks == 0 {
        0
    } else {
        segment
            + ((chunks + chunks.div_ceil(cas_core::store::format::MAX_CHUNKS)) * BLOCK_SIZE) as u64
    };
    output + prepared.manifest_bytes() as u64 + METADATA_MARGIN
}
