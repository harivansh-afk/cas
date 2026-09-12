//! Collect independently negotiated images, then publish one recovered host.
use super::*;
use crate::backend::recovery::{Activated, Validated};
use crate::deadline::{Deadline, RECOVERY_TIMEOUT};
use crate::storage::Opening;
use std::sync::mpsc::TryRecvError;

pub(crate) struct Gates {
    pub(in crate::local::host) host: Arc<state::HostGate>,
    pub(in crate::local::host) images: BudgetVec<(catalog::Id, Health), BudgetAllocator>,
}

impl Gates {
    pub(in crate::local::host) fn validate(&self, images: &[(Log, Manifest)]) -> io::Result<()> {
        if self.images.len() != images.len()
            || self
                .images
                .iter()
                .zip(images)
                .any(|((id, health), (log, _))| {
                    *id != log.config().image
                        || health.lock().map_or(true, |state| {
                            state.failure.is_some() || state.durable != log.status().durable
                        })
                })
        {
            return Err(io::Error::other(
                "recovered completion gates differ from images",
            ));
        }
        if let Some(error) = self.host.failure() {
            return Err(io::Error::other(error));
        }
        Ok(())
    }
}

struct Input {
    index: usize,
    validated: Validated,
}

pub(crate) struct Endpoint {
    index: usize,
    input: Option<mailbox::Sender<Input>>,
    output: mailbox::Receiver<Activated>,
    host: Arc<state::HostGate>,
    resolved: bool,
}

impl Endpoint {
    pub(crate) fn submit(&mut self, validated: Validated) -> io::Result<()> {
        if let Some(error) = self.host.failure() {
            return Err(io::Error::other(error));
        }
        self.input
            .take()
            .ok_or_else(|| io::Error::other("replay already submitted"))?
            .try_send(Input {
                index: self.index,
                validated,
            })
            .map_err(|_| io::Error::other("shared replay input unavailable"))
    }

    pub(crate) fn poll(&mut self) -> io::Result<Option<Activated>> {
        if let Some(error) = self.host.failure() {
            return Err(io::Error::other(error));
        }
        match self.output.try_recv() {
            Ok(value) => {
                self.resolved = true;
                Ok(Some(value))
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => Err(io::Error::other("shared replay owner exited")),
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if !self.resolved {
            self.host
                .fail("retained frontend abandoned before activation".into());
        }
    }
}

struct Attachment {
    image: catalog::Id,
    opening: Opening,
    event: EventFd,
}

struct Output {
    done: mailbox::Sender<Activated>,
    event: EventFd,
}

/// Owns root recovery until every required catalog image has negotiated.
pub struct RetainedHost {
    attachments: BudgetVec<Option<Attachment>, BudgetAllocator>,
    worker: Option<JoinHandle<io::Result<Host>>>,
    host: Option<Host>,
    gate: Arc<state::HostGate>,
}

impl RetainedHost {
    pub(crate) fn fail_all(&self, reason: &str) {
        self.gate.fail(reason.to_owned());
    }
    pub fn open(
        root: &Path,
        config: Config,
        resources: Arc<Resources>,
        limits: cas_core::space::Limits,
        staging_bytes: u64,
    ) -> io::Result<Self> {
        let deadline = Deadline::after(RECOVERY_TIMEOUT);
        let root = root.to_owned();
        let inspected = deadline.run(move || Inspection::scan(&root, config, resources))?;
        Self::start(inspected, limits, staging_bytes, deadline)
    }

    pub(crate) fn start(
        inspected: Inspection,
        limits: cas_core::space::Limits,
        staging_bytes: u64,
        deadline: Deadline,
    ) -> io::Result<Self> {
        deadline.check()?;
        let resources = &inspected.resources;
        let count = inspected.images.len();
        if count == 0 {
            return Err(io::Error::other("retained host requires catalog images"));
        }
        let mut attachments = reserved_vec(count, &resources.metadata)?;
        let mut outputs = reserved_vec(count, &resources.metadata)?;
        let mut images = reserved_vec(count, &resources.metadata)?;
        let (send, input) = mailbox::bounded(count, &resources.metadata)?;
        let gate = Arc::new(state::HostGate::default());
        for (index, image) in inspected.images.iter().enumerate() {
            let status = image.log.status();
            let health = state::Gate::new(
                ImageState {
                    durable: status.durable,
                    ..ImageState::default()
                },
                Some(Arc::clone(&gate)),
            );
            images.push((image.required.image, Arc::clone(&health)));
            let shared = Shared::with_pools(
                status,
                resources.pools.image(),
                health,
                Arc::clone(&resources.metadata),
                None,
                None,
            );
            let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK)?;
            let (done, output) = mailbox::bounded(1, &resources.metadata)?;
            outputs.push(Output {
                done,
                event: event.try_clone()?,
            });
            let endpoint = Endpoint {
                index,
                input: Some(send.clone()),
                output,
                host: Arc::clone(&gate),
                resolved: false,
            };
            attachments.push(Some(Attachment {
                image: image.required.image,
                opening: Opening::shared(image.log.config(), status, shared, deadline, endpoint),
                event,
            }));
        }
        drop(send);
        let gates = Gates {
            host: Arc::clone(&gate),
            images,
        };
        let worker = thread::Builder::new()
            .name("cas-host-recovery".into())
            .spawn(move || {
                let failure = Failure {
                    gate: Arc::clone(&gates.host),
                    outputs,
                };
                let result = recover(
                    inspected,
                    limits,
                    staging_bytes,
                    deadline,
                    gates,
                    input,
                    &failure.outputs,
                );
                if let Err(error) = &result {
                    failure.gate.fail(error.to_string());
                }
                for output in &failure.outputs {
                    let _ = notify(&output.event);
                }
                // A successful worker transfers the host and all completion owners.
                failure.finish(result)
            })?;
        Ok(Self {
            attachments,
            worker: Some(worker),
            host: None,
            gate,
        })
    }

    pub fn attach(
        &mut self,
        image: catalog::Id,
        fault: crate::fault::Fault,
    ) -> io::Result<crate::backend::Backend> {
        let slot = self
            .attachments
            .iter_mut()
            .find(|slot| slot.as_ref().is_some_and(|a| a.image == image))
            .ok_or_else(|| io::Error::other("unknown or already attached retained image"))?;
        let attachment = slot.take().unwrap();
        crate::backend::Backend::from_storage(
            crate::storage::Storage::Opening(Box::new(attachment.opening)),
            attachment.event,
            crate::BackendKind::LocalAsync,
            true,
            true,
            fault,
        )
    }

    /// Poll without dropping a recovery worker's storage or guest-memory owners.
    pub fn host(&mut self) -> io::Result<Option<&mut Host>> {
        if self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.is_finished())
        {
            self.host = Some(
                self.worker
                    .take()
                    .unwrap()
                    .join()
                    .map_err(|_| io::Error::other("shared recovery worker panicked"))??,
            );
        }
        if let Some(error) = self.gate.failure() {
            return Err(io::Error::other(error));
        }
        Ok(self.host.as_mut())
    }
}

// Also closes the host on panic; never release guest owners to fake completion.
struct Failure {
    gate: Arc<state::HostGate>,
    outputs: BudgetVec<Output, BudgetAllocator>,
}
impl Failure {
    fn finish(mut self, result: io::Result<Host>) -> io::Result<Host> {
        self.outputs.clear();
        result
    }
}
impl Drop for Failure {
    fn drop(&mut self) {
        if !self.outputs.is_empty() {
            self.gate
                .fail("shared recovery worker exited before publication".into());
            for output in &self.outputs {
                let _ = notify(&output.event);
            }
        }
    }
}

fn recover(
    inspected: Inspection,
    limits: cas_core::space::Limits,
    staging_bytes: u64,
    deadline: Deadline,
    gates: Gates,
    input: mailbox::Receiver<Input>,
    outputs: &[Output],
) -> io::Result<Host> {
    let resources = Arc::clone(&inspected.resources);
    let count = inspected.images.len();
    let mut pending = reserved_vec(count, &resources.metadata)?;
    pending.resize_with(count, || None);
    let mut received = 0;
    while received < count {
        let remaining = deadline.remaining()?;
        if let Some(error) = gates.host.failure() {
            return Err(io::Error::other(error));
        }
        let value = match input.recv_timeout(remaining.min(Duration::from_millis(10))) {
            Ok(value) => value,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(error) => {
                return Err(io::Error::other(format!(
                    "incomplete retained frontends: {error}"
                )));
            }
        };
        if value.index >= count || pending[value.index].is_some() {
            return Err(io::Error::other("duplicate or unknown retained frontend"));
        }
        pending[value.index] = Some(value.validated);
        received += 1;
    }
    deadline.check()?;
    if let Some(error) = gates.host.failure() {
        return Err(io::Error::other(error));
    }
    let mut inputs = reserved_vec(count, &resources.metadata)?;
    inputs.extend(pending.into_iter().map(Option::unwrap));
    let mut prefixes = reserved_vec(count, &resources.metadata)?;
    prefixes.extend(
        gates
            .images
            .iter()
            .zip(&inputs)
            .map(|((image, _), saved)| Prefix {
                image: *image,
                published: saved.replay.published,
            }),
    );
    let prepared = inspected
        .require(Prefixes::Retained(&prefixes))?
        .prepare_live(
            gates
                .images
                .iter()
                .zip(&inputs)
                .map(|((image, _), saved)| Retained {
                    image: *image,
                    epoch: saved.epoch,
                    highest_issued: saved.replay.highest_mutation,
                    mutations: saved.mutations(),
                }),
        )?;
    let mut replay = FrontendReplay {
        gates: &gates,
        inputs: &mut inputs,
        deadline,
    };
    let recovered = prepared.recover(limits, &mut replay)?;
    deadline.check()?;
    for ((_, health), (log, _)) in gates.images.iter().zip(&recovered.images) {
        let mut state = health.lock()?;
        state.publish(log.status().published)?;
        state.durable = log.status().durable;
    }
    let mut host = recovered.into_host_with_gates(staging_bytes, Some(gates))?;
    let mut activated = reserved_vec(count, &resources.metadata)?;
    for ((saved, output), prefix) in inputs.into_iter().zip(outputs).zip(prefixes) {
        let local = host.local(prefix.image, &output.event)?;
        activated.push(Activated { local, saved });
    }
    deadline.check()?;
    for (output, activated) in outputs.iter().zip(activated) {
        output
            .done
            .try_send(activated)
            .map_err(|_| io::Error::other("retained frontend left before activation"))?;
    }
    Ok(host)
}

struct FrontendReplay<'a> {
    gates: &'a Gates,
    inputs: &'a mut [Validated],
    deadline: Deadline,
}
impl Replay for FrontendReplay<'_> {
    fn gather(
        &mut self,
        image: catalog::Id,
        mutation: append::Mutation,
        bytes: &mut [u8],
    ) -> io::Result<()> {
        self.deadline.check()?;
        let index = self
            .gates
            .images
            .binary_search_by_key(&image, |entry| entry.0)
            .map_err(|_| io::Error::other("replay image lost its completion owner"))?;
        let health = self.gates.images[index].1.lock()?;
        if let Some(error) = &health.failure {
            return Err(io::Error::other(error.clone()));
        }
        self.inputs[index].gather(mutation, bytes)
    }
    fn publish(&mut self, prefix: Prefix) -> io::Result<()> {
        self.deadline.check()?;
        let index = self
            .gates
            .images
            .binary_search_by_key(&prefix.image, |entry| entry.0)
            .map_err(|_| io::Error::other("replay image lost its completion owner"))?;
        self.gates.images[index]
            .1
            .lock()?
            .publish(prefix.published)?;
        self.inputs[index].record_prefix(prefix.published)
    }
}
