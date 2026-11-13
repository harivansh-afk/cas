//! `--fast-settled-reads` against a model: overlapping writes, FLUSH, unsealed
//! batches and settled transitions interleaved with reads. Every read result
//! must equal the model whichever path served it, and the bypass must be
//! taken exactly when the documented conditions hold.
use super::*;

fn fast(local: &Local, key: &str) -> u64 {
    local.report()["fast"][key].as_u64().unwrap()
}
fn defer_compaction(host: &Host, defer: bool) {
    host.shared.control.lock().unwrap().defer_compaction = defer;
}
fn submit_read(local: &mut Local, id: u64, offset: usize, bytes: usize) {
    let permit = local.prepare(Kind::Read(bytes)).unwrap().unwrap();
    local
        .enqueue(
            id,
            Operation::Read {
                offset: offset as u64,
                buffer: AlignedBuffer::new(bytes),
            },
            permit,
        )
        .unwrap();
}
fn expect_read(completed: Completed, id: u64, expected: &[u8]) {
    assert_eq!(completed.id, id);
    assert!(completed.result.is_ok(), "{:?}", completed.result);
    let CompletionData::Read(bytes) = completed.data else {
        panic!("read response");
    };
    assert_eq!(bytes.as_slice(), expected);
}

#[test]
fn fast_settled_reads_interleaved_with_overlapping_writes_and_flush_match_a_model() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    let blocks = IMAGE_BYTES as usize / BLOCK_SIZE;
    let mut model = vec![0u8; IMAGE_BYTES as usize];
    let mut id = 1u64;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = |n: usize| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as usize) % n
    };
    // A fresh image is entirely holes: settled, served inline as zeros.
    read_at(&mut local, id, 0, &model[..4 * BLOCK_SIZE]);
    id += 1;
    assert_eq!(fast(&local, "inline_reads"), 1);
    // Hold compaction so the window after a write is deterministically unsettled.
    defer_compaction(&host, true);
    for round in 0..40 {
        let block = next(blocks - 4);
        let len = (1 + next(4)) * BLOCK_SIZE;
        let bytes = vec![(round as u8).wrapping_mul(37).wrapping_add(1); len];
        write(&mut local, id, block * BLOCK_SIZE, &bytes);
        id += 1;
        model[block * BLOCK_SIZE..block * BLOCK_SIZE + len].copy_from_slice(&bytes);
        // Overlapping and disjoint reads while the write is staged (D < P):
        // declined, served by the reactor with WAL precedence.
        let declined = fast(&local, "declined_unsettled");
        let start = block.saturating_sub(1) * BLOCK_SIZE;
        let end = (block + 4).min(blocks) * BLOCK_SIZE;
        read_at(&mut local, id, start as u64, &model[start..end]);
        id += 1;
        let far = (block + 8) % blocks * BLOCK_SIZE;
        read_at(&mut local, id, far as u64, &model[far..far + BLOCK_SIZE]);
        id += 1;
        assert_eq!(fast(&local, "declined_unsettled"), declined + 2);
        match round % 4 {
            0 => {
                // A pending FLUSH declines the bypass even for settled ranges.
                let declined = fast(&local, "declined_flush");
                let permit = local.prepare(Kind::Control).unwrap().unwrap();
                local.enqueue(id, Operation::Flush, permit).unwrap();
                let flush = id;
                id += 1;
                submit_read(&mut local, id, 0, BLOCK_SIZE);
                let read = id;
                id += 1;
                for _ in 0..2 {
                    let done = completed(&mut local);
                    if done.id == read {
                        expect_read(done, read, &model[..BLOCK_SIZE]);
                    } else {
                        assert_eq!(done.id, flush);
                    }
                }
                assert_eq!(fast(&local, "declined_flush"), declined + 1);
            }
            1 => {
                // Settle: once D == P, chunks still in the local store need a
                // store read (declined, condition 5); the reactor fills the
                // clean cache and the repeated reads are served inline.
                defer_compaction(&host, false);
                let published = local.status.published;
                drained(&mut local, published);
                let declined = fast(&local, "declined_io");
                read(&mut local, id, &model);
                id += 1;
                assert!(fast(&local, "declined_io") > declined);
                let inline = fast(&local, "inline_reads");
                read_at(&mut local, id, start as u64, &model[start..end]);
                id += 1;
                read(&mut local, id, &model);
                id += 1;
                assert_eq!(fast(&local, "inline_reads"), inline + 2);
                defer_compaction(&host, true);
            }
            2 => {
                // An unsealed overlapping write is in flight when the read is
                // admitted: the read seals it, is ordered behind it by the
                // reactor, and observes the new bytes.
                let value = (round as u8).wrapping_mul(53).wrapping_add(7);
                let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
                local
                    .gather(
                        id,
                        QueueHead {
                            queue: 0,
                            head: (id % 256) as u16,
                        },
                        (block * BLOCK_SIZE) as u64,
                        BLOCK_SIZE,
                        permit,
                        |target| {
                            target.fill(value);
                            Ok(())
                        },
                    )
                    .unwrap();
                let write = id;
                id += 1;
                model[block * BLOCK_SIZE..(block + 1) * BLOCK_SIZE].fill(value);
                let declined = fast(&local, "declined_unsettled");
                submit_read(&mut local, id, block * BLOCK_SIZE, 2 * BLOCK_SIZE);
                let read = id;
                id += 1;
                for _ in 0..2 {
                    let done = completed(&mut local);
                    if done.id == read {
                        expect_read(
                            done,
                            read,
                            &model[block * BLOCK_SIZE..(block + 2) * BLOCK_SIZE],
                        );
                    } else {
                        assert_eq!(done.id, write);
                    }
                }
                assert_eq!(fast(&local, "declined_unsettled"), declined + 1);
            }
            _ => (),
        }
    }
    defer_compaction(&host, false);
    let published = local.status.published;
    drained(&mut local, published);
    read(&mut local, id, &model);
    id += 1;
    let inline = fast(&local, "inline_reads");
    read(&mut local, id, &model);
    id += 1;
    assert_eq!(fast(&local, "inline_reads"), inline + 1);
    assert_eq!(fast(&local, "failed_reads"), 0);
    assert!(fast(&local, "declined_io") > 0);
    assert!(local.shared.health.lock().unwrap().failure.is_none());
    drop(local);
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
    // The same contents survive a reopen through the frozen path as well.
    let mut host = reopen(root.path(), 2, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    read(&mut local, id, &model);
    drop(local);
    shutdown(host);
}

/// A FLUSH the reactor holds must keep declining the bypass even when a later
/// FLUSH is rejected before it was sent (its completion must not cancel the
/// barrier), and a failed completion gate must decline every read.
#[test]
fn rejected_flush_does_not_cancel_the_in_flight_barrier_and_failed_gate_declines() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    let data = vec![0x5a; BLOCK_SIZE];
    write(&mut local, 1, 0, &data);
    let published = local.status.published;
    drained(&mut local, published);
    // Block the reactor inside a store read of block 0 (not cached yet).
    let (entered, wait) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    host.shared.control.lock().unwrap().before_fetch = Some(Pause {
        entered,
        resume: resumed,
    });
    submit_read(&mut local, 2, 0, BLOCK_SIZE);
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    // FLUSH 3 reaches the reactor's mailbox and stays in flight behind it.
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(3, Operation::Flush, permit).unwrap();
    // FLUSH 4 is rejected before it is sent; its completion is not a barrier.
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.paused = true;
    assert!(local.enqueue(4, Operation::Flush, permit).is_err());
    local.paused = false;
    let rejected = received(&mut local);
    assert_eq!(rejected.id, 4);
    assert!(rejected.result.is_err());
    // A settled hole read must still be declined while FLUSH 3 is in flight.
    let declined = fast(&local, "declined_flush");
    let inline = fast(&local, "inline_reads");
    submit_read(&mut local, 5, 8 * BLOCK_SIZE, BLOCK_SIZE);
    assert_eq!(fast(&local, "declined_flush"), declined + 1);
    assert_eq!(fast(&local, "inline_reads"), inline);
    resume.send(()).unwrap();
    for _ in 0..3 {
        let done = completed(&mut local);
        match done.id {
            2 => expect_read(done, 2, &data),
            5 => expect_read(done, 5, &[0; BLOCK_SIZE]),
            3 => (),
            other => panic!("unexpected completion {other}"),
        }
    }
    // The barrier is gone: the same hole read is inline again.
    read_at(&mut local, 6, 8 * BLOCK_SIZE as u64, &[0; BLOCK_SIZE]);
    assert_eq!(fast(&local, "inline_reads"), inline + 1);
    // A failed completion gate declines every read; the reactor rejects it.
    local
        .shared
        .health
        .lock()
        .unwrap()
        .fail("test failure".into());
    submit_read(&mut local, 7, 8 * BLOCK_SIZE, BLOCK_SIZE);
    assert_eq!(fast(&local, "declined_failed"), 1);
    let failed = received(&mut local);
    assert_eq!(failed.id, 7);
    assert!(failed.result.is_err());
    drop(local);
    shutdown(host);
}

/// Local exhaustion while joining the registry fails the read and its image
/// but not the shared host gate: here the registry runs out of leader entries
/// after the read's first block joined another leader as a waiter. No GET was
/// sent, so nothing shared failed; the reactor likewise fails only the image
/// for errors raised before its shared IO stage.
#[test]
fn registry_exhaustion_after_a_waiter_joined_fails_only_the_image() {
    use cas_core::{
        cache::fills::{Lookup, Registry},
        chunk::Chunk,
    };
    use std::net::TcpListener;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            peer_address: Some(listener.local_addr().unwrap()),
            peer_force_remote: true,
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    let first = vec![0x11; BLOCK_SIZE];
    let second = vec![0x22; BLOCK_SIZE];
    write(&mut local, 1, 0, &first);
    write(&mut local, 2, BLOCK_SIZE, &second);
    let published = local.status.published;
    drained(&mut local, published);
    // Warm the manifest page through the reactor (a hole read), so the
    // settled read below resolves inline instead of declining for a page load.
    read_at(&mut local, 10, 8 * BLOCK_SIZE as u64, &[0; BLOCK_SIZE]);
    assert_eq!(fast(&local, "declined_io"), 1);
    // Hold every leader entry; the first one is this image's block 0 chunk,
    // so the read joins it as a waiter before finding the registry full.
    let fetches = host.shared.fetches.clone();
    let mut held = Vec::new();
    match Registry::lookup(&fetches, Chunk::new(&first).unwrap().hash()).unwrap() {
        Some(Lookup::Leader(leader)) => held.push(leader),
        _ => panic!("fresh registry has a leader entry"),
    }
    let mut filler = 0u64;
    loop {
        filler += 1;
        match Registry::lookup(&fetches, *blake3::hash(&filler.to_le_bytes()).as_bytes()).unwrap() {
            Some(Lookup::Leader(leader)) => held.push(leader),
            Some(Lookup::Waiter(_)) => unreachable!("distinct hashes"),
            None => break,
        }
    }
    submit_read(&mut local, 3, 0, 2 * BLOCK_SIZE);
    let failed = received(&mut local);
    assert_eq!(failed.id, 3);
    assert!(failed.result.is_err(), "{:?}", failed.result);
    let CompletionData::Read(bytes) = failed.data else {
        panic!("read response");
    };
    assert_eq!(bytes.as_slice(), &[0; 2 * BLOCK_SIZE][..]);
    assert_eq!(fast(&local, "failed_reads"), 1);
    assert_eq!(fast(&local, "shared_waits"), 1);
    assert_eq!(fast(&local, "declined_capacity"), 0);
    assert_eq!(fast(&local, "declined_io"), 1);
    assert_eq!(host.report()["peer"]["counters"]["gets"], 0);
    assert!(local.shared.health.lock().unwrap().failure.is_some());
    assert!(
        host.shared.gate.failure().is_none(),
        "local exhaustion must not fail the shared host gate"
    );
    drop(held);
    drop(local);
    shutdown(host);
}

/// A vhost-user backend wakes only on its completion eventfd. A read served
/// inline (a hole on a settled image) must signal it like a reactor
/// completion does; without the signal the completion sat in the queue until
/// an unrelated wake and a booting guest stalled on its first settled reads.
#[test]
fn inline_completion_signals_the_frontend_completion_eventfd() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    let mut local = host.local([2; 16], &event).unwrap();
    // Drain whatever start-up wrote, then submit one inline hole read.
    let _ = event.read();
    submit_read(&mut local, 1, 0, BLOCK_SIZE);
    assert_eq!(fast(&local, "inline_reads"), 1);
    assert!(
        event.read().is_ok(),
        "an inline completion must signal the completion eventfd"
    );
    let done = received(&mut local);
    expect_read(done, 1, &[0; BLOCK_SIZE]);
    drop(local);
    shutdown(host);
}

/// The vhost-user backend holds the image completion gate while it enqueues
/// and while it publishes completions. The fast path runs on that thread and
/// must neither lock the gate (a self-deadlock that stalled a booting guest)
/// nor fail it directly; a failure it records lands at the next gate lock.
#[test]
fn fast_path_never_locks_the_completion_gate_the_frontend_holds() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut local = attach(&mut host, 2);
    let health = local.shared.health.clone();
    let guard = health.lock().unwrap();
    // Inline hole read with the gate held: served, not deadlocked.
    submit_read(&mut local, 1, 0, BLOCK_SIZE);
    assert_eq!(fast(&local, "inline_reads"), 1);
    let done = received(&mut local);
    expect_read(done, 1, &[0; BLOCK_SIZE]);
    drop(guard);
    // A failure recorded under the gate is visible once the guard drops, and
    // the fast path declines without locking.
    health.lock().unwrap().fail("held failure".into());
    assert!(health.is_failed());
    let _guard = health.lock().unwrap();
    submit_read(&mut local, 2, 0, BLOCK_SIZE);
    assert_eq!(fast(&local, "declined_failed"), 1);
}

/// A mock owner that answers GETs only when told to, by hash, so a test can
/// hold GET slots and release leaders in a chosen order.
struct HeldPeer {
    address: std::net::SocketAddr,
    release: Option<mpsc::Sender<cas_core::chunk_index::Hash>>,
    thread: Option<thread::JoinHandle<()>>,
}
impl HeldPeer {
    fn start(payloads: Vec<(cas_core::chunk_index::Hash, Vec<u8>)>) -> Self {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (release, releases) = mpsc::channel::<cas_core::chunk_index::Hash>();
        let thread = thread::spawn(move || {
            // Serve every connection the client opens (it reconnects after a
            // GET timeout); released hashes outlive a connection.
            let mut wanted: Vec<cas_core::chunk_index::Hash> = Vec::new();
            listener.set_nonblocking(true).unwrap();
            loop {
                // The test dropping its release sender ends the peer.
                if matches!(releases.try_recv(), Err(mpsc::TryRecvError::Disconnected)) {
                    return;
                }
                let mut socket = match listener.accept() {
                    Ok((socket, _)) => socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(error) => {
                        eprintln!("held peer: accept failed: {error}");
                        return;
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_millis(20)))
                    .unwrap();
                let mut requests: Vec<[u8; 56]> = Vec::new();
                let mut request = [0u8; 56];
                let mut filled = 0;
                loop {
                    match socket.read(&mut request[filled..]) {
                        Ok(0) => {
                            eprintln!("held peer: client closed the connection");
                            break;
                        }
                        Ok(n) => {
                            filled += n;
                            if filled == 56 {
                                requests.push(request);
                                filled = 0;
                            }
                        }
                        Err(error)
                            if matches!(
                                error.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                            ) => {}
                        Err(error) => {
                            eprintln!("held peer: read failed: {error}");
                            break;
                        }
                    }
                    loop {
                        match releases.try_recv() {
                            Ok(hash) => wanted.push(hash),
                            Err(mpsc::TryRecvError::Empty) => break,
                            Err(mpsc::TryRecvError::Disconnected) => return,
                        }
                    }
                    // Answer every released hash whose request has arrived.
                    let mut answered = Vec::new();
                    for (slot, hash) in wanted.iter().enumerate() {
                        let Some(index) = requests.iter().position(|r| r[24..56] == *hash) else {
                            continue;
                        };
                        answered.push(slot);
                        let hash = *hash;
                        let request = requests.remove(index);
                        let payload = payloads
                            .iter()
                            .find(|(h, _)| *h == hash)
                            .map(|(_, p)| p.clone())
                            .unwrap_or_else(|| vec![0; BLOCK_SIZE]);
                        let mut response = vec![0u8; 24 + BLOCK_SIZE];
                        response[..24].copy_from_slice(&request[..24]);
                        response[..4].copy_from_slice(&((24 + BLOCK_SIZE) as u32).to_le_bytes());
                        response[24..].copy_from_slice(&payload);
                        if let Err(error) = socket.write_all(&response) {
                            eprintln!("held peer: write failed: {error}");
                            break;
                        }
                    }
                    for slot in answered.into_iter().rev() {
                        wanted.remove(slot);
                    }
                }
            }
        });
        Self {
            address,
            release: Some(release),
            thread: Some(thread),
        }
    }
    fn release(&self, hash: cas_core::chunk_index::Hash) {
        self.release.as_ref().unwrap().send(hash).unwrap();
    }
}
impl Drop for HeldPeer {
    fn drop(&mut self) {
        // Closing the channel ends the peer's loop; then join it.
        drop(self.release.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A read that fails for local exhaustion after it already leads a fetch
/// must not fail that fetch's waiters: the leader stays behind as an orphan
/// that publishes for them, and a leader whose GET slot was exhausted
/// retries. Here the GET pool holds one free slot: the inline read's first
/// block leads and sends its GET, its second block leads but finds no slot,
/// the read fails (image only); a reactor read on another image then waits
/// on the first leader, the owner answers it, and the waiter is served; the
/// slots free up, the orphan issues the second GET and publishes too.
#[test]
fn local_exhaustion_after_a_held_leader_orphans_it_for_its_waiters() {
    use cas_core::chunk::Chunk;
    let root = tempfile::tempdir().unwrap();
    let data = |block: usize| vec![block as u8 + 1; BLOCK_SIZE];
    let hash = |block: usize| Chunk::new(&data(block)).unwrap().hash();
    let peer = HeldPeer::start((0..8).map(|b| (hash(b), data(b))).collect());
    let resources = Arc::new(Resources {
        peer: crate::peer::Config {
            peer_address: Some(peer.address),
            peer_force_remote: true,
            fast_settled_reads: true,
            ..Default::default()
        },
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    // Image 2 reads inline; image 3 reads through its reactor and shares
    // the chunks (identical content, identical hashes, force-remote).
    let mut fast_local = attach(&mut host, 2);
    let mut reactor_local = attach(&mut host, 3);
    for block in 0..8 {
        write(
            &mut fast_local,
            1 + block as u64,
            block * BLOCK_SIZE,
            &data(block),
        );
        write(
            &mut reactor_local,
            1 + block as u64,
            block * BLOCK_SIZE,
            &data(block),
        );
    }
    let published = fast_local.status.published;
    drained(&mut fast_local, published);
    let published = reactor_local.status.published;
    drained(&mut reactor_local, published);
    // Warm image 2's manifest page through the reactor: one GET, answered.
    submit_read(&mut fast_local, 100, 5 * BLOCK_SIZE, BLOCK_SIZE);
    thread::sleep(Duration::from_millis(50));
    peer.release(hash(5));
    let warm = received(&mut fast_local);
    expect_read(warm, 100, &data(5));
    assert_eq!(fast(&fast_local, "declined_io"), 1);
    // Fill the GET pool with unanswered requests, then free exactly one.
    let router = host.shared.peer.clone().unwrap();
    let mut held = Vec::new();
    let mut filler = 0u64;
    loop {
        filler += 1;
        let fake = *blake3::hash(&filler.to_le_bytes()).as_bytes();
        match router.fetch(fake) {
            Ok(ticket) => held.push((fake, ticket)),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("{error}"),
        }
    }
    let pool = held.len() as u64;
    let (freed, ticket) = held.pop().unwrap();
    drop(ticket);
    thread::sleep(Duration::from_millis(50));
    peer.release(freed);
    let until = Instant::now() + Duration::from_secs(5);
    while host.report()["peer"]["counters"]["errors"] == 0 {
        assert!(Instant::now() < until, "freed slot's reply not processed");
        thread::sleep(Duration::from_millis(5));
    }
    // The inline two-block read: block 6 leads with the last slot, block 7
    // leads with none. The read fails, the image gate fails, the host does
    // not, and both leaders are orphaned.
    submit_read(&mut fast_local, 200, 6 * BLOCK_SIZE, 2 * BLOCK_SIZE);
    let failed = received(&mut fast_local);
    assert_eq!(failed.id, 200);
    assert!(failed.result.is_err(), "{:?}", failed.result);
    assert_eq!(fast(&fast_local, "failed_reads"), 1);
    assert_eq!(fast(&fast_local, "orphaned_leaders"), 2);
    assert!(fast_local.shared.health.is_failed());
    assert!(host.shared.gate.failure().is_none());
    assert_eq!(host.report()["fetches"]["counters"]["failed"], 0);
    // A reactor read on image 3 waits on the orphaned leader of block 6.
    submit_read(&mut reactor_local, 300, 6 * BLOCK_SIZE, BLOCK_SIZE);
    let until = Instant::now() + Duration::from_secs(5);
    while host.report()["fetches"]["counters"]["joined"] == 0 {
        assert!(Instant::now() < until, "{}", host.report()["fetches"]);
        thread::sleep(Duration::from_millis(5));
    }
    // The owner answers block 6: the orphan publishes, the waiter is served.
    peer.release(hash(6));
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(fast_local.receive(false).unwrap().is_none());
        if let Some(done) = reactor_local.receive(false).unwrap() {
            expect_read(done, 300, &data(6));
            break;
        }
        assert!(
            Instant::now() < until,
            "waiter not served: {}",
            host.report()["fetches"]
        );
        thread::sleep(Duration::from_millis(5));
    }
    assert!(host.shared.gate.failure().is_none());
    assert!(
        reactor_local
            .shared
            .health
            .lock()
            .unwrap()
            .failure
            .is_none()
    );
    // Free the pool: the orphan issues block 7's GET on the next poll and
    // publishes once answered.
    for (fake, _) in &held {
        peer.release(*fake);
    }
    drop(held);
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        assert!(fast_local.receive(false).unwrap().is_none());
        // Every pool slot, the warm read, and both blocks of the failed read.
        if host.report()["peer"]["counters"]["gets"].as_u64().unwrap() >= pool + 3 {
            break;
        }
        assert!(
            Instant::now() < until,
            "{}",
            host.report()["peer"]["counters"]
        );
        thread::sleep(Duration::from_millis(5));
    }
    peer.release(hash(7));
    let until = Instant::now() + Duration::from_secs(5);
    while fast_local.report()["fast"]["orphans_failed"] == 0
        && host.report()["fetches"]["counters"]["completed"]
            .as_u64()
            .unwrap()
            < 3
    {
        assert!(fast_local.receive(false).unwrap().is_none());
        assert!(Instant::now() < until, "{}", host.report()["fetches"]);
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(fast(&fast_local, "orphans_failed"), 0);
    assert_eq!(host.report()["fetches"]["counters"]["failed"], 0);
    drop((fast_local, reactor_local));
    shutdown(host);
}
