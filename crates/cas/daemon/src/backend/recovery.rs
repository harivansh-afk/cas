//! Retained attachment negotiation and replay before normal queue admission.
use super::*;
use cas_core::append::{
    Mutation,
    format::{Kind, RequestId},
};
use cas_daemon::inflight::{Carrier, Geometry, Identity};
use std::fs::File;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use vhost::vhost_user::message::VhostUserInflight;

#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Phase {
    #[default]
    AwaitingFd,
    Fresh,
    Replay,
    Active,
}

#[derive(Default)]
pub(super) struct Session {
    phase: Phase,
    replayed_requests: usize,
    replayed_mutations: usize,
    replay_copy_bytes: u64,
    replayed_write_bytes: u64,
    saved_p: u64,
    recovered_p: u64,
}

impl Session {
    pub(super) fn fresh(&self) -> bool {
        self.phase == Phase::Fresh
    }
    pub(super) fn active(&self) -> bool {
        self.phase == Phase::Active
    }
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({ "active": self.phase == Phase::Active,
            "replayed_requests": self.replayed_requests, "replayed_mutations": self.replayed_mutations,
            "replay_copy_bytes": self.replay_copy_bytes, "replayed_write_bytes": self.replayed_write_bytes, "saved_p": self.saved_p,
            "recovered_p": self.recovered_p })
    }
}

fn geometry(message: &VhostUserInflight) -> io::Result<Geometry> {
    if usize::from(message.num_queues) > CONCURRENT_QUEUES
        || usize::from(message.queue_size) > CONCURRENT_QUEUE_SIZE
    {
        return Err(io::Error::other(
            "inflight geometry differs from configured queues",
        ));
    }
    Geometry::new(message.num_queues, message.queue_size)
}

impl Backend {
    pub(super) fn create_attachment(
        &mut self,
        message: &VhostUserInflight,
    ) -> io::Result<(VhostUserInflight, File)> {
        let geometry = geometry(message)?;
        let phase = self.live.as_ref().map(|session| session.phase);
        let (shared, mut identity, status) = match (&mut self.storage, phase) {
            (Storage::Opening(opening), Some(Phase::AwaitingFd)) => {
                let shared = Arc::clone(&opening.shared);
                let log = opening.fresh()?;
                let config = log.config();
                let status = log.status();
                self.storage = Storage::Local(Box::new(local::Local::from_log(
                    log,
                    &self.completion_event,
                    local::Execution::Concurrent,
                    Arc::clone(&shared),
                )?));
                (
                    shared,
                    Identity {
                        store: config.store,
                        image: config.image,
                        epoch: status.epoch,
                        attachment: status.epoch,
                    },
                    status,
                )
            }
            (Storage::Local(local), Some(Phase::Active)) => {
                let (identity, required_p) = {
                    let health = local
                        .shared
                        .health
                        .lock()
                        .map_err(|_| io::Error::other("completion gate poisoned"))?;
                    let carrier = health
                        .carrier
                        .as_ref()
                        .ok_or_else(|| io::Error::other("missing old carrier"))?;
                    (carrier.identity(), carrier.published())
                };
                let status = local
                    .new_attachment(self.change_deadline.ok_or_else(|| {
                        io::Error::other("attachment has no lifecycle deadline")
                    })?)?;
                if status.published < required_p || status.durable < required_p {
                    return Err(io::Error::other(
                        "fresh attachment lost the old required prefix",
                    ));
                }
                (Arc::clone(&local.shared), identity, status)
            }
            _ => return Err(io::Error::other("unexpected fresh inflight request")),
        };
        // A fresh carrier is exported only after its new epoch and fence are
        // durable. Keep the old map through that transition so P stays checked.
        identity.epoch = status.epoch;
        identity.attachment = status.epoch;
        let carrier = Carrier::create(geometry, identity, self.capacity_bytes, status.published)?;
        let exported = carrier.export()?;
        shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .carrier = Some(carrier);
        self.next_id = 0;
        self.blocked_queues.fill(true);
        self.rebase_queues.fill(false);
        self.live.as_mut().unwrap().phase = Phase::Fresh;
        Ok(exported)
    }

    pub(super) fn restore_attachment(
        &mut self,
        message: &VhostUserInflight,
        file: File,
    ) -> io::Result<()> {
        geometry(message)?;
        let phase = self
            .live
            .as_ref()
            .ok_or_else(|| io::Error::other("inflight is not enabled"))?
            .phase;
        let gate = self
            .storage
            .completion_gate()
            .ok_or_else(|| io::Error::other("missing image state"))?;
        let mut state = gate
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?;
        if matches!(phase, Phase::Fresh | Phase::Active) {
            let current = state
                .carrier
                .as_ref()
                .ok_or_else(|| io::Error::other("missing fresh carrier"))?;
            let (_, original) = current.export()?;
            let expected = original.metadata()?;
            let actual = file.metadata()?;
            if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
                return Err(io::Error::other("SET must return the current GET carrier"));
            }
            // QEMU sends SET after GET before queue setup, including on fresh boot.
            Carrier::attach(file, message, current.identity(), self.capacity_bytes)?;
            return Ok(());
        }
        if phase != Phase::AwaitingFd {
            return Err(io::Error::other("inflight replacement while active"));
        }
        let Storage::Opening(opening) = &self.storage else {
            return Err(io::Error::other("missing locked inspection"));
        };
        let identity = Identity {
            store: opening.config.store,
            image: opening.config.image,
            epoch: opening.status.epoch,
            attachment: opening.status.epoch,
        };
        state.carrier = Some(Carrier::attach(
            file,
            message,
            identity,
            self.capacity_bytes,
        )?);
        self.live.as_mut().unwrap().phase = Phase::Replay;
        Ok(())
    }

    pub(super) fn activate_attachment(
        &mut self,
        mem: &GuestMemoryLoadGuard<GuestMemoryMmap>,
        vrings: &[VringMutex],
    ) -> io::Result<bool> {
        let Some(session) = &self.live else {
            return Ok(true);
        };
        let phase = session.phase;
        if phase == Phase::AwaitingFd {
            return Err(io::Error::other("IO before inflight negotiation"));
        }
        let gate = self
            .storage
            .completion_gate()
            .ok_or_else(|| io::Error::other("missing image state"))?;
        let mut state = gate
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?;
        let carrier = state
            .carrier
            .as_mut()
            .ok_or_else(|| io::Error::other("missing retained carrier"))?;
        let geometry = carrier.geometry().message();
        let mut queues: Vec<_> = vrings.iter().map(VringMutex::get_mut).collect();
        let mut cursors = Vec::with_capacity(usize::from(geometry.num_queues));
        for (index, queue) in queues.iter_mut().enumerate() {
            let ready =
                !self.blocked_queues[index] && queue.is_enabled() && queue.get_queue().ready();
            if index >= usize::from(geometry.num_queues) {
                if ready {
                    return Err(io::Error::other(
                        "enabled queue lies outside retained geometry",
                    ));
                }
                continue;
            }
            let index = index as u16;
            let initialized = carrier.queue_initialized(index)?;
            if !ready {
                if phase == Phase::Replay && initialized {
                    // Another queue's SET messages must finish before replay.
                    return Ok(false);
                }
                cursors.push(None);
                continue;
            }
            if queue.get_queue().size() != geometry.queue_size {
                return Err(io::Error::other("queue size differs from carrier"));
            }
            let used = queue
                .get_queue()
                .used_idx(&**mem, Ordering::Acquire)
                .map_err(io::Error::other)?
                .0;
            if phase == Phase::Replay && !initialized && used != 0 {
                return Err(io::Error::other("unused carrier has guest completions"));
            }
            if phase != Phase::Replay && !initialized {
                carrier.initialize_queue(index, queue.get_queue().next_avail(), used)?;
                queue.get_queue_mut().set_next_used(used);
            }
            cursors.push(Some(used));
        }
        if phase != Phase::Replay {
            self.live.as_mut().unwrap().phase = Phase::Active;
            self.recovered()?;
            return Ok(true);
        }
        let initialized: Vec<_> = (0..geometry.num_queues)
            .map(|index| carrier.queue_initialized(index))
            .collect::<io::Result<_>>()?;
        let saved_used: Vec<_> = cursors
            .iter()
            .zip(&initialized)
            .map(|(used, initialized)| if *initialized { *used } else { None })
            .collect();
        let replay = carrier.reconcile(&saved_used)?;
        // Decode all original heads before allowing any WAL repair. Never read
        // heads from old available slots: they can already have wrapped.
        let mut requests = Vec::with_capacity(replay.entries.len());
        for entry in &replay.entries {
            let queue = &queues[usize::from(entry.request.queue)];
            let chain = virtio_queue::DescriptorChain::new(
                mem.clone(),
                vm_memory::GuestAddress(queue.get_queue().desc_table()),
                queue.get_queue().size(),
                entry.request.head,
            );
            let request = decode_chain(mem, chain, self.capacity_bytes)?;
            if request.inflight(entry.request.queue, entry.request.available) != entry.request {
                return Err(io::Error::other(
                    "retained guest descriptor identity or range differs",
                ));
            }
            requests.push(request);
        }
        let epoch = carrier.identity().epoch;
        drop(state); // No completion lock may span blocking filesystem IO.
        let Storage::Opening(opening) = &mut self.storage else {
            return Err(io::Error::other("missing locked recovery"));
        };
        let shared = Arc::clone(&opening.shared);
        let worker_shared = Arc::clone(&shared);
        let deadline = opening.deadline;
        let inspected = opening.take_inspection()?;
        let memory = mem.clone().into_inner();
        let Recovered {
            log,
            replay,
            requests,
            copied,
            mutations,
        } = deadline.run(move || {
            replay_storage(
                deadline,
                inspected,
                worker_shared,
                memory,
                epoch,
                replay,
                requests,
            )
        })?;
        let mut state = gate
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?;
        let carrier = state
            .carrier
            .as_mut()
            .ok_or_else(|| io::Error::other("missing retained carrier"))?;
        for (index, (queue, used)) in queues.iter_mut().zip(&cursors).enumerate() {
            let Some(used) = *used else { continue };
            if !initialized[index] {
                carrier.initialize_queue(index as u16, queue.get_queue().next_avail(), used)?;
            }
            queue
                .get_queue_mut()
                .set_next_avail(carrier.available(index as u16)?);
            queue.get_queue_mut().set_next_used(used);
        }
        state.durable = log.status().durable;
        self.next_id = replay.highest_serial;
        self.restored_used = Some(cursors.iter().flatten().copied().next().unwrap_or(0));
        self.restored_pending = replay.entries.len() as u16;
        let session = self.live.as_mut().unwrap();
        session.saved_p = replay.published;
        session.recovered_p = log.status().published;
        session.replayed_requests = replay.entries.len();
        session.replayed_mutations = mutations;
        session.replay_copy_bytes = copied;
        session.replayed_write_bytes = requests
            .iter()
            .map(|request| {
                if let Request::Write(data) = request {
                    data.len as u64
                } else {
                    0
                }
            })
            .sum();
        self.storage = Storage::Local(Box::new(local::Local::from_log(
            log,
            &self.completion_event,
            local::Execution::Concurrent,
            Arc::clone(&shared),
        )?));
        // All mutations and the recovery fence are durable. Recovered reads
        // may see this newer prefix, and recovered FLUSHes cover their boundary.
        for (entry, request) in replay.entries.into_iter().zip(requests) {
            let queue = &mut *queues[usize::from(entry.request.queue)];
            let permit = shared
                .reserve(request.admission_kind())
                .ok_or_else(|| io::Error::other("replay completion reserve exhausted"))?;
            let completion = GuestCompletion {
                queue: entry.request.queue,
                target: request.completion(),
                inflight: Some(entry),
                write_number: None,
            };
            match request {
                Request::Read(data) => {
                    self.enqueue(
                        mem,
                        queue,
                        Admitted {
                            queue: entry.request.queue,
                            id: entry.serial - 1,
                            request: Request::Read(data),
                            permit: Permit::Local { _credits: permit },
                            inflight: Some(entry),
                        },
                        Some(&mut state),
                    )?;
                }
                Request::Write(data) => {
                    self.finish(mem, queue, completion, Status::Ok, None, Some(&mut state))?;
                    self.counters.writes += 1;
                    self.counters.write_bytes += data.len as u64;
                }
                Request::Flush(_) => {
                    self.finish(mem, queue, completion, Status::Ok, None, Some(&mut state))?;
                    self.counters.flushes += 1;
                }
                Request::GetId { segments, .. } => self.finish(
                    mem,
                    queue,
                    completion,
                    Status::Ok,
                    Some((&segments, DEVICE_ID)),
                    Some(&mut state),
                )?,
                Request::Unsupported(_) => self.finish(
                    mem,
                    queue,
                    completion,
                    Status::Unsupported,
                    None,
                    Some(&mut state),
                )?,
                Request::Invalid(_) => self.finish(
                    mem,
                    queue,
                    completion,
                    Status::IoError,
                    None,
                    Some(&mut state),
                )?,
            }
        }
        drop(state);
        self.live.as_mut().unwrap().phase = Phase::Active;
        self.recovered()?;
        Ok(true)
    }
}

struct Recovered {
    log: cas_core::append::Log,
    replay: cas_daemon::inflight::Replay,
    requests: Vec<Request>,
    copied: u64,
    mutations: usize,
}

fn replay_storage(
    deadline: crate::deadline::Deadline,
    inspected: cas_core::append::Recovery,
    shared: Arc<local::Shared>,
    memory: Arc<GuestMemoryMmap>,
    epoch: u64,
    replay: cas_daemon::inflight::Replay,
    requests: Vec<Request>,
) -> io::Result<Recovered> {
    let mutations = replay
        .entries
        .iter()
        .filter(|entry| entry.mutation != 0)
        .map(|entry| Mutation {
            id: RequestId {
                serial: entry.serial,
                attachment: entry.attachment,
                queue: entry.request.queue,
                head: entry.request.head,
            },
            sequence: entry.mutation,
            offset: entry.request.offset,
            length: entry.request.length,
            kind: if entry.request.kind == cas_daemon::inflight::Kind::Write {
                Kind::Write
            } else {
                Kind::Zero
            },
        })
        .collect();
    deadline.check()?;
    let mut storage_replay = inspected
        .live(replay.published, epoch, replay.highest_mutation, mutations)
        .map_err(io::Error::other)?;
    let publish = |prefix| -> io::Result<()> {
        deadline.check()?;
        shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .publish(prefix)
    };
    publish(storage_replay.published())?;
    let mut copied = 0;
    let mut mutations = 0;
    while let Some(mutation) = storage_replay.next() {
        deadline.check()?;
        let index = replay
            .entries
            .binary_search_by_key(&mutation.id.serial, |entry| entry.serial)
            .map_err(|_| io::Error::other("replay mutation lost its descriptor"))?;
        let Request::Write(data) = &requests[index] else {
            return Err(io::Error::other("unexpected replay mutation kind"));
        };
        shared.replay_next(&mut storage_replay, |destination| {
            let health = shared
                .health
                .lock()
                .map_err(|_| io::Error::other("completion gate poisoned"))?;
            deadline.check()?;
            if let Some(error) = &health.failure {
                return Err(io::Error::other(error.clone()));
            }
            gather(&memory, &data.segments, destination, &mut copied)
        })?;
        publish(storage_replay.published())?;
        mutations += 1;
    }
    deadline.check()?;
    let log = shared.finish_replay(storage_replay)?;
    deadline.check()?;
    Ok(Recovered {
        log,
        replay,
        requests,
        copied,
        mutations,
    })
}

#[cfg(test)]
mod tests;
