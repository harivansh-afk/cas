//! Own storage exclusion before GET/SET_INFLIGHT_FD chooses fresh or live recovery.
use crate::local::{self, Shared};
use cas_core::append::{self, Config, LiveRecovery, Log, Mutation, Recovery, Status};
use std::io;
use std::path::Path;
use std::sync::Arc;

enum Source {
    Created(Log),
    Existing(Recovery),
}

pub struct Opening {
    source: Option<Source>,
    pub config: Config,
    pub status: Status,
    pub shared: Arc<Shared>,
}

impl Opening {
    pub fn new(path: &Path, create_bytes: Option<u64>) -> io::Result<Self> {
        let (source, config, status) = match create_bytes {
            Some(bytes) => {
                let log = local::create_log(path, bytes)?;
                let values = (log.config(), log.status());
                (Source::Created(log), values.0, values.1)
            }
            None => {
                let inspected =
                    Log::inspect(path, append::Limits::default()).map_err(io::Error::other)?;
                let values = (inspected.config(), inspected.status());
                (Source::Existing(inspected), values.0, values.1)
            }
        };
        Ok(Self {
            source: Some(source),
            config,
            status,
            shared: Shared::new(status),
        })
    }

    pub fn fresh(&mut self) -> io::Result<Log> {
        match self
            .source
            .take()
            .ok_or_else(|| io::Error::other("storage opening already consumed"))?
        {
            Source::Created(log) => Ok(log),
            Source::Existing(inspected) => inspected.fresh(0).map_err(io::Error::other),
        }
    }

    pub fn live(
        &mut self,
        required: u64,
        epoch: u64,
        issued: u64,
        mutations: Vec<Mutation>,
    ) -> io::Result<LiveRecovery> {
        if !matches!(self.source, Some(Source::Existing(_))) {
            return Err(io::Error::other(
                "retained attachment requires an existing image",
            ));
        }
        let Some(Source::Existing(inspected)) = self.source.take() else {
            unreachable!()
        };
        inspected
            .live(required, epoch, issued, mutations)
            .map_err(io::Error::other)
    }
}
