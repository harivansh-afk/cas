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
