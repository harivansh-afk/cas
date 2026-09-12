use super::*;
use crate::budget::Amount;

fn account(capacity: u64, images: &[(u64, u64)]) -> (BudgetArc<Staging>, Arc<Budget>) {
    let metadata = Budget::new(Amount {
        bytes: 16 * 1024,
        requests: 0,
    });
    let owner = Staging::new(
        capacity,
        images.iter().map(|&(allocated, capacity)| Image {
            allocated,
            capacity,
        }),
        &metadata,
    )
    .unwrap();
    (owner, metadata)
}

#[test]
fn image_pressure_starts_at_75_stops_at_cap_and_resumes_strictly_below_60() {
    let (owner, _) = account(2000, &[(700, 1000)]);
    let permit = Staging::reserve(&owner, 0, 50).unwrap();
    let (host, image) = owner.status(0).unwrap();
    assert!(image.compaction && !image.stopped && !host.compaction);
    assert!(owner.admits(0));
    drop(permit);
    assert!(!owner.status(0).unwrap().1.compaction);
    let mut permit = Staging::reserve(&owner, 0, 300).unwrap();
    assert!(!owner.admits(0));
    assert_eq!(owner.status(0).unwrap().1.promised, 300);
    permit.start();
    permit.installed(1000).unwrap();
    assert_eq!(owner.status(0).unwrap().1.promised, 0);
    owner.reclaimed(0, 600).unwrap();
    assert!(!owner.admits(0));
    owner.reclaimed(0, 599).unwrap();
    assert!(owner.admits(0));
    assert!(!owner.status(0).unwrap().1.compaction);
}

#[test]
fn host_promises_stop_all_images_and_image_denial_does_not_stop_its_neighbor() {
    let (owner, _) = account(1000, &[(400, 1000), (300, 1000)]);
    let first = Staging::reserve(&owner, 1, 50).unwrap();
    assert!(owner.status(0).unwrap().0.compaction);
    assert!(!owner.status(1).unwrap().1.compaction);
    let second = Staging::reserve(&owner, 0, 250).unwrap();
    assert!(!owner.admits(0) && !owner.admits(1));
    assert!(Staging::reserve(&owner, 1, 1).is_err());
    drop((first, second));
    assert!(!owner.admits(0));
    owner.reclaimed(0, 299).unwrap();
    assert!(owner.admits(0) && owner.admits(1));
    assert_eq!(owner.status(0).unwrap().0.allocated, 599);

    let (owner, _) = account(4000, &[(950, 1000), (0, 1000)]);
    assert_eq!(
        Staging::reserve(&owner, 0, 100).err().unwrap().kind(),
        io::ErrorKind::WouldBlock
    );
    assert!(!owner.admits(0) && owner.admits(1));
    assert_eq!(owner.status(0).unwrap().0.promised, 0);
    let mut other = Staging::reserve(&owner, 1, 100).unwrap();
    other.start();
    other.installed(100).unwrap();
    assert_eq!(owner.status(1).unwrap().0.allocated, 1050);
    assert!(!owner.failed());
}

#[test]
fn running_owner_loss_retains_promises_and_actual_shared_metadata_lifetime() {
    let (owner, metadata) = account(1000, &[(100, 1000)]);
    let held = metadata.usage().current.bytes;
    let unused = Staging::reserve(&owner, 0, 100).unwrap();
    assert_eq!(metadata.usage().current.bytes, held);
    drop(unused);
    assert_eq!(owner.status(0).unwrap().0.promised, 0);
    let observer = owner.clone();
    let mut running = Staging::reserve(&owner, 0, 100).unwrap();
    running.start();
    drop(owner);
    assert_eq!(metadata.usage().current.bytes, held);
    drop(running);
    assert!(observer.failed());
    assert!(!observer.admits(0));
    assert_eq!(observer.status(0).unwrap().0.promised, 100);
    drop(observer);
    assert_eq!(metadata.usage().current.bytes, 0);
}

#[test]
fn excess_output_is_recorded_and_invalid_receipts_never_refund_unknown_output() {
    let (owner, _) = account(1000, &[(100, 1000)]);
    let mut permit = Staging::reserve(&owner, 0, 100).unwrap();
    permit.start();
    assert!(permit.installed(201).is_err());
    assert!(owner.failed());
    let (host, image) = owner.status(0).unwrap();
    assert_eq!(
        (host.allocated, image.allocated, host.promised),
        (201, 201, 0)
    );
    assert!(!owner.admits(0));
    for started in [false, true] {
        let (owner, _) = account(1000, &[(100, 1000)]);
        let mut permit = Staging::reserve(&owner, 0, 100).unwrap();
        if started {
            permit.start();
        }
        assert!(permit.installed(if started { 99 } else { 200 }).is_err());
        assert!(owner.failed());
        assert_eq!(owner.status(0).unwrap().0.promised, 100);
    }
    let (owner, _) = account(1000, &[(100, 1000)]);
    assert!(owner.reclaimed(0, 101).is_err());
    assert!(owner.failed());
}

#[test]
fn invalid_geometry_and_metadata_denial_leave_no_allocations_or_pressure() {
    let empty = Budget::new(Amount::default());
    assert!(
        Staging::new(
            1000,
            [Image {
                allocated: 0,
                capacity: 1000
            }]
            .into_iter(),
            &empty
        )
        .is_err()
    );
    assert_eq!(empty.usage().current.bytes, 0);
    let metadata = Budget::new(Amount {
        bytes: 4096,
        requests: 0,
    });
    for images in [
        [Image {
            allocated: 0,
            capacity: 0,
        }; 2],
        [Image {
            allocated: u64::MAX,
            capacity: u64::MAX,
        }; 2],
    ] {
        assert!(Staging::new(u64::MAX, images.into_iter(), &metadata).is_err());
        assert_eq!(metadata.usage().current.bytes, 0);
    }
    let (owner, _) = account(1000, &[(100, 500)]);
    for (index, bytes) in [(0, 0), (0, 501), (1, 100)] {
        assert!(Staging::reserve(&owner, index, bytes).is_err());
        assert!(owner.admits(0));
        assert_eq!(owner.status(0).unwrap().0.promised, 0);
    }
}

#[test]
fn concurrent_reservations_share_one_host_cap() {
    let (owner, _) = account(500, &[(0, 1000), (0, 1000)]);
    let barrier = Arc::new(std::sync::Barrier::new(17));
    let threads: std::vec::Vec<_> = (0..16)
        .map(|index| {
            let owner = owner.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let permit = Staging::reserve(&owner, index % 2, 50).ok();
                barrier.wait();
                permit
            })
        })
        .collect();
    barrier.wait();
    let permits: std::vec::Vec<_> = threads
        .into_iter()
        .filter_map(|thread| thread.join().unwrap())
        .collect();
    assert_eq!(permits.len(), 10);
    assert_eq!(owner.status(0).unwrap().0.promised, 500);
    assert!(!owner.admits(0) && !owner.admits(1));
    drop(permits);
    assert_eq!(owner.status(0).unwrap().0.promised, 0);
    assert!(owner.admits(0) && owner.admits(1));
}
