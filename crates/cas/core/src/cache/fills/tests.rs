use super::*;

fn metadata() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 1024 * 1024,
        requests: 0,
    })
}

fn leader(registry: &BudgetArc<Registry<u64>>, hash: Hash) -> Leader<u64> {
    let Some(Lookup::Leader(leader)) = Registry::lookup(registry, hash).unwrap() else {
        panic!("expected leader")
    };
    leader
}

fn waiter(registry: &BudgetArc<Registry<u64>>, hash: Hash) -> Waiter<u64> {
    let Some(Lookup::Waiter(waiter)) = Registry::lookup(registry, hash).unwrap() else {
        panic!("expected waiter")
    };
    waiter
}

fn signaled(waiter: &Waiter<u64>) -> bool {
    let mut descriptor = libc::pollfd {
        fd: waiter.notification(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: descriptor is initialized, covers one element, and borrows the live waiter FD.
    let count = unsafe { libc::poll(&mut descriptor, 1, 0) };
    assert!(count >= 0);
    count == 1 && descriptor.revents == libc::POLLIN
}

#[test]
fn limits_and_new_fetches_do_not_replace_retained_completion_cells() {
    let metadata = metadata();
    let registry = Registry::new(1, 2, &metadata).unwrap();
    let first = leader(&registry, [1; 32]);
    assert!(Registry::lookup(&registry, [2; 32]).unwrap().is_none());
    let a = waiter(&registry, [1; 32]);
    let b = waiter(&registry, [1; 32]);
    assert!(Registry::lookup(&registry, [1; 32]).unwrap().is_none());
    assert!(!signaled(&b));
    assert!(b.poll().unwrap().is_none());
    drop(a);
    let a = waiter(&registry, [1; 32]);
    first
        .complete(BudgetArc::try_new(7, &metadata).unwrap())
        .unwrap();
    assert!(signaled(&a) && signaled(&b));
    assert_eq!(**b.poll().unwrap().as_ref().unwrap(), 7);
    assert_eq!(registry.status().pending_keys, 0);
    assert_eq!(registry.status().leaders.current.requests, 0);
    drop(a);
    let second = leader(&registry, [1; 32]);
    let new = waiter(&registry, [1; 32]);
    assert!(!signaled(&new));
    second.fail(io::ErrorKind::InvalidData).unwrap();
    assert!(signaled(&new));
    assert_eq!(new.poll().err().unwrap().kind(), io::ErrorKind::InvalidData);
    assert_eq!(**b.poll().unwrap().as_ref().unwrap(), 7);
    let status = registry.status();
    assert_eq!(status.counters.started, 2);
    assert_eq!(status.counters.completed, 1);
    assert_eq!(status.counters.failed, 1);
    assert_eq!(status.waiters.peak.requests, 2);
    drop((registry, b, new));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn abandoned_leaders_wake_readers_and_metadata_refusals_refund_counts() {
    let metadata = metadata();
    let registry = Registry::new(2, 2, &metadata).unwrap();
    let reserve_rest = || {
        metadata
            .reserve(Amount {
                bytes: 1024 * 1024 - metadata.usage().current.bytes,
                requests: 0,
            })
            .unwrap()
    };
    let held = reserve_rest();
    assert_eq!(
        Registry::lookup(&registry, [1; 32]).err().unwrap().kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(registry.status().leaders.current.requests, 0);
    assert_eq!(registry.status().pending_keys, 0);
    drop(held);
    let active = leader(&registry, [1; 32]);
    let held = reserve_rest();
    assert_eq!(
        Registry::lookup(&registry, [1; 32]).err().unwrap().kind(),
        io::ErrorKind::OutOfMemory
    );
    assert_eq!(registry.status().waiters.current.requests, 0);
    assert_eq!(registry.status().pending_keys, 1);
    drop(held);
    let waiting = waiter(&registry, [1; 32]);
    drop(active);
    assert!(signaled(&waiting));
    assert_eq!(
        waiting.poll().err().unwrap().kind(),
        io::ErrorKind::Interrupted
    );
    assert_eq!(registry.status().leaders.current.requests, 0);
    drop(registry);
    assert!(metadata.usage().current.bytes > 0);
    drop(waiting);
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn simultaneous_misses_choose_one_leader_and_notify_every_waiter() {
    let metadata = metadata();
    let registry = Registry::new(16, 16, &metadata).unwrap();
    let mut threads = std::vec::Vec::new();
    for _ in 0..16 {
        let registry = registry.clone();
        threads.push(std::thread::spawn(move || {
            Registry::lookup(&registry, [9; 32]).unwrap().unwrap()
        }));
    }
    let mut chosen = None;
    let mut waiting = std::vec::Vec::new();
    for thread in threads {
        match thread.join().unwrap() {
            Lookup::Leader(value) => {
                assert!(chosen.replace(value).is_none());
            }
            Lookup::Waiter(value) => waiting.push(value),
        }
    }
    assert_eq!(waiting.len(), 15);
    let mut collision = [9; 32];
    collision[31] = 10; // Same table hash; a distinct full chunk identity.
    let other = leader(&registry, collision);
    let other_waiter = waiter(&registry, collision);
    other
        .complete(BudgetArc::try_new(31, &metadata).unwrap())
        .unwrap();
    assert_eq!(**other_waiter.poll().unwrap().as_ref().unwrap(), 31);
    chosen
        .unwrap()
        .complete(BudgetArc::try_new(29, &metadata).unwrap())
        .unwrap();
    for waiter in &waiting {
        assert!(signaled(waiter));
        assert_eq!(**waiter.poll().unwrap().as_ref().unwrap(), 29);
    }
    assert_eq!(registry.status().counters.started, 2);
    assert_eq!(registry.status().counters.joined, 16);
    drop((waiting, other_waiter, registry));
    assert_eq!(metadata.usage().current, Amount::default());
}
