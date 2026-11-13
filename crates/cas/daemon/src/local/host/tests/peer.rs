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
    // The failed read attempted one more remote chunk but verified no payload.
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

struct Daemon {
    child: Child,
    input: std::process::ChildStdin,
    output: mpsc::Receiver<serde_json::Value>,
    reader: Option<JoinHandle<()>>,
}
impl Daemon {
    fn start(peer: Option<SocketAddr>) -> Self {
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
                submit(&mut local, 704, 0);
                receive(&mut local, None);
                reply(host.report());
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
    let pipeline = a.command("pipeline");
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
    assert_eq!(failure["peer"]["counters"]["errors"], 1);
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
