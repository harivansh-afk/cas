//! Complete mapping preparation precedes every chunk publication.
use super::{Compacted, Input};
use crate::{
    BLOCK_SIZE, MAX_REQUEST_BYTES,
    chunk::Chunk,
    encoding::require,
    manifest::{
        file::{Manifest, View},
        format::Extent,
        tree::{MAX_CHANGES, Prepared as ManifestOutput},
    },
    store::{file::Store, format::MAX_CHUNKS},
};
use arrayvec::ArrayVec;
use std::{io, sync::Arc};

/// Completed owner boundaries, exposed for process-crash controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publication {
    BeforeChunks,
    AfterChunks,
    AfterManifest,
}

/// The worker retains Input while this borrows its verified immutable payload.
/// Every hash is computed once; the complete mapping is owned before output IO.
pub struct Prepared<'a> {
    previous: View,
    manifest: ManifestOutput,
    chunks: ArrayVec<Chunk<'a>, { MAX_REQUEST_BYTES / BLOCK_SIZE }>,
}

impl Input {
    pub fn prepare(&self, manifest: &Manifest) -> io::Result<Prepared<'_>> {
        require(
            manifest.view()?.same(&self.base),
            "compaction manifest changed before preparation",
        )?;
        let mut changes = ArrayVec::<Extent, MAX_CHANGES>::new();
        let mut chunks = ArrayVec::new();
        for (index, edit) in self.edits.iter().enumerate() {
            // Bounded by 318 edits. Keep partially covered ZERO ranges ordered;
            // splitting them would expand the encoder's reserved mapping bound.
            if self.edits[index + 1..]
                .iter()
                .any(|later| later.start <= edit.start && later.end >= edit.end)
            {
                continue;
            }
            let chunk = edit.payload.and_then(|offset| {
                Chunk::new(
                    self.payload.as_slice()[offset..offset + BLOCK_SIZE]
                        .try_into()
                        .expect("verified fixed-block edit"),
                )
            });
            changes.push(Extent {
                start: edit.start,
                end: edit.end,
                hash: chunk.map(Chunk::hash),
            });
            if let Some(chunk) = chunk {
                chunks.push(chunk);
            }
        }
        Ok(Prepared {
            previous: self.base.clone(),
            manifest: manifest.prepare_with_metadata(
                &changes,
                self.through,
                Arc::clone(&self.metadata),
            )?,
            chunks,
        })
    }

    /// Reference convenience path; the host uses prepare() to reserve disk
    /// output before invoking the same writer.
    pub fn write(self, store: &mut Store, manifest: &mut Manifest) -> io::Result<Compacted> {
        self.prepare(manifest)?.write(store, manifest)
    }
}

impl Prepared<'_> {
    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    pub fn manifest_bytes(&self) -> usize {
        self.manifest.bytes().len()
    }

    pub fn through(&self) -> u64 {
        self.manifest.commit().durable
    }

    /// The exclusive worker owns Store and Manifest throughout output and sync.
    pub fn write(self, store: &mut Store, manifest: &mut Manifest) -> io::Result<Compacted> {
        self.write_with(store, manifest, |_, _| Ok(()))
    }

    pub fn write_with(
        self,
        store: &mut Store,
        manifest: &mut Manifest,
        mut observe: impl FnMut(Publication, u64) -> io::Result<()>,
    ) -> io::Result<Compacted> {
        require(
            !store.status().failed && store.config().store == self.previous.commit().store,
            "compaction store is failed or differs",
        )?;
        require(
            manifest.view()?.same(&self.previous),
            "compaction manifest changed before output",
        )?;
        observe(Publication::BeforeChunks, manifest.current().durable)?;
        for chunks in self.chunks.chunks(MAX_CHUNKS) {
            store.insert(chunks)?;
        }
        observe(Publication::AfterChunks, manifest.current().durable)?;
        manifest.publish(self.manifest)?;
        observe(Publication::AfterManifest, manifest.current().durable)?;
        Ok(Compacted {
            previous: self.previous,
            view: manifest.view()?,
        })
    }
}
