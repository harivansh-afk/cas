use super::*;
use crate::budget::Amount;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn budget(bytes: usize) -> Arc<Budget> {
    Budget::new(Amount { bytes, requests: 0 })
}

#[test]
fn close_drains_fifo_and_rejection_returns_original_owner() {
    let budget = budget(4096);
    let (tx, rx) = bounded(2, &budget).unwrap();
    tx.try_send(10).unwrap();
    tx.try_send(20).unwrap();
    assert_eq!(tx.try_send(30), Err(TrySendError::Full(30)));
    rx.close();
    assert_eq!(tx.try_send(40), Err(TrySendError::Disconnected(40)));
    assert_eq!(rx.try_recv(), Ok(10));
    assert_eq!(rx.recv(), Ok(20));
    assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    assert_eq!(
        rx.recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Disconnected)
    );
}

#[test]
fn producers_share_fixed_storage_without_loss_or_reordering() {
    let budget = budget(4096);
    let (tx, rx) = bounded(7, &budget).unwrap();
    let allocated = budget.usage().current;
    std::thread::scope(|scope| {
        for producer in 0..8 {
            let tx = tx.clone();
            scope.spawn(move || {
                for sequence in 0..1000 {
                    let mut value = (producer, sequence);
                    loop {
                        match tx.try_send(value) {
                            Ok(()) => break,
                            Err(TrySendError::Full(returned)) => {
                                value = returned;
                                std::thread::yield_now();
                            }
                            Err(TrySendError::Disconnected(_)) => panic!("receiver disappeared"),
                        }
                    }
                }
            });
        }
        drop(tx);
        let mut next = [0; 8];
        for _ in 0..8000 {
            let (producer, sequence) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(sequence, next[producer]);
            next[producer] += 1;
            assert_eq!(budget.usage().current, allocated);
        }
        assert_eq!(next, [1000; 8]);
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(2)),
            Err(RecvTimeoutError::Disconnected)
        );
    });
    assert_eq!(budget.usage().peak, allocated);
    drop(rx);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn ring_and_control_denial_release_all_allocations_and_last_endpoint_owns_storage() {
    let ring = 8 * size_of::<Option<u64>>();
    for bytes in [0, ring - 1, ring] {
        let budget = budget(bytes);
        assert!(bounded::<u64>(8, &budget).is_err());
        assert_eq!(budget.usage().current, Amount::default());
        assert!(budget.usage().peak.bytes <= bytes);
        if bytes == ring {
            assert_eq!(budget.usage().peak.bytes, ring);
        }
    }
    let budget = budget(4096);
    assert!(bounded::<u64>(0, &budget).is_err());
    assert_eq!(budget.usage().current, Amount::default());
    let (tx, rx) = bounded::<u64>(8, &budget).unwrap();
    let allocated = budget.usage().current;
    assert!(allocated.bytes > ring);
    let clone = tx.clone();
    drop(rx);
    drop(tx);
    assert_eq!(budget.usage().current, allocated);
    assert_eq!(clone.try_send(1), Err(TrySendError::Disconnected(1)));
    drop(clone);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn last_producer_drop_wakes_waiting_receiver() {
    let budget = budget(4096);
    let (tx, rx) = bounded::<u64>(1, &budget).unwrap();
    let clone = tx.clone();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(move || rx.recv_timeout(Duration::from_secs(2)));
        drop(tx);
        clone.try_send(1).unwrap();
        assert_eq!(waiter.join().unwrap(), Ok(1));
    });
    drop(clone);
    let (tx, rx) = bounded::<u64>(1, &budget).unwrap();
    std::thread::scope(|scope| {
        let waiter = scope.spawn(move || rx.recv_timeout(Duration::from_secs(2)));
        drop(tx);
        assert_eq!(waiter.join().unwrap(), Err(RecvTimeoutError::Disconnected));
    });
    assert_eq!(budget.usage().current, Amount::default());
}

struct Reentrant {
    sender: Sender<Reentrant>,
    drops: Arc<AtomicUsize>,
    budget: Arc<Budget>,
}

impl Drop for Reentrant {
    fn drop(&mut self) {
        assert!(self.budget.usage().current.bytes > 0);
        // Both cloning and dropping a producer acquire this same queue mutex.
        drop(self.sender.clone());
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn receiver_destroys_queued_owners_outside_mutex() {
    let budget = budget(4096);
    let drops = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = bounded(3, &budget).unwrap();
    for _ in 0..3 {
        tx.try_send(Reentrant {
            sender: tx.clone(),
            drops: Arc::clone(&drops),
            budget: Arc::clone(&budget),
        })
        .unwrap_or_else(|_| panic!("room for all three owners"));
    }
    drop(tx);
    let (done, result) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(rx);
        done.send(()).unwrap();
    });
    result.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(drops.load(Ordering::SeqCst), 3);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn poison_stops_publication_and_delivery_but_still_destroys_queued_owners() {
    let budget = budget(4096);
    let drops = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = bounded(1, &budget).unwrap();
    tx.try_send(Reentrant {
        sender: tx.clone(),
        drops: Arc::clone(&drops),
        budget: Arc::clone(&budget),
    })
    .unwrap_or_else(|_| panic!("empty queue"));
    std::thread::scope(|scope| {
        let tx = &tx;
        assert!(
            scope
                .spawn(move || {
                    let _queue = tx.0.queue.lock().unwrap();
                    panic!("inject internal queue failure");
                })
                .join()
                .is_err()
        );
    });
    assert!(matches!(rx.try_recv(), Err(TryRecvError::Disconnected)));
    assert_eq!(drops.load(Ordering::SeqCst), 0);
    drop(rx);
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    drop(tx);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn spurious_notifications_do_not_extend_receive_deadline() {
    let budget = budget(4096);
    let (tx, rx) = bounded::<u8>(1, &budget).unwrap();
    assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    assert_eq!(
        rx.recv_timeout(Duration::ZERO),
        Err(RecvTimeoutError::Timeout)
    );
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            while !stop.load(Ordering::Relaxed) {
                tx.0.ready.notify_one();
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let started = Instant::now();
        let result = rx.recv_timeout(Duration::from_millis(30));
        stop.store(true, Ordering::Relaxed);
        assert_eq!(result, Err(RecvTimeoutError::Timeout));
        assert!(started.elapsed() < Duration::from_secs(1));
    });
}

#[test]
fn racing_close_delivers_every_accepted_message_exactly_once() {
    let budget = budget(4096);
    let (tx, rx) = bounded(7, &budget).unwrap();
    let accepted = AtomicUsize::new(0);
    let mut seen = [false; 8000];
    std::thread::scope(|scope| {
        for producer in 0..8 {
            let tx = tx.clone();
            let accepted = &accepted;
            scope.spawn(move || {
                for value in producer * 1000..(producer + 1) * 1000 {
                    loop {
                        match tx.try_send(value) {
                            Ok(()) => {
                                accepted.fetch_add(1, Ordering::Relaxed);
                                break;
                            }
                            Err(TrySendError::Full(returned)) => {
                                assert_eq!(returned, value);
                                std::thread::yield_now();
                            }
                            Err(TrySendError::Disconnected(returned)) => {
                                assert_eq!(returned, value);
                                return;
                            }
                        }
                    }
                }
            });
        }
        drop(tx);
        for _ in 0..128 {
            let value = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert!(!std::mem::replace(&mut seen[value], true));
        }
        rx.close();
        while let Ok(value) = rx.recv() {
            assert!(!std::mem::replace(&mut seen[value], true));
        }
    });
    assert_eq!(
        seen.into_iter().filter(|seen| *seen).count(),
        accepted.load(Ordering::Relaxed)
    );
    drop(rx);
    assert_eq!(budget.usage().current, Amount::default());
}
