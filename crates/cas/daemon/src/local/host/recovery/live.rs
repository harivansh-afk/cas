//! Prepare every image before shared output; finish every fence before serving.
use super::*;

pub struct Retained<M> {
    pub image: catalog::Id,
    pub epoch: u64,
    pub highest_issued: u64,
    pub mutations: M,
}

/// The frontend owns validated original descriptors and their guest memory.
pub trait Replay {
    fn gather(
        &mut self,
        image: catalog::Id,
        mutation: append::Mutation,
        bytes: &mut [u8],
    ) -> io::Result<()>;
    fn publish(&mut self, prefix: Prefix) -> io::Result<()>;
}

pub struct Prepared {
    inspected: Inspection<append::SharedLivePlan>,
}

impl Checked {
    pub fn prepare_live<M>(
        self,
        retained: impl IntoIterator<Item = Retained<M>>,
    ) -> io::Result<Prepared>
    where
        M: IntoIterator<Item = append::Mutation>,
    {
        if self.cold {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cold requirements cannot authorize retained replay",
            ));
        }
        let inspected = self.inspected;
        let mut images = reserved_vec(inspected.images.len(), &inspected.resources.metadata)?;
        let mut retained = retained.into_iter();
        for image in inspected.images {
            let saved = retained
                .next()
                .ok_or_else(|| io::Error::other("missing retained image"))?;
            if saved.image != image.required.image {
                return Err(io::Error::other(
                    "retained replay image differs from catalog",
                ));
            }
            let log = image
                .log
                .prepare_live(
                    image.required.published,
                    saved.epoch,
                    saved.highest_issued,
                    saved.mutations,
                )
                .map_err(io::Error::other)?;
            images.push(Image {
                manifest: image.manifest,
                log,
                required: image.required,
            });
        }
        if retained.next().is_some() {
            return Err(io::Error::other("unexpected retained replay image"));
        }
        Ok(Prepared {
            inspected: Inspection {
                segment_bytes: inspected.segment_bytes,
                resources: inspected.resources,
                tickets: inspected.tickets,
                catalog: inspected.catalog,
                store: inspected.store,
                images,
                snapshots: inspected.snapshots,
            },
        })
    }
}

impl Prepared {
    pub fn recover(
        self,
        limits: cas_core::space::Limits,
        replay: &mut impl Replay,
    ) -> io::Result<Recovered> {
        let _control = self
            .inspected
            .resources
            .pools
            .administrative()
            .ok_or_else(|| io::Error::other("recovery control credits exhausted"))?;
        let _append = self
            .inspected
            .resources
            .pools
            .replay(MAX_REQUEST_BYTES)
            .ok_or_else(|| io::Error::other("recovery append credits exhausted"))?;
        stabilize(
            self.inspected,
            limits,
            |log, repair| log.validate_recovery(repair).map_err(io::Error::other),
            |log, manifest, required, repair| {
                let mut log = log
                    .start(manifest.view()?, repair)
                    .map_err(io::Error::other)?;
                let published = |log: &append::LiveRecovery| Prefix {
                    image: required.image,
                    published: log.published(),
                };
                replay.publish(published(&log))?;
                while let Some(mutation) = log.next() {
                    log.replay_next(|bytes| replay.gather(required.image, mutation, bytes))
                        .map_err(io::Error::other)?;
                    replay.publish(published(&log))?;
                }
                log.finish().map_err(io::Error::other)
            },
        )
    }
}
