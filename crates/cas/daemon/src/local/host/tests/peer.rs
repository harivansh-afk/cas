//! Native daemon harness: the same admitted Local/reactor path as vhost-user.
use super::*;
use crate::peer;
use cas_core::chunk::Chunk;
use std::{
    io::{BufRead, Write},
    net::{SocketAddr, TcpListener},
    process::{Child, Command, Stdio},
};

const BLOCKS: usize = 64;
fn data(block: usize) -> [u8; BLOCK_SIZE] {
    [block as u8 + 1; BLOCK_SIZE]
}
fn seeded(root: &Path, resources: Arc<Resources>, local_copy: bool) -> Host {
    seeded_images(root, resources, local_copy, &[2])
}
/// Every image holds the same 64 blocks, so one remote owner serves them all.
fn seeded_images(root: &Path, resources: Arc<Resources>, local_copy: bool, ids: &[u8]) -> Host {
    let tickets = Tickets::open(root, Arc::clone(&resources.metadata)).unwrap();
    let mut store = Store::create(
        Arc::clone(&tickets),
        STORE,
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    fs::create_dir(root.join("images")).unwrap();
    // Test-only absent-index fixture: compact into another store, keeping the
    // original local store empty. Production uses force-remote on full replicas.
    let remote_root = tempfile::tempdir().unwrap();
    let mut remote_store = if local_copy {
        None
    } else {
        let remote_tickets =
            Tickets::open(remote_root.path(), Arc::clone(&resources.metadata)).unwrap();
        Some(
            Store::create(
                remote_tickets,
                STORE,
                Arc::clone(&resources.metadata),
                resources.read_memory(),
            )
            .unwrap(),
        )
    };
    let mut images = Vec::new();
    for &id in ids {
        let directory = path(root, id);
        fs::create_dir(&directory).unwrap();
        let mut manifest =
            Manifest::create(&directory, identity(id), Arc::clone(&resources.metadata)).unwrap();
        let mut log = Log::create_shared(
            Arc::clone(&tickets),
            append::Config {
                store: STORE.store,
                image: [id; 16],
                image_bytes: IMAGE_BYTES,
                segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
            },
            append::Limits::default(),
            Arc::clone(&resources.metadata),
            manifest.view().unwrap(),
        )
        .unwrap();
        let mut builder =
            cas_core::append::format::Builder::new(IMAGE_BYTES, BLOCKS * BLOCK_SIZE).unwrap();
        builder
            .write(
                cas_core::append::format::RequestId {
                    attachment: 1,
                    serial: 1,
                    queue: 0,
                    head: 0,
                },
                0,
                BLOCKS * BLOCK_SIZE,
                |bytes| {
                    for (block, bytes) in
                        bytes.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate()
                    {
                        bytes.copy_from_slice(&data(block));
                    }
                    Ok(())
                },
            )
            .unwrap();
        log.append(builder).unwrap();
        log.flush().unwrap();
        while let Some(selection) = log
            .select_compaction(Arc::clone(&resources.metadata), resources.read_memory())
            .unwrap()
        {
            let input = selection.load().unwrap();
            let compacted = input
                .write(remote_store.as_mut().unwrap_or(&mut store), &mut manifest)
                .unwrap();
            log.publish_compaction(compacted).unwrap();
        }
        images.push((log, manifest));
    }
    drop(remote_store);
    Host::new(resources, store, images).unwrap()
}
fn resources(config: peer::Config) -> Arc<Resources> {
    resources_serving_after(config, 0)
}
/// A host whose own GET service delays every reply by `reply_delay_us`.
fn resources_serving_after(config: peer::Config, reply_delay_us: u64) -> Arc<Resources> {
    // CAS_FAST_SETTLED_READS runs every fixture through the frontend-inline
    // path; CAS_PEER_RACE_US arms the race on every fixture that fetches
    // (a two-host reader without a second owner or an explicit local source
    // stays unraced, with the flag on). Local racing stays test-explicit.
    let fetches = config.peer_address.is_some() || !config.peer_get_member.is_empty();
    Arc::new(Resources {
        peer: peer::Config {
            fast_settled_reads: config.fast_settled_reads
                || std::env::var_os("CAS_FAST_SETTLED_READS").is_some(),
            peer_race_us: config.peer_race_us.or_else(|| {
                fetches
                    .then(|| std::env::var("CAS_PEER_RACE_US").ok())
                    .flatten()
                    .map(|v| v.parse().unwrap())
            }),
            ..config
        },
        cache_bytes: BLOCKS * BLOCK_SIZE,
        peer_reply_delay_us: reply_delay_us,
        ..Resources::default()
    })
}
fn submit(local: &mut Local, id: u64, block: usize) {
    let received = Instant::now();
    let until = received + Duration::from_secs(5);
    let mut permit = loop {
        if let Some(permit) = local.prepare(Kind::Read(BLOCK_SIZE)).unwrap() {
            break permit;
        }
        assert!(
            Instant::now() < until,
            "read admission deadline at {id}: {}",
            local.report()
        );
        thread::sleep(Duration::from_micros(50));
    };
    permit.received = Some(received);
    local
        .enqueue(
            id,
            Operation::Read {
                offset: (block * BLOCK_SIZE) as u64,
                buffer: AlignedBuffer::new(BLOCK_SIZE),
            },
            permit,
        )
        .unwrap();
}
fn receive(local: &mut Local, expected: Option<usize>) {
    let mut completion = local.receive(true).unwrap().unwrap();
    if let Some(trace) = completion.permit.as_mut().and_then(|p| p.remote.as_mut()) {
        trace.finish(completion.result.is_ok());
    }
    if let Some(block) = expected {
        completion.result.unwrap();
        let CompletionData::Read(bytes) = completion.data else {
            panic!("read data");
        };
        assert_eq!(bytes.as_slice(), &data(block));
    } else {
        assert!(completion.result.is_err());
        let CompletionData::Read(bytes) = completion.data else {
            panic!("read data");
        };
        assert!(bytes.as_slice().iter().all(|&b| b == 0));
    }
}
fn get(local: &mut Local, id: u64, block: usize) {
    submit(local, id, block);
    receive(local, Some(block));
}

#[test]
fn local_index_miss_fetches_verified_bytes_caches_optionally_and_has_reports_durable_only() {
    let root_b = tempfile::tempdir().unwrap();
    let root_a = tempfile::tempdir().unwrap();
    let rb = resources(peer::Config {
        peer_listen: Some("127.0.0.1:0".parse().unwrap()),
        ..Default::default()
    });
    let b = seeded(root_b.path(), rb.clone(), true);
    let address = b.peer_address().unwrap();
    let ra = resources(peer::Config {
        peer_address: Some(address),
        peer_cache: true,
        ..Default::default()
    });
    let mut a = seeded(root_a.path(), ra.clone(), false);
    let mut local = attach(&mut a, 2);
    assert!(
        a.shared
            .reader
            .plan(Chunk::new(&data(0)).unwrap().hash())
            .unwrap()
            .is_none()
    );
    get(&mut local, 0, 0);
    get(&mut local, 1, 0);
    assert_eq!(a.report()["peer"]["counters"]["gets"], 1);
    assert_eq!(a.shared.cache.status().counters.hits, 1);
    let cached_only = Chunk::new(&[99; BLOCK_SIZE]).unwrap().hash();
    b.shared.cache.fill(cached_only, &[99; BLOCK_SIZE]).unwrap();
    let mut missing = [0];
    peer::has(
        address,
        &[Chunk::new(&data(0)).unwrap().hash(), [0; 32], cached_only],
        &mut missing,
    )
    .unwrap();
    assert_eq!(missing, [0b110]);
    assert_eq!(b.report()["peer"]["counters"]["server_store"], 1);
    drop(local);
    shutdown(a);
    shutdown(b);
    for r in [ra, rb] {
        assert_eq!(r.metadata.usage().current, Amount::default());
    }
}

#[test]
fn per_image_remote_read_attribution_sums_to_the_host_counters() {
    let root_b = tempfile::tempdir().unwrap();
    let root_a = tempfile::tempdir().unwrap();
    let rb = resources(peer::Config {
        peer_listen: Some("127.0.0.1:0".parse().unwrap()),
        ..Default::default()
    });
    let b = seeded(root_b.path(), rb.clone(), true);
    let ra = resources(peer::Config {
        peer_address: Some(b.peer_address().unwrap()),
        ..Default::default()
    });
    let mut a = seeded_images(root_a.path(), ra.clone(), false, &[2, 3]);
    let mut first = attach(&mut a, 2);
    let mut second = attach(&mut a, 3);
    for id in 0..5 {
        get(&mut first, id, id as usize);
    }
    for id in 0..3 {
        get(&mut second, 100 + id, 10 + id as usize);
    }
    // Stop the owner: the next remote read on image 3 fails and is attributed there.
    shutdown(b);
    submit(&mut second, 103, 20);
    receive(&mut second, None);
    let report = a.report();
    let image = |id: u8| report["peer"]["images"][format!("{id:02x}").repeat(16)].clone();
    assert_eq!(
        image(2),
        serde_json::json!({"completed_reads": 5, "failed_reads": 0, "chunks": 5,
            "verified_bytes": 5 * BLOCK_SIZE})
    );
    // The failed read attempted one more remote chunk; it verified no payload.
    assert_eq!(
        image(3),
        serde_json::json!({"completed_reads": 4, "failed_reads": 1, "chunks": 4,
            "verified_bytes": 3 * BLOCK_SIZE})
    );
    assert_eq!(report["peer"]["images"].as_object().unwrap().len(), 2);
    let counters = &report["peer"]["counters"];
    assert_eq!(counters["completed_reads"], 9);
    assert_eq!(counters["failed_reads"], 1);
    assert_eq!(counters["verified"], 8);
    assert_verified_bytes_sum(&report);
    assert_eq!(
        a.shared.peer_metrics.as_ref().unwrap().image([3; 16]),
        peer::metrics::ImageCounters {
            completed_reads: 4,
            failed_reads: 1,
            chunks: 4,
            verified_bytes: 3 * BLOCK_SIZE as u64,
        }
    );
    drop((first, second));
    shutdown(a);
    for r in [ra, rb] {
        assert_eq!(r.metadata.usage().current, Amount::default());
    }
}

#[test]
fn coalesced_remote_reads_credit_verified_bytes_to_the_issuing_image_once() {
    use std::io::Read as _;
    const BLOCK: usize = 7;
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, released) = mpsc::channel::<()>();
    // A peer that holds its one correct reply until both images are waiting.
    let mock = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut request = [0; 56];
        socket.read_exact(&mut request).unwrap();
        assert_eq!(&request[24..], Chunk::new(&data(BLOCK)).unwrap().hash());
        released.recv_timeout(Duration::from_secs(10)).unwrap();
        let mut response = [0; 24 + BLOCK_SIZE];
        response[..24].copy_from_slice(&request[..24]);
        response[..4].copy_from_slice(&((24 + BLOCK_SIZE) as u32).to_le_bytes());
        response[24..].copy_from_slice(&data(BLOCK));
        socket.write_all(&response).unwrap();
        // Stay connected until the host is gone so no reconnect is attempted.
        let mut rest = [0; 1];
        let _ = socket.read(&mut rest);
    });
    let r = resources(peer::Config {
        peer_address: Some(address),
        ..Default::default()
    });
    let mut a = seeded_images(root.path(), r.clone(), false, &[2, 3]);
    let mut first = attach(&mut a, 2);
    let mut second = attach(&mut a, 3);
    submit(&mut first, 0, BLOCK);
    let until = Instant::now() + Duration::from_secs(5);
    while a.report()["peer"]["counters"]["gets"] != 1 {
        assert!(Instant::now() < until, "{}", a.report());
        thread::sleep(Duration::from_millis(1));
    }
    submit(&mut second, 1, BLOCK);
    while a.report()["fetches"]["counters"]["joined"] != 1 {
        assert!(Instant::now() < until, "{}", a.report());
        thread::sleep(Duration::from_millis(1));
    }
    release.send(()).unwrap();
    receive(&mut first, Some(BLOCK));
    receive(&mut second, Some(BLOCK));
    let report = a.report();
    let image = |id: u8| report["peer"]["images"][format!("{id:02x}").repeat(16)].clone();
    assert_eq!(report["peer"]["counters"]["gets"], 1);
    assert_eq!(report["peer"]["counters"]["verified"], 1);
    assert_eq!(report["peer"]["counters"]["completed_reads"], 2);
    // Both reads resolved the chunk remotely; only the GET's issuer is
    // credited with verified payload, so the bytes still sum to the host count.
    assert_eq!(
        image(2),
        serde_json::json!({"completed_reads": 1, "failed_reads": 0, "chunks": 1,
            "verified_bytes": BLOCK_SIZE})
    );
    assert_eq!(
        image(3),
        serde_json::json!({"completed_reads": 1, "failed_reads": 0, "chunks": 1,
            "verified_bytes": 0})
    );
    assert_verified_bytes_sum(&report);
    drop((first, second));
    shutdown(a);
    mock.join().unwrap();
    assert_eq!(r.metadata.usage().current, Amount::default());
}

#[test]
fn peer_failure_becomes_failed_read_without_publishing_or_caching_bytes() {
    use std::io::Read as _;
    for failure in 0..4 {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = if failure == 0 {
            drop(listener);
            None
        } else {
            Some(thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(peer::TIMEOUT)).unwrap();
                let mut request = [0; 56];
                socket.read_exact(&mut request).unwrap();
                let mut response = [9; 24 + BLOCK_SIZE];
                response[..24].copy_from_slice(&request[..24]);
                response[..4].copy_from_slice(&((24 + BLOCK_SIZE) as u32).to_le_bytes());
                let length = match failure {
                    1 => 24 + 100,       // valid frame length, truncated payload
                    2 => response.len(), // complete frame, wrong content hash
                    3 => {
                        response[10] = 1;
                        response[..4].copy_from_slice(&24u32.to_le_bytes());
                        24
                    }
                    _ => unreachable!(),
                };
                socket.write_all(&response[..length]).unwrap();
            }))
        };
        let r = resources(peer::Config {
            peer_address: Some(address),
            peer_cache: true,
            ..Default::default()
        });
        let mut host = seeded(root.path(), r.clone(), false);
        let mut local = attach(&mut host, 2);
        submit(&mut local, 0, 0);
        receive(&mut local, None);
        assert_eq!(host.shared.cache.status().resident_bytes, 0);
        assert!(local.shared.health.lock().unwrap().failure.is_some());
        assert_eq!(host.report()["peer"]["counters"]["errors"], 1);
        assert_eq!(host.report()["peer"]["counters"]["verified"], 0);
        assert_eq!(local.status.durable, 1);
        assert_eq!(local.status.published, 1);
        drop(local);
        shutdown(host);
        if let Some(mock) = mock {
            mock.join().unwrap();
        }
        assert_eq!(r.metadata.usage().current, Amount::default());
    }
}

#[test]
fn force_remote_preserves_newer_wal_reads_and_flush() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let r = resources(peer::Config {
        peer_address: Some(address),
        peer_force_remote: true,
        ..Default::default()
    });
    let mut host = seeded(root.path(), r.clone(), true);
    let mut local = attach(&mut host, 2);
    // Pause compaction after it captures durable WAL input, before publication.
    let (entered, wait) = mpsc::channel();
    let (resume, resumed) = mpsc::channel();
    host.shared.control.lock().unwrap().compaction = Some(Pause {
        entered,
        resume: resumed,
    });
    write(&mut local, 0, 0, &[99; BLOCK_SIZE]);
    wait.recv_timeout(Duration::from_secs(5)).unwrap();
    read(&mut local, 1, &[99; BLOCK_SIZE]);
    let permit = local.prepare(Kind::Control).unwrap().unwrap();
    local.enqueue(2, Operation::Flush, permit).unwrap();
    let completion = completed(&mut local);
    assert_eq!(completion.id, 2);
    assert_eq!(host.report()["peer"]["counters"]["gets"], 0);
    assert_eq!(local.status.durable, 2);
    drop(completion);
    resume.send(()).unwrap();
    drained(&mut local, 2);
    drop(local);
    shutdown(host);
    assert_eq!(r.metadata.usage().current, Amount::default());
}

fn counter(host: &Host, key: &str) -> u64 {
    host.report()["peer"]["counters"][key].as_u64().unwrap()
}
/// Σ `peer.images[*].verified_bytes` = 4096 × `peer.counters.verified`: every
/// transport verification is credited to exactly one image, whichever
/// scheduler issued the GET, whether readers coalesced and whoever won a race.
fn assert_verified_bytes_sum(report: &serde_json::Value) {
    let peer = &report["peer"];
    let sum: u64 = peer["images"]
        .as_object()
        .unwrap()
        .values()
        .map(|image| image["verified_bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(
        sum,
        BLOCK_SIZE as u64 * peer["counters"]["verified"].as_u64().unwrap(),
        "{peer}"
    );
}
fn wait_counter(host: &Host, key: &str, value: u64) {
    let until = Instant::now() + Duration::from_secs(5);
    while counter(host, key) != value {
        assert!(
            Instant::now() < until,
            "{key} stayed {} not {value}: {}",
            counter(host, key),
            host.report()
        );
        thread::sleep(Duration::from_millis(1));
    }
}
fn raced(address: SocketAddr, race_us: u32) -> Arc<Resources> {
    resources(peer::Config {
        peer_address: Some(address),
        peer_force_remote: true,
        peer_race_us: Some(race_us),
        peer_race_local: true,
        ..Default::default()
    })
}
/// Mock peer: every GET gets a complete, well-framed reply whose payload
/// does not match the requested hash, after `delay`.
fn corrupting_peer(delay: Duration, requests: usize) -> (SocketAddr, JoinHandle<()>) {
    use std::io::Read as _;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mock = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(peer::TIMEOUT)).unwrap();
        for _ in 0..requests {
            let mut request = [0; 56];
            socket.read_exact(&mut request).unwrap();
            let mut response = [9; 24 + BLOCK_SIZE];
            response[..24].copy_from_slice(&request[..24]);
            response[..4].copy_from_slice(&((24 + BLOCK_SIZE) as u32).to_le_bytes());
            thread::sleep(delay);
            socket.write_all(&response).unwrap();
        }
    });
    (address, mock)
}

#[test]
fn force_remote_race_local_wins_against_a_delayed_peer_and_discards_its_reply() {
    let root_b = tempfile::tempdir().unwrap();
    let root_a = tempfile::tempdir().unwrap();
    // B's own GET service answers every request 5 ms late.
    let rb = resources_serving_after(
        peer::Config {
            peer_listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        },
        5_000,
    );
    let b = seeded(root_b.path(), rb.clone(), true);
    let ra = raced(b.peer_address().unwrap(), 0);
    let mut a = seeded(root_a.path(), ra.clone(), true);
    let mut local = attach(&mut a, 2);
    let started = Instant::now();
    for id in 0..8 {
        get(&mut local, id, id as usize);
    }
    // The counters prove the local copy won every race; wall time only
    // guards against a hang (eight 5 ms replies would otherwise be 40 ms,
    // but a loaded test host can stall either side).
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(counter(&a, "races_started"), 8);
    assert_eq!(counter(&a, "race_won_local"), 8);
    assert_eq!(counter(&a, "race_won_first"), 0);
    assert_eq!(counter(&a, "failed_reads"), 0);
    // Every late reply still verifies, is discarded, and its frame is waste.
    wait_counter(&a, "race_discarded", 8);
    assert_eq!(counter(&a, "verified"), 8);
    assert_eq!(
        counter(&a, "race_wasted_wire_bytes"),
        8 * (24 + BLOCK_SIZE) as u64
    );
    assert_eq!(a.shared.cache.status().resident_bytes, 0);
    let report = a.report();
    // A local win is a completed read without wire bytes; the discarded late
    // reply is still credited, once, to the image that issued the GET.
    assert_eq!(
        report["peer"]["images"][format!("{:02x}", 2).repeat(16)],
        serde_json::json!({"completed_reads": 8, "failed_reads": 0, "chunks": 8,
            "verified_bytes": 8 * BLOCK_SIZE})
    );
    assert_verified_bytes_sum(&report);
    let records = report["peer"]["recent"].as_array().unwrap();
    assert_eq!(records.len(), 8);
    for record in records {
        assert_eq!(record["race_winner"], peer::metrics::WINNER_LOCAL);
        assert_eq!(record["race_budget_us"], 0);
        assert_eq!(record["success"], true);
    }
    assert!(local.shared.health.lock().unwrap().failure.is_none());
    drop(local);
    shutdown(a);
    shutdown(b);
    for r in [ra, rb] {
        assert_eq!(r.metadata.usage().current, Amount::default());
    }
}

#[test]
fn race_completes_from_the_local_copy_when_the_peer_reply_fails_verification() {
    let root = tempfile::tempdir().unwrap();
    let (address, mock) = corrupting_peer(Duration::ZERO, 1);
    let r = raced(address, 0);
    let mut host = seeded(root.path(), r.clone(), true);
    let mut local = attach(&mut host, 2);
    get(&mut local, 0, 3);
    let report = host.report();
    assert_eq!(counter(&host, "races_started"), 1, "{report}");
    assert_eq!(counter(&host, "race_won_local"), 1, "{report}");
    // The corrupt reply is verified and discarded when its transport
    // finishes, which may be after the local win completed the read (the
    // local copy can win before the transport has even connected).
    wait_counter(&host, "race_discarded", 1);
    assert_eq!(counter(&host, "errors"), 1, "{report}");
    assert_eq!(counter(&host, "verified"), 0, "{report}");
    assert_eq!(
        counter(&host, "race_wasted_wire_bytes"),
        (24 + BLOCK_SIZE) as u64
    );
    assert_eq!(counter(&host, "failed_reads"), 0);
    assert!(local.shared.health.lock().unwrap().failure.is_none());
    drop(local);
    shutdown(host);
    mock.join().unwrap();
    assert_eq!(r.metadata.usage().current, Amount::default());
}

/// The peer fails before the budget: with a connection refused outright, and
/// with a peer that reads the GET and closes. Both start the local challenger
/// at once, so the read completes long before the 200 ms budget; without a
/// local copy the same failures stay closed.
#[test]
fn primary_failure_before_the_budget_starts_the_local_challenger_at_once() {
    use std::io::Read as _;
    for (local_copy, eof) in [(true, false), (true, true), (false, false), (false, true)] {
        let root = tempfile::tempdir().unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let mock = if eof {
            Some(thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket.set_read_timeout(Some(peer::TIMEOUT)).unwrap();
                let mut request = [0; 56];
                socket.read_exact(&mut request).unwrap();
                drop(socket);
            }))
        } else {
            drop(listener);
            None
        };
        let r = raced(address, 200_000);
        let mut host = seeded(root.path(), r.clone(), local_copy);
        let mut local = attach(&mut host, 2);
        let started = Instant::now();
        submit(&mut local, 0, 5);
        receive(&mut local, local_copy.then_some(5));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(150),
            "{local_copy} {eof} {elapsed:?}"
        );
        assert_eq!(counter(&host, "errors"), 1, "{local_copy} {eof}");
        assert_eq!(counter(&host, "races_started"), u64::from(local_copy));
        assert_eq!(counter(&host, "race_won_local"), u64::from(local_copy));
        assert_eq!(counter(&host, "failed_reads"), u64::from(!local_copy));
        assert_eq!(
            local.shared.health.lock().unwrap().failure.is_some(),
            !local_copy
        );
        drop(local);
        shutdown(host);
        if let Some(mock) = mock {
            mock.join().unwrap();
        }
        assert_eq!(r.metadata.usage().current, Amount::default());
    }
}

#[test]
fn race_fails_closed_when_both_sources_fail_verification() {
    use std::os::unix::fs::FileExt;
    let root = tempfile::tempdir().unwrap();
    let (address, mock) = corrupting_peer(Duration::ZERO, 1);
    let r = raced(address, 0);
    let mut host = seeded(root.path(), r.clone(), true);
    // Corrupt the local chunk payload on disk behind the verified reader.
    let hash = Chunk::new(&data(3)).unwrap().hash();
    let plan = host.shared.reader.plan(hash).unwrap().unwrap();
    let mut page = AlignedBuffer::new(BLOCK_SIZE);
    let mut payload = AlignedBuffer::new(BLOCK_SIZE);
    plan.load_with_scratch(page.as_mut_slice(), payload.as_mut_slice())
        .unwrap();
    let payload = plan.payload(page.as_slice()).unwrap();
    let path = fs::read_link(format!(
        "/proc/self/fd/{}",
        std::os::fd::AsRawFd::as_raw_fd(payload.file())
    ))
    .unwrap();
    let file = fs::OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&[0xff; BLOCK_SIZE], payload.offset())
        .unwrap();
    file.sync_all().unwrap();
    drop((file, payload, plan));
    let mut local = attach(&mut host, 2);
    submit(&mut local, 0, 3);
    receive(&mut local, None);
    assert_eq!(counter(&host, "errors"), 1);
    assert_eq!(counter(&host, "races_started"), 1);
    assert_eq!(counter(&host, "race_won_local"), 0);
    assert_eq!(counter(&host, "race_won_first"), 0);
    assert_eq!(counter(&host, "failed_reads"), 1);
    assert!(local.shared.health.lock().unwrap().failure.is_some());
    assert_eq!(host.shared.cache.status().resident_bytes, 0);
    drop(local);
    shutdown(host);
    mock.join().unwrap();
    assert_eq!(r.metadata.usage().current, Amount::default());
}

struct Daemon {
    child: Child,
    input: std::process::ChildStdin,
    output: mpsc::Receiver<serde_json::Value>,
    reader: Option<JoinHandle<()>>,
}
impl Daemon {
    fn start(peer: Option<SocketAddr>) -> Self {
        Self::start_with(peer, &[])
    }
    fn start_with(peer: Option<SocketAddr>, env: &[(&str, &str)]) -> Self {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "local::host::tests::peer::daemon_child",
                "--ignored",
                "--nocapture",
            ])
            .env(
                "CAS_PEER_CHILD",
                peer.map_or_else(|| "server".into(), |a| a.to_string()),
            )
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command.spawn().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (send, output) = mpsc::sync_channel(4);
        let reader = thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                if let Some(json) = line.strip_prefix("PEER ") {
                    let value = serde_json::from_str(json).expect("daemon JSON reply");
                    if send.send(value).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            input: child.stdin.take().unwrap(),
            output,
            reader: Some(reader),
            child,
        }
    }
    fn reply(&mut self) -> serde_json::Value {
        self.output
            .recv_timeout(Duration::from_secs(30))
            .expect("daemon reply deadline")
    }

    fn command(&mut self, command: &str) -> serde_json::Value {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        self.reply()
    }
}
impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
fn reply(value: serde_json::Value) {
    println!("PEER {value}");
    std::io::stdout().flush().unwrap();
}

#[test]
#[ignore = "subprocess entry point for two_daemon_loopback; not a standalone fixture"]
fn daemon_child() {
    let role = std::env::var("CAS_PEER_CHILD").expect("parent harness sets role");
    let config = if role == "server" {
        peer::Config {
            peer_listen: Some("127.0.0.1:0".parse().unwrap()),
            ..Default::default()
        }
    } else {
        peer::Config {
            peer_address: Some(role.parse().unwrap()),
            peer_force_remote: true,
            ..Default::default()
        }
    };
    let mut config = config;
    config.peer_inline_us = std::env::var("CAS_PEER_INLINE_US")
        .ok()
        .map(|v| v.parse().unwrap())
        .unwrap_or(0);
    let mut reply_delay_us = 0;
    if role != "server" {
        config.peer_race_us = std::env::var("CAS_PEER_RACE_US")
            .ok()
            .map(|v| v.parse().unwrap());
        config.peer_race_local = std::env::var_os("CAS_PEER_RACE_LOCAL").is_some();
    } else if let Some(delay) = std::env::var("CAS_PEER_SERVER_DELAY_US")
        .ok()
        .map(|v| v.parse().unwrap())
    {
        reply_delay_us = delay;
    }
    config.validate().unwrap();
    let race_local = config.peer_race_local;
    let root = tempfile::tempdir().unwrap();
    let r = resources_serving_after(config, reply_delay_us);
    let mut host = seeded(root.path(), r.clone(), true);
    let mut local = attach(&mut host, 2);
    reply(serde_json::json!({"ready":true,"address":host.peer_address()}));
    for line in std::io::stdin().lock().lines() {
        match line.unwrap().as_str() {
            "measure" => {
                for id in 700..700 + profile_reads() {
                    get(&mut local, id, id as usize % BLOCKS);
                }
                for id in 0..100 {
                    get(&mut local, id, id as usize % BLOCKS);
                }
                for id in 100..700 {
                    get(&mut local, id, id as usize % BLOCKS);
                }
                let mut report = host.report();
                report["local"] = local.report();
                reply(report);
            }
            "pipeline" => {
                for id in 700..704 {
                    submit(&mut local, id, id as usize % BLOCKS);
                }
                for _ in 700..704 {
                    let mut completion = local.receive(true).unwrap().unwrap();
                    completion.result.unwrap();
                    if let Some(t) = completion.permit.as_mut().and_then(|p| p.remote.as_mut()) {
                        t.finish(true);
                    }
                    let CompletionData::Read(bytes) = completion.data else {
                        panic!("read");
                    };
                    assert_eq!(bytes.as_slice(), &data(completion.id as usize % BLOCKS));
                }
                reply(host.report());
            }
            "fail" => {
                // Without a second source the read fails closed. Racing the
                // local copy completes it: the refused connection fails the
                // primary, which starts the local challenger at once.
                submit(&mut local, 704, 0);
                let mut completion = local.receive(true).unwrap().unwrap();
                if let Some(trace) = completion.permit.as_mut().and_then(|p| p.remote.as_mut()) {
                    trace.finish(completion.result.is_ok());
                }
                let ok = completion.result.is_ok();
                let CompletionData::Read(bytes) = completion.data else {
                    panic!("read data");
                };
                if ok {
                    assert!(race_local);
                    assert_eq!(bytes.as_slice(), &data(0));
                } else {
                    assert!(bytes.as_slice().iter().all(|&b| b == 0));
                }
                let mut report = host.report();
                report["fail_read_ok"] = serde_json::Value::Bool(ok);
                reply(report);
            }
            "report" => reply(host.report()),
            "stop" => break,
            other => panic!("unknown harness command {other}"),
        }
    }
    drop(local);
    shutdown(host);
    assert_eq!(r.metadata.usage().current, Amount::default());
    reply(serde_json::json!({"stopped":true}));
}

fn profile_reads() -> u64 {
    let count = std::env::var("CAS_PEER_PROFILE_READS")
        .ok()
        .map(|s| s.parse().unwrap())
        .unwrap_or(0);
    assert!(count <= 1_000_000);
    count
}

#[test]
fn two_daemon_loopback_verifies_data_measures_phases_and_fails_on_peer_loss() {
    // CAS_PEER_RACE_US / CAS_PEER_RACE_LOCAL reach the reader child; the
    // measured phases and evidence keep their meaning with the race armed.
    let race_local = std::env::var_os("CAS_PEER_RACE_LOCAL").is_some();
    let mut b = Daemon::start(None);
    let ready = b.reply();
    let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    let mut a = Daemon::start(Some(address));
    assert_eq!(a.reply()["ready"], true);
    let report = a.command("measure");
    assert_eq!(
        report["peer"]["counters"]["verified"],
        700 + profile_reads()
    );
    assert_eq!(report["cache"]["resident_bytes"], 0);
    if std::env::var_os("CAS_FAST_SETTLED_READS").is_some() {
        // Every measured read was settled. The first lookups of each
        // manifest page need a page load and are declined to the reactor;
        // everything after that completed on the frontend.
        let fast = &report["local"]["fast"];
        let remote = fast["remote_reads"].as_u64().unwrap();
        let page_loads = fast["declined_io"].as_u64().unwrap();
        assert_eq!(remote + page_loads, 700 + profile_reads(), "{fast}");
        assert!(page_loads <= 4, "{fast}");
        assert_eq!(fast["declined_unsettled"], 0, "{fast}");
    }
    let records: Vec<_> = report["peer"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["id"].as_u64().unwrap() < 700)
        .collect();
    assert_eq!(records.len(), 700);
    for r in records {
        let time = |v: &serde_json::Value| v.as_u64().unwrap();
        assert!(time(&r["received_ns"]) <= time(&r["phases"]["send_ns"]));
        assert!(time(&r["phases"]["send_ns"]) <= time(&r["phases"]["first_byte_ns"]));
        assert!(time(&r["phases"]["first_byte_ns"]) <= time(&r["phases"]["verified_ns"]));
        assert!(time(&r["phases"]["verified_ns"]) <= time(&r["completed_ns"]));
        assert_eq!(r["success"], true);
    }
    // With the local copy racing, a loser's reply is verified only when it
    // arrives; wait for every started race to settle before counting.
    let settle = |a: &mut Daemon| {
        let until = Instant::now() + Duration::from_secs(10);
        loop {
            let r = a.command("report");
            let c = &r["peer"]["counters"];
            if c["race_discarded"] == c["races_started"] {
                break;
            }
            assert!(Instant::now() < until, "{c}");
            thread::sleep(Duration::from_millis(5));
        }
    };
    a.command("pipeline");
    if race_local {
        settle(&mut a);
    }
    let pipeline = a.command("report");
    assert_eq!(
        pipeline["peer"]["counters"]["verified"],
        704 + profile_reads()
    );
    assert_verified_bytes_sum(&pipeline);
    let server = b.command("report");
    assert_eq!(server["peer"]["counters"]["server_store"], BLOCKS);
    assert_eq!(
        server["peer"]["counters"]["server_memory"],
        704 - BLOCKS + profile_reads() as usize
    );
    b.command("stop");
    assert!(b.child.wait().unwrap().success());
    let failure = a.command("fail");
    if race_local {
        settle(&mut a);
    }
    let after_loss = a.command("report");
    let c = &after_loss["peer"]["counters"];
    assert_eq!(c["errors"], 1, "{c}");
    if race_local {
        // Peer loss fails the primary, which starts the local challenger at
        // once: the read completes locally. Every started race has one winner
        // and one discard.
        assert_eq!(failure["fail_read_ok"], true, "{c}");
        assert_eq!(c["failed_reads"], 0, "{c}");
        let wins = c["race_won_local"].as_u64().unwrap() + c["race_won_first"].as_u64().unwrap();
        let started = c["races_started"].as_u64().unwrap();
        assert_eq!(wins, started, "{c}");
        assert_eq!(c["race_discarded"], started, "{c}");
    } else {
        assert_eq!(failure["fail_read_ok"], false);
        assert_eq!(c["failed_reads"], 1, "{c}");
    }
    a.command("stop");
    assert!(a.child.wait().unwrap().success());
    if let Some(output) = std::env::var_os("CAS_PEER_EVIDENCE") {
        fs::create_dir_all(&output).unwrap();
        for (name, value) in [
            ("reader", report),
            ("pipeline", pipeline),
            ("server", server),
            ("failure", failure),
        ] {
            fs::write(
                Path::new(&output).join(format!("{name}.json")),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
        }
    }
}

/// Two processes: B delays every GET reply by 1 ms (serially, so a longer
/// delay would back 700 requests up past the 2 s GET timeout), A races its
/// local copy after 100 µs. A local win costs well under a millisecond, so
/// the local copy wins every read on an idle host; the release build asserts
/// at least 95 % local wins. The debug build under the parallel suite has
/// seen the local worker descheduled past the 1 ms reply for half the
/// reads, so it asserts only that local wins occur. Both assert exact
/// accounting for every read: wins sum to starts, every start has one
/// discard, wasted wire bytes are exactly the discarded replies, every reply
/// verified, and no discarded reply reaches a guest buffer (each read checks
/// its bytes).
#[test]
fn two_daemon_loopback_race_wins_locally_against_a_delayed_server() {
    let mut b = Daemon::start_with(None, &[("CAS_PEER_SERVER_DELAY_US", "1000")]);
    let ready = b.reply();
    let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
    let mut a = Daemon::start_with(
        Some(address),
        &[("CAS_PEER_RACE_US", "100"), ("CAS_PEER_RACE_LOCAL", "1")],
    );
    assert_eq!(a.reply()["ready"], true);
    let started = Instant::now();
    let report = a.command("measure");
    let measured = started.elapsed();
    let c = &report["peer"]["counters"];
    let won_local = c["race_won_local"].as_u64().unwrap();
    let won_first = c["race_won_first"].as_u64().unwrap();
    assert_eq!(c["races_started"], 700, "{c}");
    assert_eq!(won_local + won_first, 700, "{c}");
    // The local-win rate is a timing claim: the release loopback run makes
    // it (and is recorded in docs/validation.md); the debug build under the
    // parallel suite only shows that local wins occur, with exact accounting.
    let floor = if cfg!(debug_assertions) { 1 } else { 665 };
    assert!(won_local >= floor, "fewer than {floor} local wins: {c}");
    assert_eq!(c["race_won_second"], 0, "{c}");
    assert_eq!(c["failed_reads"], 0, "{c}");
    assert_eq!(report["cache"]["resident_bytes"], 0);
    // The counters above carry the claim; wall time is only a hang guard.
    assert!(measured < Duration::from_secs(10), "{measured:?}");
    let records: Vec<_> = report["peer"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["id"].as_u64().unwrap() < 700)
        .collect();
    assert_eq!(records.len(), 700);
    let mut local_records = 0;
    for r in records {
        assert!(
            r["race_winner"] == peer::metrics::WINNER_LOCAL
                || r["race_winner"] == peer::metrics::WINNER_FIRST,
            "{r}"
        );
        local_records += u64::from(r["race_winner"] == peer::metrics::WINNER_LOCAL);
        assert_eq!(r["race_budget_us"], 100);
        assert_eq!(r["success"], true);
    }
    assert_eq!(local_records, won_local);
    let until = Instant::now() + Duration::from_secs(10);
    let late = loop {
        let r = a.command("report");
        if r["peer"]["counters"]["race_discarded"] == 700 {
            break r;
        }
        assert!(Instant::now() < until, "{}", r["peer"]["counters"]);
        thread::sleep(Duration::from_millis(10));
    };
    let c = &late["peer"]["counters"];
    assert_eq!(c["verified"], 700, "{c}");
    assert_eq!(c["errors"], 0, "{c}");
    assert_verified_bytes_sum(&late);
    // Each local win discards one whole late reply frame; a primary win
    // discards the local load, which has no wire bytes.
    assert_eq!(
        c["race_wasted_wire_bytes"],
        won_local * (24 + BLOCK_SIZE) as u64,
        "{c}"
    );
    b.command("stop");
    assert!(b.child.wait().unwrap().success());
    a.command("stop");
    assert!(a.child.wait().unwrap().success());
    if let Some(output) = std::env::var_os("CAS_PEER_EVIDENCE") {
        let output = Path::new(&output).join("race");
        fs::create_dir_all(&output).unwrap();
        for (name, value) in [("reader", report), ("late", late)] {
            fs::write(
                output.join(format!("{name}.json")),
                serde_json::to_vec_pretty(&value).unwrap(),
            )
            .unwrap();
        }
    }
}

/// Two frontend-inline reads of the same chunk coalesce through the shared
/// fetch registry: the first leads one GET, the second waits on it and is
/// woken through the frontend's own eventfd. One GET for the chunk, both
/// reads verified. The mock owner answers by hash after 20 ms so the second
/// read deterministically joins while the first GET is in flight.
#[test]
fn fast_reads_of_one_chunk_share_a_single_get_through_the_registry() {
    use std::io::Read as _;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mock = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(peer::TIMEOUT)).unwrap();
        let mut served = 0;
        loop {
            let mut request = [0; 56];
            if socket.read_exact(&mut request).is_err() {
                break;
            }
            let block = (0..BLOCKS)
                .find(|b| Chunk::new(&data(*b)).unwrap().hash() == request[24..56])
                .expect("known chunk");
            let mut response = [0; 24 + BLOCK_SIZE];
            response[..24].copy_from_slice(&request[..24]);
            response[..4].copy_from_slice(&((24 + BLOCK_SIZE) as u32).to_le_bytes());
            response[24..].copy_from_slice(&data(block));
            thread::sleep(Duration::from_millis(20));
            socket.write_all(&response).unwrap();
            served += 1;
        }
        served
    });
    let root = tempfile::tempdir().unwrap();
    let r = resources(peer::Config {
        peer_address: Some(address),
        peer_force_remote: true,
        fast_settled_reads: true,
        ..Default::default()
    });
    let mut host = seeded(root.path(), r.clone(), false);
    let mut local = attach(&mut host, 2);
    // Warm the manifest page through the reactor, then coalesce two reads.
    get(&mut local, 0, 1);
    submit(&mut local, 1, 7);
    submit(&mut local, 2, 7);
    receive(&mut local, Some(7));
    receive(&mut local, Some(7));
    assert_eq!(counter(&host, "gets"), 2);
    assert_eq!(counter(&host, "verified"), 2);
    // Both inline GETs were issued by image 2 and credited there.
    assert_eq!(
        host.report()["peer"]["images"][format!("{:02x}", 2).repeat(16)]["verified_bytes"],
        2 * BLOCK_SIZE
    );
    assert_verified_bytes_sum(&host.report());
    let fast = local.report()["fast"].clone();
    assert_eq!(fast["remote_reads"], 2, "{fast}");
    assert_eq!(fast["shared_waits"], 1, "{fast}");
    assert_eq!(host.report()["fetches"]["counters"]["joined"], 1);
    drop(local);
    shutdown(host);
    assert_eq!(mock.join().unwrap(), 2);
    assert_eq!(r.metadata.usage().current, Amount::default());
}
