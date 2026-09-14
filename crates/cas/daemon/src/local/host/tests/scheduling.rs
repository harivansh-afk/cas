use super::*;
use cas_core::scheduler::Scheduler;

fn queue_write(local: &mut Local) {
    let permit = local.prepare(Kind::Write(BLOCK_SIZE)).unwrap().unwrap();
    local
        .gather(
            0,
            QueueHead { queue: 0, head: 0 },
            0,
            BLOCK_SIZE,
            permit,
            |bytes| {
                bytes.fill(7);
                Ok(())
            },
        )
        .unwrap();
    local.seal().unwrap();
}

#[test]
fn deferred_bulk_retains_owners_and_other_image_flush_bypasses_its_turn() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    // A continuously ready first image owns the next demand opportunity.
    // Keep that opportunity pending to observe the second reactor's real queue.
    let held = Scheduler::port(&host.shared.io_scheduler, 0).unwrap();
    held.ready(true).unwrap();
    queue_write(&mut second);
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.shared.io_scheduler.status().ready_images != 2 {
        assert!(
            Instant::now() < deadline,
            "bulk owner did not reach scheduler"
        );
        thread::yield_now();
    }
    assert!(second.receive(false).unwrap().is_none());
    assert_eq!(second.report()["metrics"]["io_queued"], 0);
    assert!(second.shared.pools.append.usage().current.bytes > 0);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(0, Operation::Flush, permit).unwrap();
    assert_eq!(completed(&mut first).id, 0);
    assert!(second.receive(false).unwrap().is_none());
    assert_eq!(host.shared.io_scheduler.counters().demand, 0);
    held.ready(false).unwrap();
    assert_eq!(completed(&mut second).id, 0);
    read(&mut second, 1, &[7; BLOCK_SIZE]);
    drained(&mut second, 1);
    let counts = host.shared.io_scheduler.counters();
    assert!(counts.demand > 0);
    assert!(counts.background > 0);
    assert!(host.shared.gate.failure().is_none());
    drop((held, first, second));
    shutdown(host);
    assert_eq!(
        serde_json::to_value(resources.pools.report()).unwrap()["append"]["current"]["bytes"],
        0
    );
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn unsubmitted_bulk_waits_beyond_io_deadline_then_completes() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let held = Scheduler::port(&host.shared.io_scheduler, 0).unwrap();
    held.ready(true).unwrap();
    let started = Instant::now();
    queue_write(&mut second);
    while started.elapsed() < IO_DEADLINE + Duration::from_secs(1) {
        assert!(
            second.receive(false).unwrap().is_none(),
            "queued IO completed before capacity returned"
        );
        assert!(second.shared.health.lock().unwrap().failure.is_none());
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(host.shared.io_scheduler.counters().demand, 0);
    assert!(second.shared.pools.append.usage().current.bytes > 0);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(0, Operation::Flush, permit).unwrap();
    assert_eq!(completed(&mut first).id, 0);
    held.ready(false).unwrap();
    assert_eq!(completed(&mut second).id, 0);
    read(&mut second, 1, &[7; BLOCK_SIZE]);
    drained(&mut second, 1);
    assert!(host.shared.gate.failure().is_none());
    drop((held, first, second));
    shutdown(host);
    assert_eq!(
        serde_json::to_value(resources.pools.report()).unwrap()["append"]["current"]["bytes"],
        0
    );
    assert_eq!(resources.read_memory().usage().current, Amount::default());
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn terminal_close_cancels_unsubmitted_bulk_after_its_drain_deadline() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let held = Scheduler::port(&host.shared.io_scheduler, 0).unwrap();
    held.ready(true).unwrap();
    queue_write(&mut second);
    let health = second.shared.health.clone();
    let (done, observed) = mpsc::sync_channel(1);
    let started = Instant::now();
    let dropping = thread::spawn(move || {
        drop(second);
        done.send(()).unwrap();
    });
    observed
        .recv_timeout(IO_DEADLINE + Duration::from_secs(5))
        .unwrap();
    dropping.join().unwrap();
    assert!(started.elapsed() >= IO_DEADLINE);
    assert_eq!(
        health.lock().unwrap().failure.as_deref(),
        Some("terminal storage drain deadline expired")
    );
    assert_eq!(host.shared.io_scheduler.counters().demand, 0);
    assert!(host.shared.gate.failure().is_none());
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(0, Operation::Flush, permit).unwrap();
    completed(&mut first);
    held.ready(false).unwrap();
    drop((health, held, first));
    shutdown(host);
    assert_eq!(
        serde_json::to_value(resources.pools.report()).unwrap()["append"]["current"]["bytes"],
        0
    );
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}
