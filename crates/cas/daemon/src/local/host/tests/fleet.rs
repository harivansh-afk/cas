use super::*;
use crate::fleet::{self, Flow, Metrics};
use std::net::{SocketAddr, TcpListener};
mod process;
fn addresses_for(count: usize) -> Vec<SocketAddr> {
    // Reserve below Linux's ephemeral client range. Closing port-0 listeners
    // before starting children allowed outgoing connections to steal their ports.
    static NEXT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(17480);
    let mut listeners = Vec::new();
    while listeners.len() < count {
        let port = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(port < 32768, "test port range exhausted");
        if let Ok(listener) = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)) {
            listeners.push(listener);
        }
    }
    listeners.iter().map(|l| l.local_addr().unwrap()).collect()
}
fn addresses() -> [SocketAddr; 4] {
    addresses_for(4).try_into().unwrap()
}
fn resources(node: u8, k: u8, addresses: [SocketAddr; 4]) -> Arc<Resources> {
    member_resources(node, k, &addresses)
}
fn member_resources(node: u8, k: u8, addresses: &[SocketAddr]) -> Arc<Resources> {
    let n = node as usize;
    let count = addresses.len() / 2;
    // CAS_PEER_RACE_US arms the raced fetch for every member; k=2 fleets with
    // two remote owners then race the second owner after that budget.
    Arc::new(Resources {
        peer: crate::peer::Config {
            peer_listen: Some(addresses[n]),
            peer_get_member: addresses[..count].to_vec(),
            peer_race_us: std::env::var("CAS_PEER_RACE_US")
                .ok()
                .map(|v| v.parse().unwrap()),
            fast_settled_reads: std::env::var_os("CAS_FAST_SETTLED_READS").is_some(),
            fleet: fleet::Config {
                peer_node: Some(node),
                peer_k: k,
                peer_bulk_listen: Some(addresses[count + n]),
                peer_bulk_member: addresses[count..].to_vec(),
                ..crate::fleet::Config::default()
            },
            ..crate::peer::Config::default()
        },
        ..Resources::default()
    })
}
fn shutdown(mut host: Host) {
    let until = Instant::now() + IO_DEADLINE;
    loop {
        match host.shutdown() {
            Ok(()) => break,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until);
                thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("{e}"),
        }
    }
}
#[test]
fn compaction_ships_durable_chunks_and_retries_without_duplicate_payloads() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let addresses = addresses();
    let mut a = catalog_host(roots[0].path(), 2, resources(0, 2, addresses));
    let b = catalog_host(roots[1].path(), 3, resources(1, 2, addresses));
    let mut guest = attach(&mut a, 2);
    let data = [0x63; BLOCK_SIZE];
    let hash = *blake3::hash(&data).as_bytes();
    write(&mut guest, 1, 0, &data);
    let permit = guest.prepare(Kind::Control).unwrap().unwrap();
    guest.enqueue(2, Operation::Flush, permit).unwrap();
    completed(&mut guest);
    let until = Instant::now() + Duration::from_secs(10);
    while b.shared.reader.plan(hash).unwrap().is_none() {
        assert!(Instant::now() < until, "a={} b={}", a.report(), b.report());
        thread::sleep(Duration::from_millis(10));
    }
    let mut got = AlignedBuffer::new(BLOCK_SIZE);
    b.shared
        .reader
        .plan(hash)
        .unwrap()
        .unwrap()
        .load(got.as_mut_slice())
        .unwrap();
    assert_eq!(got.as_slice(), data);
    let metrics = Arc::new(Metrics::default());
    let mut client = fleet::Client::connect(addresses[3], Flow::Sync, metrics.clone()).unwrap();
    let mut frame = vec![0; fleet::chunks::SEGMENT];
    let mut page = AlignedBuffer::new(BLOCK_SIZE);
    let mut payload = AlignedBuffer::new(BLOCK_SIZE);
    assert_eq!(
        fleet::chunks::ship(
            &mut client,
            &a.shared.reader,
            &[hash],
            &mut frame,
            page.as_mut_slice(),
            payload.as_mut_slice()
        )
        .unwrap(),
        0
    );
    assert_eq!(metrics.report()["sync"]["sent"], 56);
    assert_eq!(metrics.report()["sync"]["received"], 25);
    assert_eq!(b.store_status().chunks, 1);
    drop(client);
    guest.stop().unwrap();
    drop(guest);
    shutdown(a);
    shutdown(b);
    // A receiver's ACK was followed by shutdown; independent cold inspection
    // must still recover the advertised bytes, with no manifest on B naming it.
    let r = resources(1, 2, addresses);
    let tickets = contended(|| Tickets::open(roots[1].path(), r.metadata.clone())).unwrap();
    let store = contended(|| {
        Store::inspect(
            tickets.clone(),
            Config {
                segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
                ..STORE
            },
            r.metadata.clone(),
            r.read_memory(),
        )?
        .recover()
    })
    .unwrap();
    let mut bytes = AlignedBuffer::new(BLOCK_SIZE);
    store
        .plan(hash)
        .unwrap()
        .unwrap()
        .load(bytes.as_mut_slice())
        .unwrap();
    assert_eq!(bytes.as_slice(), data);
}

#[test]
fn partitioned_sweep_keeps_peer_roots_releases_surplus_and_reads_remote() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let addresses = addresses();
    let mut a = catalog_host(roots[0].path(), 2, resources(0, 1, addresses));
    let b = catalog_host(roots[1].path(), 3, resources(1, 1, addresses));
    let mut guest = attach(&mut a, 2);
    let bytes: Vec<u8> = (1..=64)
        .flat_map(|n| std::iter::repeat_n(n as u8, BLOCK_SIZE))
        .collect();
    write(&mut guest, 1, 0, &bytes);
    let permit = guest.prepare(Kind::Control).unwrap().unwrap();
    guest.enqueue(2, Operation::Flush, permit).unwrap();
    completed(&mut guest);
    drained(&mut guest, 1);
    let placement = crate::fleet::Placement::new(0, 1).unwrap();
    let hashes: Vec<_> = bytes
        .as_chunks::<BLOCK_SIZE>()
        .0
        .iter()
        .map(|b| *blake3::hash(b).as_bytes())
        .collect();
    let remote = hashes.iter().filter(|h| placement.remote(h)).count();
    assert!(remote > 16 && remote < 48);
    let until = Instant::now() + Duration::from_secs(10);
    while b.store_status().chunks < remote {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(a.store_status().chunks, 64);
    let metrics = Arc::new(Metrics::default());
    fleet::collect([addresses[2], addresses[3]], metrics).unwrap();
    assert_eq!(a.store_status().chunks, 64 - remote);
    assert_eq!(b.store_status().chunks, remote);
    read(&mut guest, 3, &bytes);
    assert_eq!(a.report()["peer"]["counters"]["verified"], remote);
    guest.stop().unwrap();
    drop(guest);
    shutdown(a);
    shutdown(b);
    let state = contended(|| {
        crate::fleet::state::State::inspect(
            roots[0].path(),
            placement,
            &Resources::default().metadata,
        )
    })
    .unwrap();
    for hash in &hashes {
        assert_eq!(state.remote(hash), placement.remote(hash));
    }
}

fn catalog_host(root: &Path, image: u8, resources: Arc<Resources>) -> Host {
    use cas_core::catalog::{Entry, Initial, Kind};
    let tickets = Tickets::open(root, resources.metadata.clone()).unwrap();
    let config = Config {
        segment_bytes: 2 * MAX_REQUEST_BYTES as u64,
        ..STORE
    };
    let store = Store::create(
        tickets.clone(),
        config,
        resources.metadata.clone(),
        resources.read_memory(),
    )
    .unwrap();
    fs::create_dir(root.join("images")).unwrap();
    fs::create_dir(root.join("snapshots")).unwrap();
    let directory = path(root, image);
    fs::create_dir(&directory).unwrap();
    let manifest =
        Manifest::create(&directory, identity(image), resources.metadata.clone()).unwrap();
    let log = Log::create_shared(
        tickets.clone(),
        append::Config {
            store: config.store,
            image: [image; 16],
            image_bytes: IMAGE_BYTES,
            segment_bytes: config.segment_bytes,
        },
        append::Limits::default(),
        resources.metadata.clone(),
        manifest.view().unwrap(),
    )
    .unwrap();
    let catalog = Initial::prepare(
        config.store,
        [Entry {
            id: [image; 16],
            kind: Kind::Image {
                image_bytes: IMAGE_BYTES,
            },
        }]
        .into_iter(),
        resources.metadata.clone(),
    )
    .unwrap()
    .publish(tickets)
    .unwrap();
    Host::build_owned(
        resources,
        store,
        Roots {
            images: vec![(log, manifest)],
            snapshots: Vec::new(),
        },
        1024 * MAX_REQUEST_BYTES as u64,
        None,
        Context {
            catalog: Some(catalog),
            ..Context::default()
        },
    )
    .unwrap()
}
#[test]
fn provision_installs_only_manifest_and_migration_fences_source_before_live_attach() {
    let roots = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    let addresses = addresses();
    let mut a = catalog_host(roots[0].path(), 2, resources(0, 2, addresses));
    let mut b = catalog_host(roots[1].path(), 3, resources(1, 2, addresses));
    let mut source = attach(&mut a, 2);
    let bytes: Vec<u8> = (1..=64)
        .flat_map(|n| std::iter::repeat_n(n as u8, BLOCK_SIZE))
        .collect();
    write(&mut source, 1, 0, &bytes);
    let permit = source.prepare(Kind::Control).unwrap().unwrap();
    source.enqueue(2, Operation::Flush, permit).unwrap();
    completed(&mut source);
    drained(&mut source, 1);
    let until = Instant::now() + Duration::from_secs(10);
    while b.store_status().chunks < 64 {
        assert!(Instant::now() < until);
        thread::sleep(Duration::from_millis(10));
    }
    let report = fleet::transfer(
        addresses[2],
        addresses[3],
        [2; 16],
        [4; 16],
        fleet::Mode::Provision,
        Arc::new(Metrics::default()),
    )
    .unwrap();
    assert_eq!(report.tail_bytes, 24);
    assert!(report.manifest_bytes < bytes.len() as u64 / 4);
    let mut clone = attach(&mut b, 4);
    read(&mut clone, 1, &bytes);
    assert!(
        b.report()["fleet"]["provision"]["received"]
            .as_u64()
            .unwrap()
            < bytes.len() as u64 / 4
    );
    a.shared.control.lock().unwrap().defer_compaction = true;
    let tail = [0x91; BLOCK_SIZE];
    write(&mut source, 3, 0, &tail);
    let permit = source.prepare(Kind::Control).unwrap().unwrap();
    source.enqueue(4, Operation::Flush, permit).unwrap();
    completed(&mut source);
    let report = fleet::transfer(
        addresses[2],
        addresses[3],
        [2; 16],
        [2; 16],
        fleet::Mode::Migration,
        Arc::new(Metrics::default()),
    )
    .unwrap();
    assert!(report.tail_bytes >= 24 + 32 + BLOCK_SIZE as u64);
    assert!(source.prepare(Kind::Write(BLOCK_SIZE)).is_err());
    let mut destination = attach(&mut b, 2);
    let mut expected = bytes.clone();
    expected[..BLOCK_SIZE].copy_from_slice(&tail);
    read(&mut destination, 2, &expected);
    read(&mut clone, 3, &bytes);
    assert!(!a.shared.fleet.as_ref().unwrap().writable([2; 16]).unwrap());
    assert!(b.shared.fleet.as_ref().unwrap().writable([2; 16]).unwrap());
    let _ = source.stop();
    drop(source);
    clone.stop().unwrap();
    drop(clone);
    destination.stop().unwrap();
    drop(destination);
    shutdown(a);
    shutdown(b);
}
