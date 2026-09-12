use super::*;
use crate::{
    aligned::AlignedBuffer,
    budget::{Amount, Budget},
    direct,
};
use std::{
    fs,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::mpsc,
    thread,
    time::Duration,
};

fn tickets(root: &std::path::Path) -> Arc<Tickets> {
    Tickets::open(
        root,
        Budget::new(Amount {
            bytes: 1024 * 1024,
            requests: 0,
        }),
    )
    .unwrap()
}

struct Fixture {
    root: tempfile::TempDir,
    account: Arc<Governor>,
}

impl Fixture {
    fn new(allocated: u64) -> Self {
        let root = tempfile::tempdir().unwrap();
        let tickets = tickets(root.path());
        let domain = Observation::inspect(&tickets).unwrap().domain;
        let account = Arc::new(Governor {
            tickets,
            domain,
            space: Space::new(
                Limits {
                    capacity: 1000,
                    reserve: 200,
                },
                allocated,
            )
            .unwrap(),
            owner: Mutex::new(()),
            samples: Mutex::new(Default::default()),
        });
        Self { root, account }
    }

    fn observe(&self, allocated: u64) {
        self.account
            .samples
            .lock()
            .unwrap()
            .push_back(Ok(Observation {
                domain: self.account.domain,
                allocated,
            }));
    }
}

#[test]
fn observation_uses_the_retained_root_and_validates_capacity() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store");
    fs::create_dir(&path).unwrap();
    let owner = tickets(&path);
    let initial = Observation::inspect(&owner).unwrap();
    assert!(initial.capacity() > initial.allocated);
    assert!(initial.domain.unit > 0);
    assert_eq!(initial.allocated % initial.domain.unit, 0);
    assert!(
        Governor::open(
            Arc::clone(&owner),
            Limits {
                capacity: initial.capacity() + 1,
                reserve: 1,
            }
        )
        .is_err()
    );
    let account = Governor::open(
        Arc::clone(&owner),
        Limits {
            capacity: initial.capacity(),
            reserve: 1,
        },
    )
    .unwrap();
    fs::rename(&path, root.path().join("retained")).unwrap();
    fs::create_dir(&path).unwrap();
    drop(owner);
    assert_eq!(account.refresh().unwrap().domain, initial.domain);
    assert!(
        Tickets::open(
            &root.path().join("retained"),
            Budget::new(Amount::default())
        )
        .is_err()
    );
    drop(account);
    tickets(&root.path().join("retained"));
}

#[test]
fn observation_retires_only_its_promise_and_counts_partial_output() {
    let f = Fixture::new(500);
    let first = f.account.foreground(100).unwrap();
    let queued = f.account.foreground(100).unwrap();
    f.observe(540);
    let result = first.run(|| Err::<(), _>(io::Error::from_raw_os_error(libc::ENOSPC)));
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOSPC));
    let status = f.account.status();
    assert_eq!((status.allocated, status.promised), (540, 100));
    assert!(!status.failed);
    f.observe(620);
    queued.run(|| Ok(())).unwrap();
    assert_eq!(
        (f.account.status().allocated, f.account.status().promised),
        (620, 0)
    );
    assert!(!f.account.status().failed);
}

#[test]
fn delayed_frees_need_a_new_observation_before_admission_resumes() {
    let f = Fixture::new(790);
    assert!(f.account.foreground(11).is_err());
    let background = f.account.background(200).unwrap();
    assert!(f.account.background(1).is_err());
    f.observe(810); // The unlink has returned; physical reclamation is delayed.
    background.run(|| Ok(())).unwrap();
    assert!(f.account.status().pressured);
    assert!(f.account.foreground(1).is_err());
    f.observe(610);
    f.account.refresh().unwrap();
    assert!(f.account.foreground(1).is_err()); // Hysteresis, not just below the cap.
    f.observe(600);
    f.account.refresh().unwrap();
    assert!(f.account.foreground(1).is_err());
    f.observe(599);
    f.account.refresh().unwrap();
    assert!(f.account.foreground(100).is_ok());
    assert_eq!(f.account.status().peak_used, 990);
}

#[test]
fn excess_output_is_recorded_before_the_account_fails() {
    for allocated in [601, 1001, u64::MAX] {
        let f = Fixture::new(500);
        let permit = f.account.foreground(100).unwrap();
        let queued = f.account.foreground(100).unwrap();
        f.observe(allocated);
        assert!(permit.run(|| Ok(())).is_err());
        assert_eq!(
            (f.account.status().allocated, f.account.status().promised),
            (allocated, 100)
        );
        assert!(f.account.status().failed);
        assert!(f.account.foreground(1).is_err());
        assert!(
            queued
                .run(|| -> io::Result<()> { panic!("failed account executed queued IO") })
                .is_err()
        );
        assert_eq!(f.account.status().promised, 0);
        assert_eq!(f.account.status().allocated, allocated);
    }
}

#[test]
fn unknown_or_changed_domain_retains_the_complete_promise() {
    for changed in 0..5 {
        let f = Fixture::new(500);
        let permit = f.account.background(200).unwrap();
        let mut sample = Observation {
            domain: f.account.domain,
            allocated: 550,
        };
        match changed {
            1 => sample.domain.device += 1,
            2 => sample.domain.filesystem += 1,
            3 => sample.domain.capacity += 1,
            4 => sample.domain.unit += 1,
            _ => (),
        }
        f.account
            .samples
            .lock()
            .unwrap()
            .push_back(if changed == 0 {
                Err(io::Error::from_raw_os_error(libc::EIO))
            } else {
                Ok(sample)
            });
        let error = permit
            .run(|| Err::<(), _>(io::Error::other("output failed")))
            .unwrap_err();
        assert!(error.to_string().contains("output failed"));
        assert!(error.to_string().contains("physical observation"));
        let status = f.account.status();
        assert_eq!((status.allocated, status.promised), (500, 200));
        assert!(status.failed && status.background_active);
        assert!(f.account.background(1).is_err());
    }
}

#[test]
fn panic_reconciles_partial_output_and_stops_the_owner() {
    let f = Fixture::new(500);
    let permit = f.account.foreground(100).unwrap();
    f.observe(575);
    assert!(
        catch_unwind(AssertUnwindSafe(|| {
            permit
                .run(|| -> io::Result<()> { panic!("injected allocator unwind") })
                .unwrap();
        }))
        .is_err()
    );
    let status = f.account.status();
    assert_eq!((status.allocated, status.promised), (575, 0));
    assert!(status.failed);
    assert!(f.account.refresh().is_err());
    assert!(f.account.foreground(1).is_err());
}

#[test]
fn stalled_owner_retains_its_promise_and_serializes_later_io() {
    let f = Fixture::new(100);
    let first = f.account.foreground(100).unwrap();
    let second = f.account.foreground(100).unwrap();
    f.observe(150);
    f.observe(230);
    let (entered, running) = mpsc::sync_channel(1);
    let (release, resume) = mpsc::sync_channel(1);
    let first_thread = thread::spawn(move || {
        first.run(|| {
            entered.send(()).unwrap();
            resume.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok(())
        })
    });
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    let (entered, running) = mpsc::sync_channel(1);
    let second_thread = thread::spawn(move || {
        second.run(|| {
            entered.send(()).unwrap();
            Ok(())
        })
    });
    assert!(matches!(
        running.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    // Status and admission do not wait on filesystem IO; no caller timeout
    // can cancel the running worker's promise or release the locked root.
    assert_eq!(f.account.status().promised, 200);
    let queued = f.account.foreground(100).unwrap();
    assert_eq!(f.account.status().promised, 300);
    drop(queued);
    let weak = Arc::downgrade(&f.account);
    let Fixture { root, account } = f;
    drop(account);
    assert!(weak.upgrade().is_some());
    assert!(Tickets::open(root.path(), Budget::new(Amount::default())).is_err());
    release.send(()).unwrap();
    first_thread.join().unwrap().unwrap();
    second_thread.join().unwrap().unwrap();
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(weak.upgrade().is_none());
    tickets(root.path());
}

#[test]
#[ignore = "requires the packaged dedicated XFS fixture and CAS_SPACE_REPORT"]
fn dedicated_filesystem_counts_preallocation_and_failed_output() {
    let report = std::path::PathBuf::from(
        std::env::var_os("CAS_SPACE_REPORT").expect("run the packaged XFS fixture"),
    );
    let root = tempfile::tempdir().unwrap();
    let owner = tickets(root.path());
    let initial = Observation::inspect(&owner).unwrap();
    let mib = 1024 * 1024;
    let limits = Limits::new(initial.capacity(), 4 * mib, 8 * mib).unwrap();
    fs::write(
        report.with_file_name("space-conditions.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "limits": limits, "initial": initial, "output_bytes_each": 4 * mib,
            "foreground_promise": 20 * mib, "background_promise": 16 * mib,
        }))
        .unwrap(),
    )
    .unwrap();
    let account = Governor::open(Arc::clone(&owner), limits).unwrap();
    let create = |name: &str| -> io::Result<()> {
        let file = direct::open(&root.path().join(name), true)?;
        direct::preallocate(&file, 0, 4 * mib)?;
        direct::write(&file, &AlignedBuffer::new(crate::BLOCK_SIZE), 0)?;
        file.sync_all()?;
        owner.root_file().sync_all()
    };
    account
        .foreground(20 * mib)
        .unwrap()
        .run(|| create("output"))
        .unwrap();
    let allocated = account.refresh().unwrap();
    assert!(allocated.allocated >= initial.allocated + 4 * mib);
    let failed = account.foreground(20 * mib).unwrap().run(|| {
        create("retained-partial")?;
        Err::<(), _>(io::Error::from_raw_os_error(libc::ENOSPC))
    });
    assert_eq!(failed.unwrap_err().raw_os_error(), Some(libc::ENOSPC));
    let partial = account.refresh().unwrap();
    assert!(partial.allocated >= allocated.allocated + 4 * mib);
    assert_eq!(account.status().promised, 0);
    assert!(!account.status().failed);
    account
        .background(16 * mib)
        .unwrap()
        .run(|| {
            fs::remove_file(root.path().join("output"))?;
            owner.root_file().sync_all()
        })
        .unwrap();
    let unlinked = account.refresh().unwrap();
    // No exact reclaim delta: XFS may defer inode/extent reclamation. The
    // retained partial file remains physically allocated despite the error.
    assert!(unlinked.allocated >= initial.allocated + 4 * mib);
    let status = account.status();
    fs::write(
        report,
        serde_json::to_vec_pretty(&serde_json::json!({
            "initial": initial, "allocated": allocated, "partial": partial,
            "unlinked": unlinked, "limits": limits, "status": status,
            "output_bytes_each": 4 * mib, "injected_error": "ENOSPC after synced output",
        }))
        .unwrap(),
    )
    .unwrap();
}
