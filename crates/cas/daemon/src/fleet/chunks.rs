//! Sealed logical chunk segments: one receiver Store::insert and one data fence.
use super::{Client, Flow, Metrics, Placement};
use allocator_api2::vec::Vec;
use cas_core::{
    BLOCK_SIZE,
    aligned::AlignedBuffer,
    budget::{Budget, BudgetAllocator},
    chunk::Chunk,
    store::{file::Reader, format::MAX_CHUNKS},
};
use std::{
    io,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(crate) const HAS: u8 = 1;
pub(crate) const PUT: u8 = 2;
pub(crate) const SYNC: u8 = 6;
const RECORD: usize = 32 + BLOCK_SIZE;
pub(crate) const SEGMENT: usize = 4 + MAX_CHUNKS * RECORD + 32;
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
pub(crate) fn decode(
    body: &[u8],
    mut insert: impl FnMut(&[Chunk<'_>]) -> io::Result<()>,
) -> io::Result<()> {
    if body.len() < 4 + RECORD + 32 {
        return Err(invalid());
    }
    let count = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
    if !(1..=MAX_CHUNKS).contains(&count) || body.len() != 4 + count * RECORD + 32 {
        return Err(invalid());
    }
    let end = body.len() - 32;
    if blake3::hash(&body[..end]).as_bytes() != &body[end..] {
        return Err(invalid());
    }
    let chunk = |i: usize| -> io::Result<Chunk<'_>> {
        let entry = &body[4 + i * RECORD..4 + (i + 1) * RECORD];
        let chunk = Chunk::new(entry[32..].try_into().unwrap()).ok_or_else(invalid)?;
        if chunk.hash() != entry[..32] {
            return Err(invalid());
        }
        Ok(chunk)
    };
    let mut chunks = [chunk(0)?; MAX_CHUNKS];
    for (i, out) in chunks[..count].iter_mut().enumerate() {
        *out = chunk(i)?;
    }
    insert(&chunks[..count])
}
pub(crate) fn has(
    client: &mut Client,
    hashes: &[[u8; 32]],
    frame: &mut [u8],
    missing: &mut [u8],
) -> io::Result<()> {
    if hashes.is_empty() || hashes.len() > MAX_CHUNKS || missing.len() < hashes.len().div_ceil(8) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    for (out, hash) in frame[..hashes.len() * 32]
        .as_chunks_mut::<32>()
        .0
        .iter_mut()
        .zip(hashes)
    {
        out.copy_from_slice(hash);
    }
    let n = client.exchange(HAS, &mut frame[..hashes.len() * 32], missing)?;
    if n != hashes.len().div_ceil(8) {
        return Err(invalid());
    }
    Ok(())
}
/// Send only durable local chunks selected by placement that the peer lacks.
/// Pins are dropped before sending so they cannot stall a subsequent fleet GC.
pub(crate) fn ship(
    client: &mut Client,
    reader: &Reader,
    hashes: &[[u8; 32]],
    frame: &mut [u8],
    page: &mut [u8],
    payload: &mut [u8],
) -> io::Result<usize> {
    let mut missing = [0; MAX_CHUNKS.div_ceil(8)];
    has(client, hashes, frame, &mut missing)?;
    let mut count = 0;
    for (i, hash) in hashes.iter().enumerate() {
        if missing[i / 8] & (1 << (i % 8)) == 0 {
            continue;
        }
        let Some(plan) = reader.plan(*hash)? else {
            continue;
        };
        plan.load_with_scratch(page, payload)?;
        if blake3::hash(payload).as_bytes() != hash {
            return Err(invalid());
        }
        let entry = &mut frame[4 + count * RECORD..4 + (count + 1) * RECORD];
        entry[..32].copy_from_slice(hash);
        entry[32..].copy_from_slice(payload);
        count += 1;
    }
    if count != 0 {
        frame[..4].copy_from_slice(&(count as u32).to_le_bytes());
        let end = 4 + count * RECORD;
        let digest = blake3::hash(&frame[..end]);
        frame[end..end + 32].copy_from_slice(digest.as_bytes());
        if client.exchange(PUT, &mut frame[..end + 32], &mut [])? != 0 {
            return Err(invalid());
        }
        client.record_put_ack(count);
    }
    Ok(count)
}
const QUEUE: usize = 16384;
/// Shipped batches retained for the durability-window distribution.
pub(crate) const WINDOW_RETAIN: usize = 1024;
struct Pending {
    hashes: Vec<[u8; 32], BudgetAllocator>,
    /// Queue time of each hash, parallel to `hashes`; one host-local clock.
    queued: Vec<u64, BudgetAllocator>,
    rescan: bool,
    sync: Option<cas_core::budget::channel::Sender<io::Result<u64>>>,
}
/// One acknowledged PUT. `queued_ns` is when the batch's first hash was queued
/// after compaction publication, zero for inventory rescans; `acked_ns` is when
/// the owner's durable acknowledgement arrived. Same origin as `peer.recent`.
#[derive(Default, Clone, Copy, Debug, serde::Serialize)]
pub struct Shipped {
    pub queued_ns: u64,
    pub acked_ns: u64,
    pub chunks: u64,
    pub bytes: u64,
    pub owner: u8,
}
#[derive(Default, Clone, Copy, serde::Serialize)]
struct WindowTotals {
    batches: u64,
    window_batches: u64,
    window_ns_sum: u64,
    rescan_batches: u64,
    chunks: u64,
    bytes: u64,
}
struct Window {
    recent: Vec<Shipped, BudgetAllocator>,
    cursor: usize,
    totals: WindowTotals,
}
impl Window {
    fn record(&mut self, shipped: Shipped) {
        let totals = &mut self.totals;
        totals.batches += 1;
        totals.chunks += shipped.chunks;
        totals.bytes += shipped.bytes;
        if shipped.queued_ns == 0 {
            totals.rescan_batches += 1;
        } else {
            totals.window_batches += 1;
            totals.window_ns_sum += shipped.acked_ns.saturating_sub(shipped.queued_ns);
        }
        if self.recent.len() < WINDOW_RETAIN {
            self.recent.push(shipped);
        } else {
            self.recent[self.cursor] = shipped;
        }
        self.cursor = (self.cursor + 1) % WINDOW_RETAIN;
    }
}
struct Shared {
    pending: Mutex<Pending>,
    window: Mutex<Window>,
    origin: Instant,
    stop: AtomicBool,
    paused: AtomicBool,
    active: AtomicBool,
}
impl Shared {
    fn now(&self) -> u64 {
        Instant::now()
            .saturating_duration_since(self.origin)
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }
}
pub(crate) struct Shipper {
    budget: Arc<Budget>,
    shared: Arc<Shared>,
    worker: Mutex<Option<JoinHandle<()>>>,
}
impl Shipper {
    pub fn start(
        addresses: std::vec::Vec<(u8, SocketAddr)>,
        membership: (Placement, [u8; 16]),
        reader: Reader,
        budget: &Arc<Budget>,
        metrics: Arc<Metrics>,
        scheduler: cas_core::budget::BudgetArc<cas_core::scheduler::Scheduler>,
        origin: Instant,
    ) -> io::Result<Self> {
        let (placement, store) = membership;
        let hashes = crate::local::reserved_vec(QUEUE, budget)?;
        let queued = crate::local::reserved_vec(QUEUE, budget)?;
        let shared = Arc::new(Shared {
            pending: Mutex::new(Pending {
                hashes,
                queued,
                rescan: true,
                sync: None,
            }),
            window: Mutex::new(Window {
                recent: crate::local::reserved_vec(WINDOW_RETAIN, budget)?,
                cursor: 0,
                totals: WindowTotals::default(),
            }),
            origin,
            stop: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            active: AtomicBool::new(false),
        });
        let retained_budget = budget.clone();
        let owner = shared.clone();
        let budget = budget.clone();
        let mut frame = AlignedBuffer::try_new_in(
            SEGMENT.next_multiple_of(BLOCK_SIZE),
            BudgetAllocator::new(budget.clone()),
        )?;
        let mut page = AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(budget.clone()))?;
        let mut payload =
            AlignedBuffer::try_new_in(BLOCK_SIZE, BudgetAllocator::new(budget.clone()))?;
        let worker = thread::Builder::new()
            .name("cas-fleet-ship".into())
            .spawn(move || {
                let _scope = cas_core::scheduler::Scheduler::background(&scheduler);
                while !owner.stop.load(Ordering::Acquire) {
                    if owner.paused.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    owner.active.store(true, Ordering::Release);
                    // Pause publishes before checking active. Recheck after setting
                    // active, so a fleet freeze cannot race a new shipping pass.
                    if owner.paused.load(Ordering::Acquire) {
                        owner.active.store(false, Ordering::Release);
                        continue;
                    }
                    let sync = owner.pending.lock().unwrap().sync.take();
                    let flow = if sync.is_some() {
                        Flow::Sync
                    } else {
                        Flow::Ship
                    };
                    let result = (|| -> io::Result<u64> {
                        let mut hashes = Vec::new_in(BudgetAllocator::new(budget.clone()));
                        // Queue times stay parallel to `hashes`; a rescan has none.
                        let mut queued = Vec::new_in(BudgetAllocator::new(budget.clone()));
                        let rescan = {
                            let mut pending = owner.pending.lock().unwrap();
                            let rescan = pending.rescan;
                            pending.rescan = false;
                            if !rescan {
                                hashes
                                    .try_reserve_exact(pending.hashes.len())
                                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                                hashes.extend_from_slice(&pending.hashes);
                                queued
                                    .try_reserve_exact(pending.queued.len())
                                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                                queued.extend_from_slice(&pending.queued);
                            }
                            pending.hashes.clear();
                            pending.queued.clear();
                            rescan
                        };
                        if rescan {
                            hashes = reader.inventory(budget.clone())?;
                        }
                        let mut count = 0u64;
                        let mut failed = None;
                        for (node, address) in &addresses {
                            let result = (|| -> io::Result<u64> {
                                let mut selected =
                                    Vec::new_in(BudgetAllocator::new(budget.clone()));
                                selected
                                    .try_reserve_exact(hashes.len())
                                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                                let mut selected_queued =
                                    Vec::new_in(BudgetAllocator::new(budget.clone()));
                                selected_queued
                                    .try_reserve_exact(hashes.len())
                                    .map_err(|_| io::ErrorKind::OutOfMemory)?;
                                for (i, hash) in hashes.iter().enumerate() {
                                    if placement.owns(hash, *node) {
                                        selected.push(*hash);
                                        selected_queued.push(queued.get(i).copied().unwrap_or(0));
                                    }
                                }
                                if selected.is_empty() {
                                    return Ok(0);
                                }
                                let mut client = Client::member_at(
                                    *address,
                                    flow,
                                    metrics.clone(),
                                    placement,
                                    store,
                                    *node,
                                )?;
                                let mut sent = 0;
                                for (batch, times) in selected
                                    .chunks(MAX_CHUNKS)
                                    .zip(selected_queued.chunks(MAX_CHUNKS))
                                {
                                    if owner.stop.load(Ordering::Acquire)
                                        || owner.paused.load(Ordering::Acquire)
                                    {
                                        return Err(io::ErrorKind::Interrupted.into());
                                    }
                                    let count = ship(
                                        &mut client,
                                        &reader,
                                        batch,
                                        frame.as_mut_slice(),
                                        page.as_mut_slice(),
                                        payload.as_mut_slice(),
                                    )?;
                                    if count != 0 {
                                        // The PUT reply is the owner's durable acknowledgement;
                                        // the batch's first hash was queued earliest.
                                        owner.window.lock().unwrap().record(Shipped {
                                            queued_ns: times[0],
                                            acked_ns: owner.now(),
                                            chunks: count as u64,
                                            bytes: (4 + count * RECORD + 32) as u64,
                                            owner: *node,
                                        });
                                    }
                                    sent += count as u64;
                                }
                                Ok(sent)
                            })();
                            match result {
                                Ok(sent) => count += sent,
                                Err(error) => failed = Some(error),
                            }
                        }
                        if let Some(error) = failed {
                            return Err(error);
                        }
                        Ok(count)
                    })();
                    if result.is_err() {
                        owner.pending.lock().unwrap().rescan = true;
                    }
                    let failed = result.is_err();
                    if let Some(done) = sync {
                        let _ = done.try_send(result);
                    }
                    owner.active.store(false, Ordering::Release);
                    thread::sleep(Duration::from_millis(if failed { 250 } else { 20 }));
                }
            })?;
        Ok(Self {
            budget: retained_budget,
            shared,
            worker: Mutex::new(Some(worker)),
        })
    }
    pub fn synchronize(&self) -> io::Result<u64> {
        if self.shared.stop.load(Ordering::Acquire) || self.shared.paused.load(Ordering::Acquire) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        let (done, completion) = cas_core::budget::channel::bounded(1, &self.budget)?;
        {
            let mut pending = self.shared.pending.lock().unwrap();
            if pending.sync.is_some() {
                return Err(io::ErrorKind::WouldBlock.into());
            }
            pending.sync = Some(done);
            pending.rescan = true;
        }
        completion
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| io::ErrorKind::TimedOut)?
    }
    pub fn enqueue(&self, hashes: impl Iterator<Item = [u8; 32]>) {
        let now = self.shared.now();
        let mut pending = self.shared.pending.lock().unwrap();
        for hash in hashes {
            if pending.hashes.len() == pending.hashes.capacity() {
                pending.rescan = true;
                break;
            }
            pending.hashes.push(hash);
            pending.queued.push(now);
        }
    }
    /// Durability window of local-first writes: acknowledged batches with their
    /// queue and acknowledgement times, plus cumulative totals.
    pub fn report(&self) -> serde_json::Value {
        let window = self.shared.window.lock().unwrap();
        let mut value = serde_json::to_value(window.totals).expect("shipping totals");
        value["recent"] = serde_json::to_value(&*window.recent).expect("shipping ring");
        value["retention"] = WINDOW_RETAIN.into();
        value["next_slot"] = window.cursor.into();
        value["clock"] = "nanoseconds since peer runtime start; monotonic, host local".into();
        value
    }
    pub fn pause(&self) {
        self.shared.paused.store(true, Ordering::Release);
    }
    pub fn drained(&self) -> bool {
        !self.shared.active.load(Ordering::Acquire)
    }
    pub fn resume(&self) {
        self.shared.paused.store(false, Ordering::Release);
    }
    pub fn shutdown(&self) -> io::Result<()> {
        self.shared.stop.store(true, Ordering::Release);
        let mut worker = self.worker.lock().unwrap();
        if worker.as_ref().is_some_and(|w| !w.is_finished()) {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        if let Some(worker) = worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("shipper panicked"))?;
        }
        Ok(())
    }
}
impl Drop for Shipper {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.get_mut().unwrap().take() {
            let _ = worker.join();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn put_verifies_the_whole_segment_before_publishing_any_chunk() {
        let mut frame = vec![0; 4 + 2 * RECORD + 32];
        frame[..4].copy_from_slice(&2u32.to_le_bytes());
        for i in 0..2 {
            let start = 4 + i * RECORD;
            frame[start + 32..start + RECORD].fill(i as u8 + 1);
            let hash = blake3::hash(&frame[start + 32..start + RECORD]);
            frame[start..start + 32].copy_from_slice(hash.as_bytes());
        }
        let end = frame.len() - 32;
        let digest = blake3::hash(&frame[..end]);
        frame[end..].copy_from_slice(digest.as_bytes());
        let mut called = false;
        decode(&frame, |chunks| {
            assert_eq!(chunks.len(), 2);
            called = true;
            Ok(())
        })
        .unwrap();
        assert!(called);
        frame[4 + RECORD + 32] ^= 1;
        let digest = blake3::hash(&frame[..end]);
        frame[end..].copy_from_slice(digest.as_bytes());
        assert!(decode(&frame, |_| panic!("bad payload reached writer")).is_err());
        assert!(
            decode(&frame[..frame.len() - 1], |_| panic!(
                "short segment reached writer"
            ))
            .is_err()
        );
    }
}
