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
    pub fn report(&self) -> serde_json::Value {
        serde_json::json!({ "active": self.phase == Phase::Active,
            "replayed_requests": self.replayed_requests, "replayed_mutations": self.replayed_mutations,
            "replay_copy_bytes": self.replay_copy_bytes, "replayed_write_bytes": self.replayed_write_bytes, "saved_p": self.saved_p,
            "recovered_p": self.recovered_p })
    }
}

fn geometry(message: &VhostUserInflight) -> io::Result<Geometry> {
    if message.num_queues != 1 || usize::from(message.queue_size) != QUEUE_SIZE {
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
        if self
            .live
            .as_ref()
            .is_none_or(|session| session.phase != Phase::AwaitingFd)
        {
            return Err(io::Error::other("unexpected fresh inflight request"));
        }
        let Storage::Opening(opening) = &mut self.storage else {
            return Err(io::Error::other("fresh attachment has no locked opening"));
        };
        let shared = Arc::clone(&opening.shared);
        let log = opening.fresh()?;
        let config = log.config();
        let status = log.status();
        // Fresh attachment generation and durable writer epoch use the same
        // unique ticket. The trailer still stores and checks both identities.
        let carrier = Carrier::create(
            geometry,
            Identity {
                store: config.store,
                image: config.image,
                epoch: status.epoch,
                attachment: status.epoch,
            },
            config.image_bytes,
            status.published,
        )?;
        let exported = carrier.export()?;
        shared
            .health
            .lock()
            .map_err(|_| io::Error::other("completion gate poisoned"))?
            .carrier = Some(carrier);
        self.storage = Storage::Local(Box::new(local::Local::from_log(
            log,
            &self.completion_event,
            local::Execution::Concurrent,
            shared,
        )?));
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
        if phase == Phase::Fresh {
            let current = state
                .carrier
                .as_ref()
                .ok_or_else(|| io::Error::other("missing fresh carrier"))?;
            let (_, original) = current.export()?;
            let expected = original.metadata()?;
            let actual = file.metadata()?;
            if (expected.dev(), expected.ino()) != (actual.dev(), actual.ino()) {
                return Err(io::Error::other("first SET must return the GET carrier"));
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
        queue: &mut VringState,
    ) -> io::Result<()> {
        let Some(session) = &self.live else {
            return Ok(());
        };
        let phase = session.phase;
        if phase == Phase::Active {
            return Ok(());
        }
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
        if usize::from(queue.get_queue().size()) != QUEUE_SIZE {
            return Err(io::Error::other("queue size differs from carrier"));
        }
        let used = queue
            .get_queue()
            .used_idx(&**mem, Ordering::Acquire)
            .map_err(io::Error::other)?
            .0;
        if phase == Phase::Fresh {
            carrier.initialize_queue(0, queue.get_queue().next_avail(), used)?;
            queue.get_queue_mut().set_next_used(used);
            self.live.as_mut().unwrap().phase = Phase::Active;
            return Ok(());
        }
        let initialized = carrier.queue_initialized(0)?;
        if !initialized && used != 0 {
            return Err(io::Error::other("unused carrier has guest completions"));
        }
        let replay = carrier.reconcile(&[initialized.then_some(used)])?;
        // Decode all original heads before allowing any WAL repair. Never read
        // heads from old available slots: they can already have wrapped.
        let mut requests = Vec::with_capacity(replay.entries.len());
        for entry in &replay.entries {
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
        let Storage::Opening(opening) = &mut self.storage else {
            return Err(io::Error::other("missing locked recovery"));
        };
        let shared = Arc::clone(&opening.shared);
        let mut storage_replay = opening.live(
            replay.published,
            carrier.identity().epoch,
            replay.highest_mutation,
            mutations,
        )?;
        carrier.publish(storage_replay.published())?;
        let mut copied = 0;
        let mut mutations = 0;
        while let Some(mutation) = storage_replay.next() {
            let index = replay
                .entries
                .binary_search_by_key(&mutation.id.serial, |entry| entry.serial)
                .map_err(|_| io::Error::other("replay mutation lost its descriptor"))?;
            let Request::Write(data) = &requests[index] else {
                return Err(io::Error::other("unexpected replay mutation kind"));
            };
            shared.replay_next(&mut storage_replay, |destination| {
                gather(mem, &data.segments, destination, &mut copied)
            })?;
            carrier.publish(storage_replay.published())?;
            mutations += 1;
        }
        let mut log = shared.finish_replay(storage_replay)?;
        if !initialized {
            carrier.initialize_queue(0, queue.get_queue().next_avail(), used)?;
        }
        queue.get_queue_mut().set_next_avail(carrier.available(0)?);
        queue.get_queue_mut().set_next_used(used);
        state.durable = log.status().durable;
        self.next_id = replay.highest_serial;
        self.restored_used = Some(used);
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
        // All mutations and the recovery fence are durable. Recovered reads
        // may see this newer prefix, and recovered FLUSHes cover their boundary.
        for (entry, request) in replay.entries.into_iter().zip(requests) {
            let _permit = shared
                .reserve(request.admission_kind())
                .ok_or_else(|| io::Error::other("replay completion reserve exhausted"))?;
            let completion = GuestCompletion {
                target: request.completion(),
                inflight: Some(entry),
                write_number: None,
            };
            match request {
                Request::Read(data) => {
                    let mut buffer = AlignedBuffer::new(data.len);
                    log.read_into(data.offset, &mut buffer)
                        .map_err(io::Error::other)?;
                    self.finish(
                        mem,
                        queue,
                        completion,
                        Status::Ok,
                        Some((&data.segments, buffer.as_slice())),
                        Some(&mut state),
                    )?;
                    self.counters.reads += 1;
                    self.counters.read_bytes += data.len as u64;
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
        self.storage = Storage::Local(Box::new(local::Local::from_log(
            log,
            &self.completion_event,
            local::Execution::Concurrent,
            shared,
        )?));
        self.live.as_mut().unwrap().phase = Phase::Active;
        Ok(())
    }
}
