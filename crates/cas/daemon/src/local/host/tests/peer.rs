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
    let tickets = Tickets::open(root, Arc::clone(&resources.metadata)).unwrap();
    let mut store = Store::create(
        Arc::clone(&tickets),
        STORE,
        Arc::clone(&resources.metadata),
        resources.read_memory(),
    )
    .unwrap();
    fs::create_dir(root.join("images")).unwrap();
    let directory = path(root, 2);
    fs::create_dir(&directory).unwrap();
    let mut manifest =
        Manifest::create(&directory, identity(2), Arc::clone(&resources.metadata)).unwrap();
    let mut log = Log::create_shared(
        Arc::clone(&tickets),
        append::Config {
            store: STORE.store,
            image: [2; 16],
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
                for (block, bytes) in bytes.as_chunks_mut::<BLOCK_SIZE>().0.iter_mut().enumerate() {
                    bytes.copy_from_slice(&data(block));
                }
                Ok(())
            },
        )
        .unwrap();
    log.append(builder).unwrap();
    log.flush().unwrap();
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
    drop(remote_store);
    Host::new(resources, store, vec![(log, manifest)]).unwrap()
}
fn resources(config: peer::Config) -> Arc<Resources> {
    Arc::new(Resources {
        peer: config,
        cache_bytes: BLOCKS * BLOCK_SIZE,
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
    let rb = resources(peer::Config {
        peer_listen: Some("127.0.0.1:0".parse().unwrap()),
        ..Default::default()
    });
    let b = seeded(root_b.path(), rb.clone(), true);
    let ra = raced(b.peer_address().unwrap(), 0);
    let mut a = seeded(root_a.path(), ra.clone(), true);
    let mut local = attach(&mut a, 2);
    peer::TEST_REPLY_DELAY_US.store(5_000, std::sync::atomic::Ordering::Relaxed);
    let started = Instant::now();
    for id in 0..8 {
        get(&mut local, id, id as usize);
    }
    peer::TEST_REPLY_DELAY_US.store(0, std::sync::atomic::Ordering::Relaxed);
    // Eight 5 ms server delays would take 40 ms; the local copy won each read.
    assert!(
        started.elapsed() < Duration::from_millis(40),
        "{:?}",
        started.elapsed()
    );
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
    assert_eq!(counter(&host, "errors"), 1);
    assert_eq!(counter(&host, "verified"), 0);
    assert_eq!(counter(&host, "races_started"), 1);
    assert_eq!(counter(&host, "race_won_local"), 1);
    assert_eq!(counter(&host, "race_discarded"), 1);
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
    if role != "server" {
        config.peer_race_us = std::env::var("CAS_PEER_RACE_US")
            .ok()
            .map(|v| v.parse().unwrap());
        config.peer_race_local = std::env::var_os("CAS_PEER_RACE_LOCAL").is_some();
    } else if let Some(delay) = std::env::var("CAS_PEER_SERVER_DELAY_US")
        .ok()
        .map(|v| v.parse().unwrap())
    {
        peer::TEST_REPLY_DELAY_US.store(delay, std::sync::atomic::Ordering::Relaxed);
    }
    config.validate().unwrap();
    let race_local = config.peer_race_local;
    let root = tempfile::tempdir().unwrap();
    let r = resources(config);
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
                reply(host.report());
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
                // local copy completes it after peer loss only if the
                // challenger started before the refused connection failed
                // the primary; a failure with no other source stays closed.
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
        // A primary that completes before its challenger attaches records no
        // winner, so wins can trail starts; every start has one discard.
        let read_ok = failure["fail_read_ok"].as_bool().unwrap();
        assert_eq!(c["failed_reads"], u64::from(!read_ok), "{c}");
        let wins = c["race_won_local"].as_u64().unwrap() + c["race_won_first"].as_u64().unwrap();
        let started = c["races_started"].as_u64().unwrap();
        assert!(wins <= started, "{c}");
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

/// Two processes: B delays every GET reply by 1 ms, A races its local copy
/// after 100 µs. Every measured read must be won locally, every late reply
/// must be verified, discarded and counted as wasted wire bytes, and the
/// discarded replies never reach a guest buffer (each read checks its bytes).
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
    assert_eq!(c["races_started"], 700);
    assert_eq!(c["race_won_local"], 700);
    assert_eq!(c["race_won_first"], 0);
    assert_eq!(c["failed_reads"], 0);
    assert_eq!(report["cache"]["resident_bytes"], 0);
    // 700 serialized 1 ms replies would need at least 700 ms.
    assert!(measured < Duration::from_millis(700), "{measured:?}");
    let records: Vec<_> = report["peer"]["recent"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["id"].as_u64().unwrap() < 700)
        .collect();
    assert_eq!(records.len(), 700);
    for r in records {
        assert_eq!(r["race_winner"], peer::metrics::WINNER_LOCAL);
        assert_eq!(r["race_budget_us"], 100);
        assert_eq!(r["success"], true);
    }
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
    assert_eq!(c["verified"], 700);
    assert_eq!(c["errors"], 0);
    assert_eq!(c["race_wasted_wire_bytes"], 700 * (24 + BLOCK_SIZE) as u64);
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
