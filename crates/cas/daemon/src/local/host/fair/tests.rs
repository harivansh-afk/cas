use super::*;

fn setup() -> (Arc<Budget>, [Port; 2]) {
    let metadata = metadata_budget();
    let admission = admission::Admission::new(2, &metadata).unwrap();
    let owner = Fair::new(2, admission, &metadata).unwrap();
    (
        metadata,
        [
            Port {
                owner: owner.clone(),
                image: 0,
            },
            Port { owner, image: 1 },
        ],
    )
}

#[test]
fn fifo_and_byte_quanta_share_ready_admission_without_idle_credit_growth() {
    let (metadata, [small, large]) = setup();
    let mut waiting = std::collections::VecDeque::new();
    for _ in 0..HEADS {
        waiting.push_back(small.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap());
    }
    let large_request = large.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    assert!(waiting[1].turn().unwrap().is_none());
    for _ in 0..QUANTUM / BLOCK_SIZE {
        assert!(large_request.turn().unwrap().is_none());
        let ticket = waiting.pop_front().unwrap();
        ticket.turn().unwrap().unwrap().commit();
        waiting.push_back(small.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap());
    }
    assert!(waiting[0].turn().unwrap().is_none());
    large_request.turn().unwrap().unwrap().commit();
    assert_eq!(small.owner.report()["images"][0]["admitted_bytes"], QUANTUM);
    assert_eq!(small.owner.report()["images"][1]["admitted_bytes"], QUANTUM);
    drop((waiting, large_request));
    assert_eq!(small.owner.report()["images"][0]["waiting"], 0);
    assert_eq!(small.owner.report()["images"][0]["deficit_bytes"], 0);
    drop((small, large));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn capacity_refusal_cancellation_and_control_do_not_consume_another_turn() {
    let (metadata, [first, second]) = setup();
    let a = first.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    let canceled = first.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let b = second.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    assert!(first.ticket(Kind::Control).unwrap().is_none());
    drop(a.turn().unwrap().unwrap()); // Resource reservation refused.
    b.turn().unwrap().unwrap().commit();
    drop(canceled);
    assert_eq!(first.owner.report()["images"][0]["admitted_bytes"], 0);
    a.turn().unwrap().unwrap().commit();
    assert_eq!(first.owner.report()["images"][0]["admitted_bytes"], QUANTUM);
    drop((a, b, first, second));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn release_during_a_refused_turn_wakes_the_waiting_frontend() {
    let (metadata, [port, other]) = setup();
    let first = port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    let release = first.turn().unwrap().unwrap().commit();
    let waiting = port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    let frontend = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
    port.owner
        .admission
        .bind(
            port.image,
            frontend.try_clone().unwrap(),
            EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap(),
        )
        .unwrap();
    let refused = waiting.turn().unwrap().unwrap();
    // Capacity changes after the reservation failed, before the turn is dropped.
    drop(release);
    assert_eq!(
        frontend.read().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(refused);
    assert_eq!(frontend.read().unwrap(), 1);
    let release = waiting.turn().unwrap().unwrap().commit();
    assert_eq!(
        port.owner.report()["images"][0]["admitted_bytes"],
        2 * QUANTUM
    );
    // A later refusal still blocks until a new release, without stale readiness.
    let next = port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    drop(next.turn().unwrap().unwrap());
    assert_eq!(
        frontend.read().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    drop(release);
    assert_eq!(frontend.read().unwrap(), 1);
    next.turn().unwrap().unwrap().commit();
    drop((first, waiting, next, port, other));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn another_queue_can_read_while_the_older_write_lacks_capacity() {
    let (metadata, [port, other]) = setup();
    let write = port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    let read = port.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    assert!(read.turn().unwrap().is_none()); // FIFO while both are eligible.
    drop(write.turn().unwrap().unwrap()); // WAL refusal on the first queue.
    let read_release = read.turn().unwrap().unwrap().commit();
    assert_eq!(port.owner.report()["images"][0]["blocked"], 1);
    assert_eq!(
        port.owner.report()["images"][0]["admitted_bytes"],
        BLOCK_SIZE
    );
    drop(read_release);
    write.turn().unwrap().unwrap().commit();
    drop((write, read, port, other));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn two_blocked_writers_do_not_preempt_each_others_eligible_reads() {
    let (metadata, ports) = setup();
    let waiting: Vec<_> = ports
        .iter()
        .map(|port| {
            (
                port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap(),
                port.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap(),
            )
        })
        .collect();
    // Match the frontend: try the ordinary write head, then its independent
    // read candidate. Writes keep refusing capacity; reads have capacity.
    let mut releases = Vec::new();
    'visits: for _ in 0..4 {
        for (write, read) in &waiting {
            drop(write.turn().unwrap());
            if let Some(turn) = read.turn().unwrap() {
                releases.push(turn.commit());
                if releases.len() == waiting.len() {
                    break 'visits;
                }
            }
        }
    }
    assert_eq!(releases.len(), waiting.len(), "each image's eligible read");
    for port in &ports {
        assert_eq!(port.owner.report()["images"][port.image]["admitted"], 1);
    }
    drop(releases);
    // Released capacity readmits the refused writes in image order.
    for (write, _) in &waiting {
        write.turn().unwrap().unwrap().commit();
    }
    drop((waiting, ports));
    assert_eq!(metadata.usage().current, Amount::default());
}

#[test]
fn a_refused_write_regains_fifo_order_ahead_of_reads_that_arrive_after_release() {
    let (metadata, [port, other]) = setup();
    let write = port.ticket(Kind::Write(QUANTUM)).unwrap().unwrap();
    let first = port.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    drop(write.turn().unwrap().unwrap()); // Refused; rejoins behind `first`.
    let release = first.turn().unwrap().unwrap().commit();
    let passing = port.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    drop(passing.turn().unwrap().unwrap().commit()); // Blocked writes never hold reads.
    drop(release); // The write is ready again and first in its image.
    let later = port.ticket(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    assert!(later.turn().unwrap().is_none());
    write.turn().unwrap().unwrap().commit();
    later.turn().unwrap().unwrap().commit();
    assert_eq!(
        port.owner.report()["images"][0]["admitted_bytes"],
        QUANTUM + 3 * BLOCK_SIZE
    );
    drop((write, first, passing, later, port, other));
    assert_eq!(metadata.usage().current, Amount::default());
}
