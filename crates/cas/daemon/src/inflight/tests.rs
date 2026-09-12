use std::os::unix::fs::FileExt;

use super::*;

const IMAGE_BYTES: u64 = 8 * 1024 * 1024;

fn identity() -> Identity {
    Identity {
        store: [0x21; 16],
        image: [0x52; 16],
        epoch: 17,
        attachment: 41,
    }
}

fn fresh(queues: u16) -> Carrier {
    let mut carrier = Carrier::create(
        Geometry::new(queues, 256).unwrap(),
        identity(),
        IMAGE_BYTES,
        0,
    )
    .unwrap();
    for queue in 0..queues {
        carrier.initialize_queue(queue, 0, 0).unwrap();
    }
    carrier
}

fn request(kind: Kind, queue: u16, head: u16, available: u16) -> Request {
    Request {
        kind,
        queue,
        head,
        available,
        offset: 0,
        length: if matches!(kind, Kind::Read | Kind::Write | Kind::Zero) {
            4096
        } else {
            0
        },
    }
}

fn reopen(carrier: Carrier) -> Carrier {
    let (message, file) = carrier.export().unwrap();
    drop(carrier);
    Carrier::attach(file, &message, identity(), IMAGE_BYTES).unwrap()
}

#[test]
fn reclamation_keeps_prepared_and_active_mutation_identities_until_retirement() {
    let mut carrier = fresh(2);
    assert_eq!(carrier.oldest_live_mutation().unwrap(), None);
    let first = carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
    let second = carrier.admit(request(Kind::Write, 1, 0, 0)).unwrap();
    carrier.publish(2).unwrap();
    assert_eq!(carrier.oldest_live_mutation().unwrap(), Some(1));
    carrier.complete(first, 0, || Ok(())).unwrap();
    assert_eq!(carrier.oldest_live_mutation().unwrap(), Some(2));
    carrier.complete(second, 0, || Ok(())).unwrap();
    let read = carrier.admit(request(Kind::Read, 0, 1, 1)).unwrap();
    carrier.complete(read, 1, || Ok(())).unwrap();
    let rejected = carrier.reject(request(Kind::Write, 0, 2, 2)).unwrap();
    carrier.complete(rejected, 2, || Ok(())).unwrap();
    assert_eq!(carrier.oldest_live_mutation().unwrap(), None);
    assert!(
        carrier
            .admit_observed(request(Kind::Write, 0, 3, 3), false, |_| {
                Err(io::Error::other("stop after PREPARED"))
            })
            .is_err()
    );
    assert_eq!(carrier.oldest_live_mutation().unwrap(), Some(3));
}

fn bytes(carrier: &Carrier) -> Vec<u8> {
    let mut result = vec![0; carrier.geometry.bytes()];
    carrier.mapping.file.read_exact_at(&mut result, 0).unwrap();
    result
}

#[test]
fn fd_survives_creator_and_has_bounded_standard_and_private_regions() {
    let mut original = fresh(4);
    let entry = original.admit(request(Kind::Write, 3, 255, 0)).unwrap();
    assert_eq!(original.geometry.bytes(), 155648);
    let wire = bytes(&original);
    let trailer = original.geometry.trailer();
    assert_eq!(&wire[trailer..trailer + 8], b"CASIFL02");
    assert_eq!(&wire[trailer + 16..trailer + 32], &[0x21; 16]);
    let mut replacement = reopen(original);
    let replay = replacement.reconcile(&[Some(0); 4]).unwrap();
    assert_eq!(replay.entries, vec![entry]);
    assert_eq!(
        (
            replay.highest_serial,
            replay.highest_mutation,
            replay.published
        ),
        (1, 1, 0)
    );
    let (_, file) = replacement.export().unwrap();
    assert!(file.set_len(0).is_err());
    assert!(file.set_len(1024 * 1024).is_err());
}

#[test]
fn every_admission_cut_retains_identity_and_reserves_counters() {
    for cut in 0..7 {
        let mut carrier = fresh(1);
        let entry = carrier
            .prepare(request(Kind::Write, 0, 7, 0), false)
            .unwrap();
        if cut >= 1 {
            carrier
                .descriptor(0, 7)
                .unwrap()
                .counter
                .store(entry.serial, Relaxed);
        }
        if cut >= 2 {
            carrier.descriptor(0, 7).unwrap().inflight.store(1, Release);
        }
        if cut >= 3 {
            carrier.slot(0, 7).unwrap().state.store(ACTIVE, Release);
        }
        if cut >= 4 {
            carrier.header().serial.store(entry.serial, Release);
        }
        if cut >= 5 {
            carrier.header().mutation.store(entry.mutation, Release);
        }
        if cut >= 6 {
            carrier.header().available[0].store(1, Release);
        }
        let mut replacement = reopen(carrier);
        assert_eq!(
            replacement.reconcile(&[Some(0)]).unwrap().entries,
            vec![entry],
            "cut {cut}"
        );
        assert_eq!(replacement.available(0).unwrap(), 1, "cut {cut}");
        // A second replacement interrupts replay itself; the same IDs survive.
        let mut replacement = reopen(replacement);
        assert_eq!(
            replacement.reconcile(&[Some(0)]).unwrap().entries,
            vec![entry]
        );
        let next = replacement.admit(request(Kind::Write, 0, 8, 1)).unwrap();
        assert_eq!((next.serial, next.mutation), (2, 2));
    }
}

#[test]
fn completion_cuts_replay_only_before_guest_used_publication() {
    for cut in 0..6 {
        let mut carrier = fresh(1);
        let entry = carrier.admit(request(Kind::Write, 0, 7, 0)).unwrap();
        carrier.publish(1).unwrap();
        carrier.begin_completion(entry, 0).unwrap();
        // 0: before status; 1: status written, used unchanged; 2: used published.
        let used = u16::from(cut >= 2);
        if cut >= 3 {
            carrier.descriptor(0, 7).unwrap().inflight.store(0, Release);
        }
        if cut >= 4 {
            carrier.queue(0).unwrap().used.store(1, Release);
        }
        if cut >= 5 {
            carrier.slot(0, 7).unwrap().state.store(EMPTY, Release);
        }
        let mut replacement = reopen(carrier);
        let expected = if cut < 2 { vec![entry] } else { vec![] };
        assert_eq!(
            replacement.reconcile(&[Some(used)]).unwrap().entries,
            expected,
            "cut {cut}"
        );
        let mut replacement = reopen(replacement);
        assert_eq!(
            replacement.reconcile(&[Some(used)]).unwrap().entries,
            expected
        );
    }
}

#[test]
fn retirement_allows_reuse_and_rejects_old_completion_identity() {
    let mut carrier = fresh(1);
    let first = carrier.admit(request(Kind::Write, 0, 4, 0)).unwrap();
    assert!(carrier.admit(request(Kind::Write, 0, 4, 1)).is_err());
    assert!(
        carrier
            .complete(first, 0, || panic!("unpublished write completed"))
            .is_err()
    );
    carrier.publish(1).unwrap();
    carrier.complete(first, 0, || Ok(())).unwrap();
    let second = carrier.admit(request(Kind::Write, 0, 4, 1)).unwrap();
    assert!(
        carrier
            .complete(first, 1, || panic!("stale identity completed"))
            .is_err()
    );
    assert_eq!(carrier.reconcile(&[Some(1)]).unwrap().entries, vec![second]);
}

#[test]
fn old_head_across_full_available_wrap_never_advances_cursor() {
    let mut carrier = fresh(1);
    let old = carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
    for serial in 1..=u16::MAX {
        let entry = carrier
            .admit(request(Kind::Protocol, 0, 1, serial))
            .unwrap();
        carrier.complete(entry, serial - 1, || Ok(())).unwrap();
    }
    assert_eq!(carrier.available(0).unwrap(), 0);
    let mut replacement = reopen(carrier);
    assert_eq!(
        replacement.reconcile(&[Some(u16::MAX)]).unwrap().entries,
        vec![old]
    );
    assert_eq!(replacement.available(0).unwrap(), 0);
    let next = replacement.admit(request(Kind::Write, 0, 1, 0)).unwrap();
    assert_eq!((next.serial, next.mutation), (65537, 2));
}

#[test]
fn global_order_and_boundaries_span_queues_and_empty_zero() {
    let mut carrier = fresh(4);
    let a = carrier.admit(request(Kind::Write, 3, 9, 0)).unwrap();
    let b = carrier.admit(request(Kind::Read, 0, 8, 0)).unwrap();
    let c = carrier
        .admit(Request {
            length: 0,
            ..request(Kind::Zero, 2, 7, 0)
        })
        .unwrap();
    let d = carrier.admit(request(Kind::Zero, 1, 6, 0)).unwrap();
    let e = carrier.admit(request(Kind::Flush, 3, 10, 1)).unwrap();
    assert_eq!(
        (a.mutation, b.mutation, c.mutation, d.mutation, e.mutation),
        (1, 0, 0, 2, 0)
    );
    assert_eq!(
        (b.boundary, c.boundary, d.boundary, e.boundary),
        (1, 1, 1, 2)
    );
    assert_eq!(
        reopen(carrier).reconcile(&[Some(0); 4]).unwrap().entries,
        vec![a, b, c, d, e]
    );
}

#[test]
fn read_and_flush_cannot_complete_before_the_captured_publication_boundary() {
    for kind in [Kind::Read, Kind::Flush] {
        let mut carrier = fresh(2);
        carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
        let entry = carrier.admit(request(kind, 1, 0, 0)).unwrap();
        assert!(
            carrier
                .complete(entry, 0, || panic!("boundary not yet published"))
                .is_err()
        );
        // A saved used publication with this impossible prefix must also fail
        // reconciliation before either queue's metadata is repaired.
        let before = bytes(&carrier);
        assert!(carrier.reconcile(&[Some(0), Some(1)]).is_err());
        assert_eq!(bytes(&carrier), before);
        carrier.publish(1).unwrap();
        carrier.complete(entry, 0, || Ok(())).unwrap();
    }
}

#[test]
fn exhaustion_rejects_before_prepared_or_cursor_change() {
    for exhausted_mutation in [false, true] {
        let mut carrier = fresh(1);
        if exhausted_mutation {
            carrier.header().mutation.store(u64::MAX, Release);
        } else {
            carrier.header().serial.store(u64::MAX, Release);
        }
        let before = bytes(&carrier);
        assert!(carrier.admit(request(Kind::Write, 0, 0, 0)).is_err());
        assert_eq!(bytes(&carrier), before);
    }
    let mut carrier = fresh(1);
    carrier.header().serial.store(u64::MAX - 1, Release);
    assert_eq!(
        carrier
            .admit(request(Kind::Protocol, 0, 0, 0))
            .unwrap()
            .serial,
        u64::MAX
    );
    assert!(carrier.admit(request(Kind::Protocol, 0, 1, 1)).is_err());
}

#[test]
fn failed_attachment_cannot_admit_publish_complete_or_reconnect() {
    let mut carrier = fresh(1);
    let entry = carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
    carrier.publish(1).unwrap();
    assert!(
        carrier
            .complete(entry, 0, || Err(io::Error::other(
                "guest memory write failed"
            )))
            .is_err()
    );
    assert!(carrier.admit(request(Kind::Read, 0, 1, 1)).is_err());
    assert!(carrier.publish(1).is_err());
    assert!(
        carrier
            .complete(entry, 0, || panic!("success after FAILED"))
            .is_err()
    );
    let (message, file) = carrier.export().unwrap();
    assert!(Carrier::attach(file, &message, identity(), IMAGE_BYTES).is_err());
}

#[test]
fn invalid_mapping_and_identity_are_rejected_without_modification() {
    for mutation in 0..11 {
        let original = fresh(1);
        let (mut message, file) = original.export().unwrap();
        let mut expected = identity();
        match mutation {
            0 => message.mmap_offset = 4096,
            1 => message.mmap_size += 4096,
            2 => message.queue_size = 255,
            3 => message.num_queues = 5,
            4 => expected.store[0] ^= 1,
            5 => expected.image[0] ^= 1,
            6 => expected.epoch += 1,
            7 => expected.attachment += 1,
            8 => original.header().version.store(1, Release),
            9 => original.header().reserved[0].store(1, Release),
            10 => original.header().available[0].store(65536, Release),
            _ => unreachable!(),
        }
        let before = bytes(&original);
        assert!(
            Carrier::attach(file, &message, expected, IMAGE_BYTES).is_err(),
            "case {mutation}"
        );
        assert_eq!(bytes(&original), before);
    }
    let geometry = Geometry::new(1, 256).unwrap();
    let ordinary = tempfile::tempfile().unwrap();
    ordinary.set_len(geometry.bytes() as u64).unwrap();
    assert!(Carrier::attach(ordinary, &geometry.message(), identity(), IMAGE_BYTES).is_err());
}

#[test]
fn invalid_recovery_state_is_rejected_before_any_repair() {
    for mutation in 0..7 {
        let mut carrier = fresh(2);
        let first = carrier.admit(request(Kind::Write, 0, 7, 0)).unwrap();
        let second = carrier.admit(request(Kind::Write, 1, 8, 0)).unwrap();
        carrier.publish(1).unwrap();
        carrier.begin_completion(first, 0).unwrap();
        let mut used = [Some(1), Some(0)]; // first completion otherwise needs repair
        match mutation {
            0 => used[1] = Some(2),
            1 => carrier
                .slot(1, 8)
                .unwrap()
                .serial
                .store(first.serial, Release),
            2 => carrier.slot(1, 8).unwrap().state.store(3, Release),
            3 => carrier.descriptor(1, 8).unwrap().inflight.store(0, Release), // second isn't published
            4 => carrier.slot(1, 8).unwrap().attachment.store(0, Release),
            5 => carrier.header().available[1].store(7, Release),
            6 => carrier
                .descriptor(1, 8)
                .unwrap()
                .counter
                .store(second.serial + 1, Release),
            _ => unreachable!(),
        }
        let before = bytes(&carrier);
        assert!(carrier.reconcile(&used).is_err(), "case {mutation}");
        assert_eq!(
            bytes(&carrier),
            before,
            "case {mutation} modified shared metadata"
        );
    }
}

#[test]
fn never_enabled_queue_retains_no_replay_work() {
    let geometry = Geometry::new(4, 256).unwrap();
    let mut carrier = Carrier::create(geometry, identity(), IMAGE_BYTES, 93).unwrap();
    carrier.initialize_queue(2, u16::MAX, u16::MAX).unwrap();
    let entry = carrier.admit(request(Kind::Write, 2, 1, u16::MAX)).unwrap();
    assert_eq!((entry.mutation, entry.boundary), (94, 93));
    let mut replacement = reopen(carrier);
    assert_eq!(
        replacement
            .reconcile(&[None, None, Some(u16::MAX), None])
            .unwrap()
            .entries,
        vec![entry]
    );
    assert_eq!(replacement.available(2).unwrap(), 0);
    replacement.publish(94).unwrap();
    replacement.complete(entry, u16::MAX, || Ok(())).unwrap();
    assert!(
        reopen(replacement)
            .reconcile(&[None, None, Some(0), None])
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn failed_retirement_requires_failed_state_and_never_allows_a_later_success() {
    let mut carrier = fresh(1);
    let a = carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
    let b = carrier.admit(request(Kind::Write, 0, 1, 1)).unwrap();
    assert!(
        carrier
            .complete_error(a, 0, || panic!("healthy IOERR retirement"))
            .is_err()
    );
    carrier.fail();
    carrier.complete_error(a, 0, || Ok(())).unwrap();
    assert_eq!(carrier.published(), 0);
    assert!(carrier.read_slot(0, 0).unwrap().is_none());
    assert!(carrier.publish(2).is_err());
    assert!(
        carrier
            .complete(b, 1, || panic!("success after FAILED"))
            .is_err()
    );
    assert!(
        carrier
            .complete_error(b, 0, || panic!("wrong used cursor"))
            .is_err()
    );
    carrier.complete_error(b, 1, || Ok(())).unwrap();
    let (message, file) = carrier.export().unwrap();
    drop(carrier);
    assert!(Carrier::attach(file, &message, identity(), IMAGE_BYTES).is_err());
}

#[test]
fn quiesced_queue_reset_preserves_global_ids_and_refuses_live_ownership() {
    let mut carrier = fresh(2);
    let first = carrier.admit(request(Kind::Write, 0, 0, 0)).unwrap();
    let before = bytes(&carrier);
    assert!(carrier.reset_queue(0, u16::MAX, u16::MAX).is_err());
    assert_eq!(bytes(&carrier), before);
    carrier.publish(1).unwrap();
    carrier.complete(first, 0, || Ok(())).unwrap();
    carrier.reset_queue(0, u16::MAX, u16::MAX).unwrap();
    assert_eq!(carrier.published(), 1);
    assert_eq!(carrier.available(1).unwrap(), 0);
    let mut carrier = reopen(carrier);
    assert!(
        carrier
            .reconcile(&[Some(u16::MAX), Some(0)])
            .unwrap()
            .entries
            .is_empty()
    );
    let next = carrier.admit(request(Kind::Write, 0, 0, u16::MAX)).unwrap();
    assert_eq!((next.serial, next.mutation), (2, 2));
    carrier.publish(2).unwrap();
    carrier.complete(next, u16::MAX, || Ok(())).unwrap();
    assert_eq!(carrier.available(0).unwrap(), 0);
    assert!(
        reopen(carrier)
            .reconcile(&[Some(0), Some(0)])
            .unwrap()
            .entries
            .is_empty()
    );
}

#[test]
fn rejected_outcomes_survive_admission_and_used_cuts_without_spending_mutations() {
    for kind in [Kind::Write, Kind::Read, Kind::Flush, Kind::Zero] {
        for cut in 0..=4 {
            let mut carrier = fresh(1);
            let older = carrier.admit(request(Kind::Write, 0, 3, 0)).unwrap();
            let rejected = carrier.prepare(request(kind, 0, 7, 1), true).unwrap();
            assert_eq!(
                (rejected.serial, rejected.boundary, rejected.mutation),
                (2, 1, 0)
            );
            assert!(rejected.rejected);
            if cut >= 1 {
                carrier.activate(rejected).unwrap();
            }
            if cut >= 2 {
                carrier.finish_admission(rejected);
            }
            if cut >= 3 {
                carrier.begin_completion(rejected, 0).unwrap();
            }
            let guest_used = u16::from(cut == 4);
            let mut carrier = reopen(carrier);
            let replay = carrier.reconcile(&[Some(guest_used)]).unwrap();
            assert_eq!(
                (
                    replay.highest_serial,
                    replay.highest_mutation,
                    replay.published
                ),
                (2, 1, 0)
            );
            assert_eq!(
                replay.entries,
                if cut == 4 {
                    vec![older]
                } else {
                    vec![older, rejected]
                }
            );
            if cut != 4 {
                carrier.complete(rejected, 0, || Ok(())).unwrap();
            }
            let next = carrier.admit(request(Kind::Write, 0, 8, 2)).unwrap();
            assert_eq!((next.serial, next.mutation), (3, 2));
            assert!(!next.rejected);
            carrier.publish(1).unwrap();
            carrier.complete(older, 1, || Ok(())).unwrap();
            carrier.publish(2).unwrap();
            carrier.complete(next, 2, || Ok(())).unwrap();
            assert!(
                reopen(carrier)
                    .reconcile(&[Some(3)])
                    .unwrap()
                    .entries
                    .is_empty()
            );
        }
    }
}
