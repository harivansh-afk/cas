use super::*;

#[test]
fn private_images_share_verified_read_fills_without_write_admission() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: 2 * BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    assert_eq!(host.shared.cache.status().counters.fills, 0);
    read(&mut first, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.fills, 1);
    read(&mut second, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.hits, 1);
    write(&mut first, 2, 0, &[9; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().counters.fills, 1);
    read(&mut first, 3, &[9; BLOCK_SIZE]);
    read(&mut second, 2, &[7; BLOCK_SIZE]);
    assert!(host.shared.cache.status().payload.peak.bytes <= 2 * BLOCK_SIZE);
    assert!(host.shared.gate.failure().is_none());
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn reads_succeed_when_evicted_readers_hold_all_cache_capacity() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    write(&mut first, 0, 0, &[1; BLOCK_SIZE]);
    write(&mut second, 0, 0, &[2; BLOCK_SIZE]);
    drained(&mut first, 1);
    drained(&mut second, 1);
    read(&mut first, 1, &[1; BLOCK_SIZE]);
    let hash = cas_core::chunk::Chunk::new(&[1; BLOCK_SIZE])
        .unwrap()
        .hash();
    let held = host.shared.cache.get(&hash).unwrap();
    read(&mut second, 1, &[2; BLOCK_SIZE]);
    let status = host.shared.cache.status();
    assert_eq!(status.capacity_bytes, BLOCK_SIZE);
    assert_eq!(status.reader_held_bytes, BLOCK_SIZE);
    assert_eq!(status.resident_bytes, 0);
    assert_eq!(status.counters.refused, 1);
    read(&mut first, 2, &[1; BLOCK_SIZE]);
    assert!(host.shared.gate.failure().is_none());
    drop(held);
    read(&mut second, 2, &[2; BLOCK_SIZE]);
    assert_eq!(host.shared.cache.status().resident_bytes, BLOCK_SIZE);
    assert_eq!(host.shared.cache.status().payload.peak.bytes, BLOCK_SIZE);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

fn enqueue_read(local: &mut Local, id: u64) {
    let permit = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    local
        .enqueue(
            id,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
}

#[test]
fn simultaneous_images_share_one_fetch_and_retain_its_original_byte_credit() {
    shared_fetch(false);
}

#[test]
fn failed_host_gate_blocks_publication_after_the_shared_fetch_completes() {
    shared_fetch(true);
}

fn shared_fetch(fail_before_publication: bool) {
    use cas_core::cache::fills::{Lookup, Registry};

    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: 2 * BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    let (entered, wait) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    host.shared.control.lock().unwrap().fetch = Some(Pause {
        entered,
        resume: resumed,
    });
    enqueue_read(&mut first, 1);
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    enqueue_read(&mut second, 1);
    joined(&host, &mut second);
    let hash = cas_core::chunk::Chunk::new(&[7; BLOCK_SIZE])
        .unwrap()
        .hash();
    let Some(Lookup::Waiter(held)) = Registry::lookup(&host.shared.fetches, hash).unwrap() else {
        panic!("pending leader must retain its exact completion cell");
    };
    assert!(held.poll().unwrap().is_none());
    if fail_before_publication {
        host.shared
            .gate
            .fail("failed before fetch publication".into());
    }
    resume.send(()).unwrap();
    for local in [&mut first, &mut second] {
        let response = received(local);
        let mut gate = local.shared.health.lock().unwrap();
        assert_eq!(gate.failure.is_some(), fail_before_publication);
        if fail_before_publication {
            assert!(gate.publish(1).is_err());
        } else {
            assert!(response.result.is_ok());
        }
        drop(gate);
        assert_eq!(response.id, 1);
        let CompletionData::Read(bytes) = &response.data else {
            panic!("read response");
        };
        if !fail_before_publication {
            assert_eq!(bytes.as_slice(), &[7; BLOCK_SIZE]);
        }
        drop(response);
    }
    let fetched = held.poll().unwrap().unwrap();
    drop(held);
    let status = host.shared.fetches.status();
    assert_eq!(status.counters.started, 1);
    assert_eq!(status.counters.joined, 2);
    assert_eq!(status.counters.completed, 1);
    assert_eq!(status.pending_keys, 0);
    assert_eq!(status.leaders.current, Amount::default());
    assert_eq!(status.waiters.current, Amount::default());
    assert_eq!(host.shared.cache.status().counters.fills, 1);
    assert_eq!(fetched.bytes.as_slice(), &[7; BLOCK_SIZE]);
    assert_eq!(
        first.shared.pools.read.usage().current.bytes,
        MAX_REQUEST_BYTES + BLOCK_SIZE
    );
    assert_eq!(second.shared.pools.read.usage().current, Amount::default());
    assert!(resources.read_memory().usage().current.bytes >= MAX_REQUEST_BYTES + BLOCK_SIZE);
    drop(fetched);
    assert_eq!(first.shared.pools.read.usage().current, Amount::default());
    assert_eq!(
        host.shared.gate.failure().is_some(),
        fail_before_publication
    );
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

fn joined(host: &Host, second: &mut Local) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.shared.fetches.status().counters.joined == 0 {
        assert!(Instant::now() < deadline, "second image did not join fetch");
        thread::sleep(Duration::from_millis(1));
    }
    assert!(second.receive(false).unwrap().is_none());
}

#[test]
fn shared_fetch_waiter_survives_leader_scheduler_congestion() {
    congested_fetch(false);
}

#[test]
fn closing_shared_fetch_waiter_cancels_its_poll_without_waiting_for_the_leader() {
    congested_fetch(true);
}

fn congested_fetch(close_waiter: bool) {
    use cas_core::scheduler::Scheduler;

    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 3, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    let (entered, wait) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    host.shared.control.lock().unwrap().before_fetch = Some(Pause {
        entered,
        resume: resumed,
    });
    enqueue_read(&mut first, 1);
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    let queued = second.report()["metrics"]["io_queued"].as_u64().unwrap();
    enqueue_read(&mut second, 1);
    joined(&host, &mut second);
    let until = Instant::now() + Duration::from_secs(3);
    while second.report()["metrics"]["io_queued"].as_u64().unwrap() < queued + 2 {
        assert!(Instant::now() < until, "waiter poll not submitted");
        thread::sleep(Duration::from_millis(1));
    }
    // The second image just submitted its manifest read, so image 2 owns
    // the next demand visit. Hold it exactly as the existing scheduling tests do.
    let held = Scheduler::port(&host.shared.io_scheduler, 2).unwrap();
    held.ready(true).unwrap();
    resume.send(()).unwrap();
    let until = Instant::now() + Duration::from_secs(3);
    while host.shared.io_scheduler.status().ready_images != 2 {
        assert!(Instant::now() < until, "leader payload not queued");
        thread::sleep(Duration::from_millis(1));
    }
    if close_waiter {
        let health = second.shared.health.clone();
        let started = Instant::now();
        drop(second);
        let elapsed = started.elapsed();
        let failure = health.lock().unwrap().failure.clone();
        let leader_failure = first.shared.health.lock().unwrap().failure.clone();
        held.ready(false).unwrap();
        let leader = received(&mut first);
        let succeeded = leader.result.is_ok();
        let host_failed = host.shared.gate.failure();
        drop((leader, held, first, health));
        shutdown(host);
        assert!(elapsed < IO_DEADLINE + Duration::from_secs(5));
        assert_eq!(
            failure.as_deref(),
            Some("terminal storage drain deadline expired")
        );
        assert!(leader_failure.is_none());
        assert!(host_failed.is_none());
        assert!(succeeded);
        assert_eq!(resources.read_memory().usage().current, Amount::default());
        return;
    }
    let started = Instant::now();
    while started.elapsed() < IO_DEADLINE + Duration::from_secs(1) {
        thread::sleep(Duration::from_millis(10));
    }
    let leader_failure = first.shared.health.lock().unwrap().failure.clone();
    let waiter_failure = second.shared.health.lock().unwrap().failure.clone();
    // Always release the scheduler and drain both owners before asserting.
    held.ready(false).unwrap();
    let leader = received(&mut first);
    let waiter = received(&mut second);
    let outcomes = (leader.result.is_ok(), waiter.result.is_ok());
    drop((leader, waiter, held, first, second));
    shutdown(host);
    assert!(leader_failure.is_none(), "leader: {leader_failure:?}");
    assert!(
        waiter_failure.is_none(),
        "waiter: {waiter_failure:?}; results: {outcomes:?}"
    );
    assert_eq!(outcomes, (true, true));
}

#[test]
fn corrupt_shared_fetch_wakes_waiters_and_fails_every_image_before_publication() {
    use std::os::unix::fs::FileExt;

    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    let (entered, wait) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    host.shared.control.lock().unwrap().before_fetch = Some(Pause {
        entered,
        resume: resumed,
    });
    enqueue_read(&mut first, 1);
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    enqueue_read(&mut second, 1);
    joined(&host, &mut second);
    let hash = cas_core::chunk::Chunk::new(&[7; BLOCK_SIZE])
        .unwrap()
        .hash();
    let pin = host.shared.reader.plan(hash).unwrap().unwrap();
    let address = pin.address();
    let file = fs::OpenOptions::new()
        .write(true)
        .open(
            root.path()
                .join("chunks")
                .join(format!("segment-{:020}.v2", address.segment())),
        )
        .unwrap();
    file.write_all_at(&[0], address.offset()).unwrap();
    file.sync_all().unwrap();
    resume.send(()).unwrap();
    for local in [&mut first, &mut second] {
        let response = received(local);
        assert!(response.result.is_err());
        assert!(host.shared.gate.failure().is_some());
        assert!(local.shared.health.lock().unwrap().publish(1).is_err());
    }
    let status = host.shared.fetches.status();
    assert_eq!(status.counters.started, 1);
    assert_eq!(status.counters.joined, 1);
    assert_eq!(status.counters.completed, 0);
    assert_eq!(status.counters.failed, 1);
    assert_eq!(status.pending_keys, 0);
    assert_eq!(status.leaders.current, Amount::default());
    assert_eq!(status.waiters.current, Amount::default());
    assert_eq!(host.shared.cache.status().counters.fills, 0);
    drop((pin, file, first, second));
    shutdown(host);
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn manifest_page_hits_preserve_private_images_and_successor_root_contents() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources {
        cache_bytes: 2 * BLOCK_SIZE,
        metadata_cache_bytes: 4 * BLOCK_SIZE,
        ..Resources::default()
    });
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    for local in [&mut first, &mut second] {
        write(local, 0, 0, &[7; BLOCK_SIZE]);
        drained(local, 1);
    }
    assert_eq!(host.shared.pages.status().counters.fills, 0);
    read(&mut first, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.pages.status().counters.fills, 1);
    read(&mut first, 2, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.pages.status().counters.hits, 1);
    read(&mut second, 1, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.pages.status().counters.fills, 2);
    write(&mut first, 3, 0, &[9; BLOCK_SIZE]);
    drained(&mut first, 2);
    assert_eq!(host.shared.pages.status().counters.fills, 2);
    read(&mut first, 4, &[9; BLOCK_SIZE]);
    read(&mut second, 2, &[7; BLOCK_SIZE]);
    assert_eq!(host.shared.pages.status().counters.fills, 3);
    assert!(host.shared.pages.status().payload.peak.bytes <= 4 * BLOCK_SIZE);
    assert!(host.shared.gate.failure().is_none());
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}
