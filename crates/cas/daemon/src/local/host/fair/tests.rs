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
