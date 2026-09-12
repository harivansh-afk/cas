//! Explicit one-shot development pauses at shared writer operations.
use super::*;
use std::{num::NonZeroU64, path::PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Point {
    BeforeChunks,
    AfterChunks,
    AfterManifest,
    AfterD,
    AfterPunch,
    AfterUnlink,
}

pub struct Selection {
    pub point: Point,
    pub image: [u8; 16],
    pub after: NonZeroU64,
    pub wait_for_arm: bool,
}

pub(super) struct Pause {
    selection: Selection,
    marker: PathBuf,
    arm: Option<PathBuf>,
}

pub(super) struct Observation {
    pub image: [u8; 16],
    pub through: u64,
    pub manifest_durable: u64,
    pub chunks: Option<usize>,
    pub operation: Option<append::ReclaimOperation>,
}

impl Resources {
    pub(crate) fn pause_compaction(
        &mut self,
        selection: Selection,
        marker: PathBuf,
    ) -> io::Result<()> {
        match marker.symlink_metadata() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => (),
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("compaction marker already exists")),
        }
        if self.fault.is_some() {
            return Err(io::Error::other("compaction pause already configured"));
        }
        let arm = selection
            .wait_for_arm
            .then(|| marker.with_file_name("compaction-arm"));
        self.fault = Some(Mutex::new(Some(Pause {
            selection,
            marker,
            arm,
        })));
        Ok(())
    }

    pub(super) fn hit_compaction(
        &self,
        point: Point,
        observation: Observation,
        health: &Health,
    ) -> io::Result<()> {
        let Observation {
            image,
            through,
            manifest_durable,
            chunks,
            operation,
        } = observation;
        if matches!(point, Point::BeforeChunks | Point::AfterChunks) && chunks == Some(0) {
            return Ok(());
        }
        let Some(fault) = &self.fault else {
            return Ok(());
        };
        let pause = fault
            .lock()
            .map_err(|_| io::Error::other("fault state poisoned"))?
            .take_if(|pause| {
                pause.selection.point == point
                    && pause.selection.image == image
                    && through >= pause.selection.after.get()
                    && pause.arm.as_ref().is_none_or(|path| path.is_file())
            });
        let Some(pause) = pause else { return Ok(()) };
        let state = health.lock()?;
        let snapshot = state.snapshot();
        let value = serde_json::json!({
            "schema_version":1, "point":point, "image":format!("{:032x}",u128::from_be_bytes(image)),
            "selected_through":through,"manifest_durable":manifest_durable,
            "published":snapshot.map(|s|s.published), "durable":state.durable,
            "reclamation":operation, "chunks":chunks, "armed":pause.selection.wait_for_arm,
        });
        drop(state);
        crate::fault::publish_marker(&pause.marker, &value)?;
        crate::fault::stop_process()
    }
}
