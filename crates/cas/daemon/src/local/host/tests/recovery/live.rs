use super::*;
use crate::recovery::{Replay, Retained};
use cas_core::catalog;

fn inputs(inspected: &Inspection) -> Vec<Retained<Vec<append::Mutation>>> {
    inspected
        .images()
        .map(|(image, status)| Retained {
            image,
            epoch: status.epoch,
            highest_issued: status.published,
            mutations: Vec::new(),
        })
        .collect()
}

fn prefixes(inspected: &Inspection) -> Vec<Prefix> {
    inspected
        .images()
        .map(|(image, _)| Prefix {
            image,
            published: 0,
        })
        .collect()
}

#[derive(Default)]
struct Oracle {
    gathered: Vec<(catalog::Id, u64)>,
    published: Vec<Prefix>,
    fail: Option<catalog::Id>,
}

impl Replay for Oracle {
    fn gather(
        &mut self,
        image: catalog::Id,
        mutation: append::Mutation,
        bytes: &mut [u8],
    ) -> io::Result<()> {
        if self.fail == Some(image) {
            return Err(io::Error::other("injected recovery client loss"));
        }
        assert_eq!(mutation.sequence, 1);
        self.gathered.push((image, mutation.sequence));
        bytes.fill(image[0]);
        Ok(())
    }

    fn publish(&mut self, prefix: Prefix) -> io::Result<()> {
        self.published.push(prefix);
        Ok(())
    }
}

fn released(resources: &Resources) {
    let usage = serde_json::to_value(resources.pools.report()).unwrap();
    for pool in ["requests", "append", "read", "control"] {
        assert_eq!(usage[pool]["current"]["bytes"], 0);
        assert_eq!(usage[pool]["current"]["requests"], 0);
    }
}

#[test]
fn invalid_later_live_inputs_do_not_repair_earlier_images() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    for case in 0..6 {
        let inspected = scan(root.path(), &resources).unwrap();
        let mut saved = inputs(&inspected);
        let required = prefixes(&inspected);
        match case {
            0 => {
                saved.pop();
            }
            1 => {
                saved[1].image = [9; 16];
            }
            2 => {
                saved[1].epoch += 1;
            }
            3 => {
                saved[1].highest_issued = 1;
            }
            4 => {
                saved.push(Retained {
                    image: [9; 16],
                    epoch: 1,
                    highest_issued: 0,
                    mutations: vec![],
                });
            }
            5 => (),
            _ => unreachable!(),
        }
        let mode = if case == 5 {
            Prefixes::Cold
        } else {
            Prefixes::Retained(&required)
        };
        assert!(
            inspected
                .require(mode)
                .unwrap()
                .prepare_live(saved)
                .is_err()
        );
        assert_eq!(files(root.path()), before);
        assert_eq!(resources.metadata.usage().current, Amount::default());
    }
    let inspected = scan(root.path(), &resources).unwrap();
    let saved = inputs(&inspected);
    let required = prefixes(&inspected);
    let prepared = inspected
        .require(Prefixes::Retained(&required))
        .unwrap()
        .prepare_live(saved)
        .unwrap();
    assert_eq!(files(root.path()), before);
    assert!(Tickets::open(root.path(), Arc::clone(&resources.metadata)).is_err());
    drop(prepared);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn shared_replay_reserves_control_and_payload_capacity_before_all_output() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Absent);
    let before = files(root.path());
    for control in [true, false] {
        let inspected = scan(root.path(), &resources).unwrap();
        let limits = physical_limits(&inspected);
        let saved = inputs(&inspected);
        let required = prefixes(&inspected);
        let prepared = inspected
            .require(Prefixes::Retained(&required))
            .unwrap()
            .prepare_live(saved)
            .unwrap();
        let mut controls = Vec::new();
        let mut payloads = Vec::new();
        if control {
            while let Some(credit) = resources.pools.administrative() {
                controls.push(credit);
            }
        } else {
            while let Some(credit) = resources.pools.replay(MAX_REQUEST_BYTES) {
                payloads.push(credit);
            }
        }
        let mut oracle = Oracle::default();
        assert!(prepared.recover(limits, &mut oracle).is_err());
        assert!(oracle.gathered.is_empty() && oracle.published.is_empty());
        assert_eq!(files(root.path()), before);
        drop((controls, payloads));
        released(&resources);
        assert_eq!(resources.metadata.usage().current, Amount::default());
    }
}

#[test]
#[ignore = "requires the exclusive XFS allocation fixture"]
fn shared_replay_finishes_every_image_after_interruption_and_survives_cold_restart() {
    fn replay_inputs(inspected: &Inspection) -> Vec<Retained<Vec<append::Mutation>>> {
        let mut saved = inputs(inspected);
        for image in &mut saved {
            image.highest_issued = 2;
            image.mutations = (1..=2)
                .map(|sequence| append::Mutation {
                    id: append::format::RequestId {
                        serial: sequence * 2,
                        attachment: 7,
                        queue: 0,
                        head: sequence as u16,
                    },
                    sequence,
                    offset: if sequence == 1 { 0 } else { BLOCK_SIZE as u64 },
                    length: if sequence == 1 {
                        2 * BLOCK_SIZE as u64
                    } else {
                        BLOCK_SIZE as u64
                    },
                    kind: if sequence == 1 {
                        append::format::Kind::Write
                    } else {
                        append::format::Kind::Zero
                    },
                })
                .collect();
        }
        saved
    }
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    setup(root.path(), &resources, SnapshotFixture::Present);
    let inspected = scan(root.path(), &resources).unwrap();
    let limits = physical_limits(&inspected);
    let saved = replay_inputs(&inspected);
    let mut required = prefixes(&inspected);
    let prepared = inspected
        .require(Prefixes::Retained(&required))
        .unwrap()
        .prepare_live(saved)
        .unwrap();
    let mut interrupted = Oracle {
        fail: Some([3; 16]),
        ..Oracle::default()
    };
    assert!(prepared.recover(limits, &mut interrupted).is_err());
    assert_eq!(interrupted.gathered, vec![([2; 16], 1)]);
    assert_eq!(
        interrupted.published,
        vec![
            Prefix {
                image: [2; 16],
                published: 0
            },
            Prefix {
                image: [2; 16],
                published: 1
            },
            Prefix {
                image: [2; 16],
                published: 2
            },
            Prefix {
                image: [3; 16],
                published: 0
            },
        ]
    );
    assert_eq!(resources.metadata.usage().current, Amount::default());
    released(&resources);
    // The first image's published P is retained across this replacement.
    required[0].published = 2;
    let inspected = scan(root.path(), &resources).unwrap();
    let saved = replay_inputs(&inspected);
    let prepared = inspected
        .require(Prefixes::Retained(&required))
        .unwrap()
        .prepare_live(saved)
        .unwrap();
    let mut oracle = Oracle::default();
    let recovered = prepared.recover(limits, &mut oracle).unwrap();
    assert_eq!(oracle.gathered, vec![([3; 16], 1)]);
    let expected = vec![
        Prefix {
            image: [2; 16],
            published: 2,
        },
        Prefix {
            image: [3; 16],
            published: 0,
        },
        Prefix {
            image: [3; 16],
            published: 1,
        },
        Prefix {
            image: [3; 16],
            published: 2,
        },
    ];
    assert_eq!(oracle.published, expected);
    assert_eq!(recovered.contents().len(), 3);
    released(&resources);
    let mut host = recovered
        .into_host(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap();
    for restart in 0..2 {
        for image in 2..4 {
            let mut local = attach(&mut host, image);
            let mut expected = vec![image; 2 * BLOCK_SIZE];
            expected[BLOCK_SIZE..].fill(0);
            read(&mut local, 0, &expected);
        }
        shutdown(host);
        assert_eq!(resources.metadata.usage().current, Amount::default());
        assert_eq!(resources.compaction.usage().current, Amount::default());
        assert_eq!(resources.read_memory().usage().current, Amount::default());
        released(&resources);
        if restart == 1 {
            break;
        }
        let inspected = scan(root.path(), &resources).unwrap();
        let limits = physical_limits(&inspected);
        host = inspected
            .require(Prefixes::Cold)
            .unwrap()
            .recover_cold(limits)
            .unwrap()
            .into_host(1024 * MAX_REQUEST_BYTES as u64)
            .unwrap();
    }
}
