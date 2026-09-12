use super::*;

#[test]
fn unused_permits_and_returned_completions_prevent_quiescence_until_final_release() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let unused = first.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    let permit = second.prepare(Kind::Read(BLOCK_SIZE)).unwrap().unwrap();
    second
        .enqueue(
            0,
            Operation::Read {
                offset: 0,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
    let returned = completed(&mut second);
    let mut pause = host.pause_admission().unwrap();
    assert_eq!(host.report()["admission"]["active"], 2);
    for kind in [
        Kind::Read(BLOCK_SIZE),
        Kind::Write(BLOCK_SIZE),
        Kind::Control,
    ] {
        assert!(first.prepare(kind).unwrap().is_none());
        assert!(second.prepare(kind).unwrap().is_none());
    }
    assert_eq!(pause.begin().unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(unused);
    assert_eq!(host.report()["admission"]["active"], 1);
    assert!(!pause.drained());
    drop(returned);
    assert!(pause.drained());
    // Internal barriers retain control credits but bypass stopped guest entry.
    first
        .pause(Instant::now() + Duration::from_secs(3))
        .unwrap();
    second
        .pause(Instant::now() + Duration::from_secs(3))
        .unwrap();
    assert_eq!(host.report()["admission"]["active"], 0);
    pause.begin().unwrap();
    assert_eq!(host.report()["admission"]["running"], true);
    pause.finish().unwrap();
    first.resume().unwrap();
    second.resume().unwrap();
    read(&mut first, 0, &[0; BLOCK_SIZE]);
    read(&mut second, 1, &[0; BLOCK_SIZE]);
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}

#[test]
fn losing_started_quiescence_closes_admission_and_fails_each_image() {
    let root = tempfile::tempdir().unwrap();
    let resources = Arc::new(Resources::default());
    let mut host = create(root.path(), 2, Arc::clone(&resources));
    let mut first = attach(&mut host, 2);
    let mut second = attach(&mut host, 3);
    let mut pause = host.pause_admission().unwrap();
    first
        .pause(Instant::now() + Duration::from_secs(3))
        .unwrap();
    second
        .pause(Instant::now() + Duration::from_secs(3))
        .unwrap();
    assert!(pause.drained());
    pause.begin().unwrap();
    drop(pause);
    let deadline = Instant::now() + Duration::from_secs(3);
    while host.shared.gate.failure().is_none() {
        assert!(
            Instant::now() < deadline,
            "lost quiescence did not fail the host"
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(host.report()["admission"]["failed"], true);
    for local in [&first, &second] {
        assert!(local.shared.reserve(Kind::Read(BLOCK_SIZE)).is_none());
        assert!(local.shared.health.lock().unwrap().failure.is_some());
    }
    assert!(host.pause_admission().is_err());
    drop((first, second));
    shutdown(host);
    assert_eq!(resources.metadata.usage().current, Amount::default());
}
