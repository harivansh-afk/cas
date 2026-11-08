//! Real process death, independent root locks, cold recovery and TCP only.
use super::*;
use serde_json::{Value, json};
use std::{
    io::{BufRead, Write},
    process::{Child, Command, Stdio},
};

struct Daemon {
    child: Child,
    input: std::process::ChildStdin,
    replies: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}
impl Daemon {
    fn start(root: &Path, node: u8, k: u8, addresses: [SocketAddr; 4], pause: &str) -> Self {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "local::host::tests::fleet::process::daemon_child",
                "--ignored",
                "--nocapture",
            ])
            .env("CAS_FLEET_ROOT", root)
            .env("CAS_FLEET_NODE", node.to_string())
            .env("CAS_FLEET_K", k.to_string())
            .env(
                "CAS_FLEET_ADDRESSES",
                serde_json::to_string(&addresses).unwrap(),
            )
            .env("CAS_FLEET_PAUSE", pause)
            .env("CAS_FLEET_MARKER", root.join("pause"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(value) = line.strip_prefix("FLEET ")
                    && tx.send(serde_json::from_str(value).unwrap()).is_err()
                {
                    break;
                }
            }
        });
        let mut result = Self {
            input: child.stdin.take().unwrap(),
            child,
            replies,
            reader: Some(reader),
        };
        assert_eq!(result.reply()["ready"], true);
        result
    }
    fn reply(&mut self) -> Value {
        self.replies
            .recv_timeout(Duration::from_secs(60))
            .expect("fleet child response")
    }
    fn command(&mut self, command: Value) -> Value {
        writeln!(self.input, "{command}").unwrap();
        self.input.flush().unwrap();
        self.reply()
    }
    fn kill(&mut self) {
        self.child.kill().unwrap();
        self.child.wait().unwrap();
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
fn reply(value: Value) {
    println!("FLEET {value}");
    std::io::stdout().flush().unwrap();
}
fn cold(root: &Path, resources: Arc<Resources>) -> Host {
    let inspected = super::super::recovery::scan(root, &resources).unwrap();
    inspected
        .require(crate::recovery::Prefixes::Cold)
        .unwrap()
        .recover_test(1024 * MAX_REQUEST_BYTES as u64)
        .unwrap()
}
fn data() -> Vec<u8> {
    (1..=64)
        .flat_map(|n| std::iter::repeat_n(n as u8, BLOCK_SIZE))
        .collect()
}
#[test]
#[ignore = "subprocess entry point used by the fleet integration tests"]
fn daemon_child() {
    let root = std::path::PathBuf::from(std::env::var_os("CAS_FLEET_ROOT").unwrap());
    let node: u8 = std::env::var("CAS_FLEET_NODE").unwrap().parse().unwrap();
    let k = std::env::var("CAS_FLEET_K").unwrap().parse().unwrap();
    let addresses = serde_json::from_str(&std::env::var("CAS_FLEET_ADDRESSES").unwrap()).unwrap();
    let resources = resources(node, k, addresses);
    let mut host = if root.join("images").exists() {
        cold(&root, resources)
    } else {
        catalog_host(&root, node + 2, resources)
    };
    let mut guests = std::collections::BTreeMap::<u8, Local>::new();
    let mut id = 1;
    reply(json!({"ready":true}));
    for line in std::io::stdin().lock().lines() {
        let command: Value = serde_json::from_str(&line.unwrap()).unwrap();
        let image = command["image"].as_u64().unwrap_or(2) as u8;
        let result = match command["op"].as_str().unwrap() {
            "attach" => {
                let event = EventFd::new(EFD_CLOEXEC | EFD_NONBLOCK).unwrap();
                let result = host.local([image; 16], &event);
                let ok = result.is_ok();
                if let Ok(local) = result {
                    guests.insert(image, local);
                }
                json!({"ok":ok})
            }
            "seed" => {
                let local = guests.get_mut(&image).unwrap();
                write(local, id, 0, &data());
                id += 1;
                let permit = local.prepare(Kind::Control).unwrap().unwrap();
                local.enqueue(id, Operation::Flush, permit).unwrap();
                id += 1;
                completed(local);
                drained(local, 1);
                json!({"ok":true})
            }
            "tail" => {
                host.shared.control.lock().unwrap().defer_compaction = true;
                let local = guests.get_mut(&image).unwrap();
                let value = command["value"].as_u64().unwrap_or(0x91) as u8;
                write(
                    local,
                    id,
                    command["offset"].as_u64().unwrap_or(0) as usize,
                    &[value; BLOCK_SIZE],
                );
                id += 1;
                let permit = local.prepare(Kind::Control).unwrap().unwrap();
                local.enqueue(id, Operation::Flush, permit).unwrap();
                id += 1;
                completed(local);
                json!({"ok":true,"durable":local.status.durable})
            }
            "read" => {
                let mut expected = data();
                if let Some(value) = command["value"].as_u64() {
                    expected[..BLOCK_SIZE].fill(value as u8);
                }
                if let Some(value) = command["second"].as_u64() {
                    expected[BLOCK_SIZE..2 * BLOCK_SIZE].fill(value as u8);
                }
                read(guests.get_mut(&image).unwrap(), id, &expected);
                id += 1;
                json!({"ok":true})
            }
            "write_denied" => {
                json!({"ok":guests.get_mut(&image).unwrap().prepare(Kind::Write(BLOCK_SIZE)).is_err()})
            }
            "compact" => {
                host.shared.control.lock().unwrap().defer_compaction = false;
                let local = guests.get_mut(&image).unwrap();
                drained(local, local.status.durable);
                json!({"ok":true})
            }
            "shipping" => {
                let shipper = host.shared.fleet.as_ref().unwrap().shipper.get().unwrap();
                if command["pause"] == true {
                    shipper.pause();
                    let until = Instant::now() + IO_DEADLINE;
                    while !shipper.drained() {
                        assert!(Instant::now() < until);
                        thread::sleep(Duration::from_millis(1));
                    }
                } else {
                    shipper.resume();
                }
                json!({"ok":true})
            }
            "report" => host.report(),
            "sync_paused" => {
                let runtime = host.shared.fleet.as_ref().unwrap();
                let placement = runtime.state.lock().unwrap().placement;
                let mut hashes = host
                    .shared
                    .reader
                    .inventory(host.shared.resources.metadata.clone())
                    .unwrap();
                hashes.retain(|h| placement.remote(h));
                let mut client = fleet::Client::member(
                    addresses[3 - node as usize],
                    Flow::Sync,
                    runtime.metrics.clone(),
                    placement,
                    runtime.store,
                )
                .unwrap();
                let mut frame = vec![0; fleet::chunks::SEGMENT];
                let mut page = AlignedBuffer::new(BLOCK_SIZE);
                let mut payload = AlignedBuffer::new(BLOCK_SIZE);
                let mut count = 0;
                for batch in hashes.chunks(63) {
                    count += fleet::chunks::ship(
                        &mut client,
                        &host.shared.reader,
                        batch,
                        &mut frame,
                        page.as_mut_slice(),
                        payload.as_mut_slice(),
                    )
                    .unwrap();
                }
                json!({"chunks":count})
            }
            "stop" => break,
            other => panic!("unknown command {other}"),
        };
        reply(result);
    }
    drop(guests);
    shutdown(host);
    reply(json!({"stopped":true}));
}
fn wait_chunks(host: &mut Daemon, count: usize) {
    let until = Instant::now() + Duration::from_secs(20);
    loop {
        let report = host.command(json!({"op":"report"}));
        if report["store"]["chunks"].as_u64() == Some(count as u64) {
            return;
        }
        assert!(Instant::now() < until, "{report}");
        thread::sleep(Duration::from_millis(10));
    }
}
fn attached(host: &mut Daemon, image: u8) {
    assert_eq!(
        host.command(json!({"op":"attach","image":image}))["ok"],
        true
    );
}
fn transfer(addresses: [SocketAddr; 4], mode: fleet::Mode) -> io::Result<fleet::ImageReport> {
    fleet::transfer(
        addresses[2],
        addresses[3],
        [2; 16],
        if mode == fleet::Mode::Provision {
            [4; 16]
        } else {
            [2; 16]
        },
        mode,
        Arc::new(Metrics::default()),
    )
}
fn evidence(name: &str, value: &Value) {
    if let Some(path) = std::env::var_os("CAS_FLEET_EVIDENCE") {
        fs::create_dir_all(&path).unwrap();
        fs::write(
            Path::new(&path).join(format!("{name}.json")),
            serde_json::to_vec_pretty(value).unwrap(),
        )
        .unwrap();
    }
}
#[test]
fn two_daemon_provision_migrate_sync_and_sweep_verify_data() {
    for k in [1, 2] {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let addresses = addresses();
        let mut a = Daemon::start(roots[0].path(), 0, k, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, k, addresses, "");
        attached(&mut a, 2);
        a.command(json!({"op":"seed"}));
        let remote = data()
            .as_chunks::<BLOCK_SIZE>()
            .0
            .iter()
            .filter(|d| {
                fleet::Placement::new(0, k)
                    .unwrap()
                    .remote(blake3::hash(*d).as_bytes())
            })
            .count();
        wait_chunks(&mut b, remote);
        let wire_before = [
            a.command(json!({"op":"report"}))["fleet"].clone(),
            b.command(json!({"op":"report"}))["fleet"].clone(),
        ];
        let provision = transfer(addresses, fleet::Mode::Provision).unwrap();
        let wire_provision = [
            a.command(json!({"op":"report"}))["fleet"].clone(),
            b.command(json!({"op":"report"}))["fleet"].clone(),
        ];
        attached(&mut b, 4);
        b.command(json!({"op":"read","image":4}));
        a.command(json!({"op":"tail"}));
        let migration = transfer(addresses, fleet::Mode::Migration).unwrap();
        let wire_migration = [
            a.command(json!({"op":"report"}))["fleet"].clone(),
            b.command(json!({"op":"report"}))["fleet"].clone(),
        ];
        assert_eq!(a.command(json!({"op":"write_denied"}))["ok"], true);
        attached(&mut b, 2);
        b.command(json!({"op":"read","value":0x91}));
        b.command(json!({"op":"read","image":4}));
        // Repeat is idempotent, including a lost activation reply.
        // Restart both before retrying the frozen source, as an old guest is fenced.
        a.kill();
        b.kill();
        drop((a, b));
        let mut a = Daemon::start(roots[0].path(), 0, k, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, k, addresses, "");
        assert_eq!(a.command(json!({"op":"attach"}))["ok"], false);
        attached(&mut b, 2);
        b.command(json!({"op":"read","value":0x91}));
        attached(&mut b, 4);
        b.command(json!({"op":"tail","image":4,"value":0x92}));
        b.command(json!({"op":"compact","image":4}));
        let first = fleet::synchronize(addresses[3], Arc::new(Metrics::default())).unwrap();
        let repeat = fleet::synchronize(addresses[3], Arc::new(Metrics::default())).unwrap();
        assert_eq!(repeat, 0);
        let sweep =
            fleet::collect([addresses[2], addresses[3]], Arc::new(Metrics::default())).unwrap();
        b.command(json!({"op":"read","value":0x91}));
        b.command(json!({"op":"read","image":4,"value":0x92}));
        evidence(
            &format!("flows-k{k}"),
            &json!({"provision":provision,"migration":migration,"wire":{"before":wire_before,"provision":wire_provision,"migration":wire_migration},"sync_chunks":first,"sync_repeat":repeat,"sweep":sweep,"a":a.command(json!({"op":"report"})),"b":b.command(json!({"op":"report"}))}),
        );
        let back = fleet::transfer(
            addresses[3],
            addresses[2],
            [2; 16],
            [2; 16],
            fleet::Mode::Migration,
            Arc::new(Metrics::default()),
        )
        .unwrap();
        assert_eq!(a.command(json!({"op":"attach"}))["ok"], true);
        a.command(json!({"op":"read","value":0x91}));
        assert_eq!(b.command(json!({"op":"write_denied"}))["ok"], true);
        evidence(
            &format!("return-k{k}"),
            &serde_json::to_value(back).unwrap(),
        );
    }
}
#[test]
fn migration_kill_at_each_durable_boundary_never_grants_two_writers_or_loses_tail() {
    for point in [
        "source-fenced",
        "destination-receiving",
        "destination-bytes",
        "destination-durable",
        "destination-catalog",
        "destination-active",
    ] {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let addresses = addresses();
        let mut a = Daemon::start(
            roots[0].path(),
            0,
            2,
            addresses,
            if point == "source-fenced" { point } else { "" },
        );
        let mut b = Daemon::start(
            roots[1].path(),
            1,
            2,
            addresses,
            if point == "source-fenced" { "" } else { point },
        );
        attached(&mut a, 2);
        a.command(json!({"op":"seed"}));
        wait_chunks(&mut b, 64);
        a.command(json!({"op":"tail"}));
        let request = thread::spawn(move || transfer(addresses, fleet::Mode::Migration));
        let marker = roots[usize::from(point != "source-fenced")]
            .path()
            .join("pause");
        let until = Instant::now() + Duration::from_secs(40);
        while !marker.exists() {
            assert!(Instant::now() < until, "missing crash point {point}");
            thread::sleep(Duration::from_millis(5));
        }
        a.kill();
        b.kill();
        drop((a, b));
        assert!(request.join().unwrap().is_err());
        let mut a = Daemon::start(roots[0].path(), 0, 2, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, 2, addresses, "");
        assert_eq!(a.command(json!({"op":"attach"}))["ok"], false, "{point}");
        transfer(addresses, fleet::Mode::Migration)
            .unwrap_or_else(|e| panic!("retry {point}: {e}"));
        attached(&mut b, 2);
        b.command(json!({"op":"read","value":0x91}));
        b.command(json!({"op":"tail","value":0xa2}));
        a.kill();
        b.kill();
        drop((a, b));
        let mut a = Daemon::start(roots[0].path(), 0, 2, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, 2, addresses, "");
        assert_eq!(a.command(json!({"op":"attach"}))["ok"], false);
        attached(&mut b, 2);
        b.command(json!({"op":"read","value":0xa2}));
        eprintln!("migration crash/restart verified: {point}");
    }
}
#[test]
fn put_kill_before_or_after_durability_keeps_source_and_retries() {
    for point in ["put-before", "put-durable"] {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let addresses = addresses();
        let mut a = Daemon::start(roots[0].path(), 0, 2, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, 2, addresses, point);
        attached(&mut a, 2);
        a.command(json!({"op":"seed"}));
        let until = Instant::now() + Duration::from_secs(30);
        while !roots[1].path().join("pause").exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        b.kill();
        drop(b);
        a.command(json!({"op":"read"}));
        a.command(json!({"op":"tail","value":0x94}));
        a.command(json!({"op":"compact"}));
        a.command(json!({"op":"read","value":0x94}));
        let mut b = Daemon::start(roots[1].path(), 1, 2, addresses, "");
        fleet::synchronize(addresses[2], Arc::new(Metrics::default())).unwrap();
        wait_chunks(&mut b, 65);
        transfer(addresses, fleet::Mode::Provision).unwrap();
        attached(&mut b, 4);
        b.command(json!({"op":"read","image":4,"value":0x94}));
        eprintln!("PUT crash/retry verified: {point}");
    }
}

#[test]
fn interrupted_fleet_sweep_keeps_both_hosts_fenced_until_retry() {
    for point in ["gc-frozen", "gc-swept"] {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let addresses = addresses();
        let mut a = Daemon::start(roots[0].path(), 0, 1, addresses, point);
        let mut b = Daemon::start(roots[1].path(), 1, 1, addresses, "");
        attached(&mut a, 2);
        a.command(json!({"op":"seed"}));
        fleet::synchronize(addresses[2], Arc::new(Metrics::default())).unwrap();
        let request = thread::spawn(move || {
            fleet::collect([addresses[2], addresses[3]], Arc::new(Metrics::default()))
        });
        let until = Instant::now() + Duration::from_secs(30);
        while !roots[0].path().join("pause").exists() {
            assert!(Instant::now() < until);
            thread::sleep(Duration::from_millis(5));
        }
        a.kill();
        b.kill();
        drop((a, b));
        assert!(request.join().unwrap().is_err());
        let mut a = Daemon::start(roots[0].path(), 0, 1, addresses, "");
        let _b = Daemon::start(roots[1].path(), 1, 1, addresses, "");
        assert_eq!(a.command(json!({"op":"attach"}))["ok"], false);
        fleet::collect([addresses[2], addresses[3]], Arc::new(Metrics::default())).unwrap();
        attached(&mut a, 2);
        a.command(json!({"op":"read"}));
        eprintln!("GC crash/retry verified: {point}");
    }
}

#[test]
fn two_drifted_clones_send_only_the_other_owners_missing_chunks() {
    for k in [1, 2] {
        let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let addresses = addresses();
        let mut a = Daemon::start(roots[0].path(), 0, k, addresses, "");
        let mut b = Daemon::start(roots[1].path(), 1, k, addresses, "");
        attached(&mut a, 2);
        a.command(json!({"op":"seed"}));
        fleet::synchronize(addresses[2], Arc::new(Metrics::default())).unwrap();
        transfer(addresses, fleet::Mode::Provision).unwrap();
        attached(&mut b, 4);
        for daemon in [&mut a, &mut b] {
            daemon.command(json!({"op":"shipping","pause":true}));
        }
        for (daemon, image, value) in [(&mut a, 2, 0x92), (&mut b, 4, 0x93)] {
            daemon.command(json!({"op":"tail","image":image,"value":0x91}));
            daemon.command(json!({"op":"tail","image":image,"value":value,"offset":BLOCK_SIZE}));
            daemon.command(json!({"op":"compact","image":image}));
        }
        let before = [
            a.command(json!({"op":"report"})),
            b.command(json!({"op":"report"})),
        ];
        let mut counts = [0; 2];
        for (node, daemon, value) in [(0, &mut a, 0x92), (1, &mut b, 0x93)] {
            counts[node as usize] = daemon.command(json!({"op":"sync_paused"}))["chunks"]
                .as_u64()
                .unwrap();
            assert_eq!(
                counts[node as usize],
                u64::from(
                    fleet::Placement::new(node, k)
                        .unwrap()
                        .remote(blake3::hash(&[value; BLOCK_SIZE]).as_bytes())
                )
            );
        }
        let after_first = [
            a.command(json!({"op":"report"})),
            b.command(json!({"op":"report"})),
        ];
        assert_eq!(a.command(json!({"op":"sync_paused"}))["chunks"], 0);
        assert_eq!(b.command(json!({"op":"sync_paused"}))["chunks"], 0);
        a.command(json!({"op":"read","value":0x91,"second":0x92}));
        b.command(json!({"op":"read","image":4,"value":0x91,"second":0x93}));
        evidence(
            &format!("drift-k{k}"),
            &json!({"before":before,"after_first":after_first,"after":[a.command(json!({"op":"report"})),b.command(json!({"op":"report"}))],"unique_chunks_sent":counts}),
        );
    }
}

#[test]
fn cold_recovery_refuses_a_tail_below_the_durable_handoff_record_without_repair() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let addresses = addresses();
    let mut a = Daemon::start(roots[0].path(), 0, 2, addresses, "");
    let mut b = Daemon::start(roots[1].path(), 1, 2, addresses, "");
    attached(&mut a, 2);
    a.command(json!({"op":"seed"}));
    wait_chunks(&mut b, 64);
    a.command(json!({"op":"tail"}));
    transfer(addresses, fleet::Mode::Migration).unwrap();
    a.kill();
    b.kill();
    drop((a, b));
    let staging = path(roots[1].path(), 2).join("staging");
    let segment = fs::read_dir(staging)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("segment-")
        })
        .max()
        .unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(segment)
        .unwrap()
        .set_len(BLOCK_SIZE as u64)
        .unwrap();
    let before = super::super::recovery::files(roots[1].path());
    let result = super::super::recovery::scan(roots[1].path(), &resources(1, 2, addresses))
        .and_then(|i| i.require(crate::recovery::Prefixes::Cold));
    assert!(result.is_err());
    assert_eq!(before, super::super::recovery::files(roots[1].path()));
}
