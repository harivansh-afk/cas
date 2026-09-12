use super::*;

#[test]
fn full_index_waits_for_compaction_without_rotating_or_assigning_mutations() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create_with_limits(
        root.path(),
        2,
        Arc::clone(&resources),
        MAX_REQUEST_BYTES as u64,
        append::Limits {
            intervals: 126,
            ..append::Limits::default()
        },
    );
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let (entered, observed) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    host.shared.control.lock().unwrap().compaction = Some(Pause { entered, resume });
    let mut expected = vec![0; MAX_REQUEST_BYTES];
    write(&mut first, 0, 0, &[1; BLOCK_SIZE]);
    expected[..BLOCK_SIZE].fill(1);
    observed.recv_timeout(Duration::from_secs(3)).unwrap();
    for id in 1..125 {
        let bytes = [id as u8 + 1; BLOCK_SIZE];
        let start = id * BLOCK_SIZE;
        write(&mut first, id as u64, start, &bytes);
        expected[start..start + BLOCK_SIZE].copy_from_slice(&bytes);
    }
    assert!(first.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(first.admitted, 125);
    assert_eq!(first.report()["wal_window"]["index_pressure"], true);
    assert_eq!(first.report()["wal_window"]["rotation_wanted"], false);
    assert_eq!(first.report()["status"]["segments"], 1);
    read(&mut first, 125, &expected);
    let permit = first.prepare(Kind::Control).unwrap().unwrap();
    first.enqueue(126, Operation::Flush, permit).unwrap();
    completed(&mut first);
    assert_eq!(first.status.durable, 125);
    write(&mut second, 0, 0, &[7; BLOCK_SIZE]);
    read_at(&mut second, 1, 0, &[7; BLOCK_SIZE]);
    assert!(first.prepare(Kind::Write(BLOCK_SIZE)).unwrap().is_none());
    assert_eq!(first.admitted, 125);
    release.send(()).unwrap();
    write(&mut first, 127, 0, &[9; BLOCK_SIZE]);
    expected[..BLOCK_SIZE].fill(9);
    assert_eq!(first.admitted, 126);
    drained(&mut first, 126);
    assert_eq!(first.report()["status"]["segments"], 1);
    read(&mut first, 128, &expected);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current.bytes, 0);
}
