use super::{metrics::Metrics, wire::*, *};
use cas_core::{
    BLOCK_SIZE,
    budget::{Amount, Budget},
};
use std::{
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn memory() -> Arc<Budget> {
    Budget::new(Amount {
        bytes: 32 * 1024 * 1024,
        requests: 0,
    })
}
/// The reactor's contract: poll the primary eventfd, finish, and re-arm while
/// no source has verified yet.
fn wait(ticket: &Ticket) -> (metrics::Phases, std::io::Result<()>, [u8; BLOCK_SIZE]) {
    let mut bytes = [0xa5; BLOCK_SIZE];
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        let mut fd = libc::pollfd {
            fd: ticket.notification(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: fd is initialized and the ticket retains the live eventfd.
        assert_eq!(unsafe { libc::poll(&mut fd, 1, 4000) }, 1);
        match ticket.finish(&mut bytes) {
            (phases, Ok(true)) => return (phases, Ok(()), bytes),
            (phases, Err(error)) => return (phases, Err(error), bytes),
            (_, Ok(false)) => assert!(Instant::now() < until, "no source completed"),
        }
    }
}
fn counter(metrics: &Metrics, key: &str) -> u64 {
    metrics.report()["counters"][key].as_u64().unwrap()
}
fn wait_counter(metrics: &Metrics, key: &str, value: u64) {
    let until = Instant::now() + Duration::from_secs(4);
    while counter(metrics, key) != value {
        assert!(
            Instant::now() < until,
            "{key} stayed {} not {value}",
            counter(metrics, key)
        );
        thread::sleep(Duration::from_millis(1));
    }
}
/// A mock owner serving `requests` GETs on one connection, each delayed and
/// optionally corrupted so it fails BLAKE3 verification at the reader.
fn member(
    value: u8,
    delay: Duration,
    corrupt: bool,
    requests: usize,
) -> (std::net::SocketAddr, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_nodelay(true).unwrap();
        for _ in 0..requests {
            let h = request(&mut socket);
            let mut frame = response(h, &[value; BLOCK_SIZE]);
            if corrupt {
                frame[HEADER] ^= 1;
            }
            thread::sleep(delay);
            socket.write_all(&frame).unwrap();
        }
    });
    (address, server)
}
/// A chunk value whose rendezvous owners in a three-member k=2 fleet are
/// both remote members of node 0.
fn raced_value() -> u8 {
    let placement = crate::fleet::Placement::with_members(0, 2, 3).unwrap();
    (0..=255u8)
        .find(|v| {
            let hash = *blake3::hash(&[*v; BLOCK_SIZE]).as_bytes();
            placement.owns(&hash, 1) && placement.owns(&hash, 2)
        })
        .expect("a chunk owned by both remote members")
}
/// Node 0 of a three-member k=2 fleet with two mock remote owners.
fn raced_config(race_us: u32, first: std::net::SocketAddr, second: std::net::SocketAddr) -> Config {
    let listen: std::net::SocketAddr = "127.0.0.1:17380".parse().unwrap();
    let bulk: Vec<std::net::SocketAddr> = (0..3)
        .map(|i| format!("127.0.0.1:{}", 17_384 + i).parse().unwrap())
        .collect();
    Config {
        peer_listen: Some(listen),
        peer_get_member: vec![listen, first, second],
        peer_race_us: Some(race_us),
        fleet: crate::fleet::Config {
            peer_node: Some(0),
            peer_k: 2,
            peer_bulk_listen: Some(bulk[0]),
            peer_bulk_member: bulk,
            ..crate::fleet::Config::default()
        },
        ..Config::default()
    }
}
fn shutdown(router: Router) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        match router.shutdown() {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < until, "peer workers did not stop");
                thread::sleep(Duration::from_millis(2));
            }
            Err(e) => panic!("{e}"),
        }
    }
    drop(router);
}

#[test]
fn race_completes_from_the_second_owner_and_discards_the_late_first_reply() {
    let value = raced_value();
    let (first, first_server) = member(value, Duration::from_millis(40), false, 1);
    let (second, second_server) = member(value, Duration::ZERO, false, 1);
    let config = raced_config(0, first, second);
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let router = Router::new(&config, &budget, metrics.clone(), None).unwrap();
    // Issued through an image's view: the challenger inherits the image.
    metrics.bind(0, [9; 16]);
    let view = router.for_image(0);
    let hash = *blake3::hash(&[value; BLOCK_SIZE]).as_bytes();
    let ticket = view.fetch(hash).unwrap();
    view.race(&ticket, hash, None);
    let started = Instant::now();
    let (phases, result, bytes) = wait(&ticket);
    result.unwrap();
    assert_eq!(bytes, [value; BLOCK_SIZE]);
    assert!(started.elapsed() < Duration::from_millis(40));
    assert_eq!(phases.race_winner, metrics::WINNER_SECOND);
    assert_eq!(phases.race_budget_us, 0);
    assert_eq!(counter(&metrics, "races_started"), 1);
    assert_eq!(counter(&metrics, "race_won_second"), 1);
    assert_eq!(counter(&metrics, "race_won_first"), 0);
    assert_eq!(
        metrics.image([9; 16]).verified_bytes,
        BLOCK_SIZE as u64,
        "the winning challenger is credited to the issuing image"
    );
    // The loser's bytes never reach a reader; its slot is recycled only after
    // its transport finishes, and the whole frame counts as wasted.
    drop(ticket);
    wait_counter(&metrics, "race_discarded", 1);
    assert_eq!(counter(&metrics, "race_wasted_wire_bytes"), FRAME as u64);
    assert_eq!(counter(&metrics, "verified"), 2);
    // The late loser verified too, once, to the same image: the sum invariant.
    assert_eq!(metrics.image([9; 16]).verified_bytes, 2 * BLOCK_SIZE as u64);
    drop(view);
    shutdown(router);
    first_server.join().unwrap();
    second_server.join().unwrap();
    drop(metrics);
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn race_masks_a_failed_source_only_with_a_verified_reply() {
    // First owner answers at once with a corrupt payload; the second owner
    // verifies later. Then both corrupt: the read fails with an IO error.
    for both_fail in [false, true] {
        let value = raced_value();
        let (first, first_server) = member(value, Duration::ZERO, true, 1);
        let (second, second_server) = member(value, Duration::from_millis(10), both_fail, 1);
        let config = raced_config(0, first, second);
        let budget = memory();
        let metrics = Metrics::new(&budget, 1).unwrap();
        let router = Router::new(&config, &budget, metrics.clone(), None).unwrap();
        let hash = *blake3::hash(&[value; BLOCK_SIZE]).as_bytes();
        let ticket = router.fetch(hash).unwrap();
        router.race(&ticket, hash, None);
        let (phases, result, bytes) = wait(&ticket);
        if both_fail {
            assert!(result.is_err());
            assert_eq!(bytes, [0xa5; BLOCK_SIZE]);
            assert_eq!(counter(&metrics, "errors"), 2);
            assert_eq!(counter(&metrics, "race_won_second"), 0);
        } else {
            result.unwrap();
            assert_eq!(bytes, [value; BLOCK_SIZE]);
            assert_eq!(phases.race_winner, metrics::WINNER_SECOND);
            assert_eq!(counter(&metrics, "errors"), 1);
            assert_eq!(counter(&metrics, "verified"), 1);
            assert_eq!(counter(&metrics, "race_won_second"), 1);
        }
        assert_eq!(counter(&metrics, "races_started"), 1);
        drop(ticket);
        shutdown(router);
        first_server.join().unwrap();
        second_server.join().unwrap();
        drop(metrics);
        assert_eq!(budget.usage().current, Amount::default());
    }
}

#[test]
fn race_budget_starts_the_challenger_only_after_the_primary_is_late() {
    let value = raced_value();
    // Two requests: the first owner is prompt for one, late for the other.
    let (first, first_server) = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(TIMEOUT)).unwrap();
            let h = request(&mut socket);
            socket
                .write_all(&response(h, &[value; BLOCK_SIZE]))
                .unwrap();
            let h = request(&mut socket);
            thread::sleep(Duration::from_millis(500));
            socket
                .write_all(&response(h, &[value; BLOCK_SIZE]))
                .unwrap();
        });
        (address, server)
    };
    let (second, second_server) = member(value, Duration::ZERO, false, 1);
    // A 100 ms budget: a prompt loopback reply stays far inside it even on a
    // loaded parallel test host, and the 500 ms late reply far outside.
    let config = raced_config(100_000, first, second);
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let router = Router::new(&config, &budget, metrics.clone(), None).unwrap();
    let hash = *blake3::hash(&[value; BLOCK_SIZE]).as_bytes();
    let ticket = router.fetch(hash).unwrap();
    router.race(&ticket, hash, None);
    let (phases, result, _) = wait(&ticket);
    result.unwrap();
    assert_eq!(phases.race_winner, metrics::WINNER_NONE);
    drop(ticket);
    thread::sleep(Duration::from_millis(120));
    assert_eq!(counter(&metrics, "races_started"), 0);
    let ticket = router.fetch(hash).unwrap();
    router.race(&ticket, hash, None);
    let started = Instant::now();
    let (phases, result, bytes) = wait(&ticket);
    result.unwrap();
    assert_eq!(bytes, [value; BLOCK_SIZE]);
    assert!(started.elapsed() >= Duration::from_millis(100));
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(phases.race_winner, metrics::WINNER_SECOND);
    assert_eq!(phases.race_budget_us, 100_000);
    assert_eq!(counter(&metrics, "races_started"), 1);
    drop(ticket);
    wait_counter(&metrics, "race_discarded", 1);
    shutdown(router);
    first_server.join().unwrap();
    second_server.join().unwrap();
    drop(metrics);
    assert_eq!(budget.usage().current, Amount::default());
}
/// A slot is reused while the racer still holds the previous occupant's
/// deadline. When that stale deadline fires it must not cancel the new
/// request's armed race: the second request still races and the second
/// owner still wins against the slow first owner.
#[test]
fn stale_racer_deadline_does_not_cancel_a_reused_slots_race() {
    let value = raced_value();
    // First owner: prompt for request 1, 200 ms late for request 2.
    let (first, first_server) = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(TIMEOUT)).unwrap();
            let h = request(&mut socket);
            socket
                .write_all(&response(h, &[value; BLOCK_SIZE]))
                .unwrap();
            let h = request(&mut socket);
            thread::sleep(Duration::from_millis(500));
            socket
                .write_all(&response(h, &[value; BLOCK_SIZE]))
                .unwrap();
        });
        (address, server)
    };
    let (second, second_server) = member(value, Duration::ZERO, false, 1);
    let config = raced_config(100_000, first, second);
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let router = Router::new(&config, &budget, metrics.clone(), None).unwrap();
    let hash = *blake3::hash(&[value; BLOCK_SIZE]).as_bytes();
    let ticket = router.fetch(hash).unwrap();
    router.race(&ticket, hash, None);
    let (phases, result, _) = wait(&ticket);
    result.unwrap();
    assert_eq!(phases.race_winner, metrics::WINNER_NONE);
    // The slot is free again while its 100 ms deadline is still armed; the
    // next fetch reuses it (the free list is a stack) with a new generation.
    drop(ticket);
    let ticket = router.fetch(hash).unwrap();
    router.race(&ticket, hash, None);
    let started = Instant::now();
    let (phases, result, bytes) = wait(&ticket);
    result.unwrap();
    assert_eq!(bytes, [value; BLOCK_SIZE]);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "the reused slot's race was cancelled: {:?}",
        started.elapsed()
    );
    assert_eq!(phases.race_winner, metrics::WINNER_SECOND);
    assert_eq!(phases.race_budget_us, 100_000);
    assert_eq!(counter(&metrics, "races_started"), 1);
    assert_eq!(counter(&metrics, "race_won_second"), 1);
    assert_eq!(counter(&metrics, "race_dropped"), 0);
    drop(ticket);
    wait_counter(&metrics, "race_discarded", 1);
    shutdown(router);
    first_server.join().unwrap();
    second_server.join().unwrap();
    drop(metrics);
    assert_eq!(budget.usage().current, Amount::default());
}
fn request(socket: &mut TcpStream) -> Header {
    let mut bytes = [0; HEADER + 32];
    socket.read_exact(&mut bytes).unwrap();
    Header::decode(&bytes, false).unwrap()
}
fn response(h: Header, bytes: &[u8; BLOCK_SIZE]) -> [u8; FRAME] {
    let mut frame = [0; FRAME];
    Header { bytes: FRAME, ..h }.encode(&mut frame);
    frame[HEADER..].copy_from_slice(bytes);
    frame
}

#[test]
fn pipelined_gets_handle_fragmentation_without_growing_transport_memory() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        socket.set_nodelay(true).unwrap();
        let requests: Vec<_> = (0..4).map(|_| request(&mut socket)).collect();
        // No reply before all four requests proves the client pipelines.
        for h in requests {
            let frame = response(h, &[7; BLOCK_SIZE]);
            for part in frame.chunks(1023) {
                socket.write_all(part).unwrap();
            }
        }
    });
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let client = Client::new(address, &budget, metrics.clone()).unwrap();
    let before = budget.usage();
    let hash = *blake3::hash(&[7; BLOCK_SIZE]).as_bytes();
    let tickets: Vec<_> = (0..4).map(|_| client.fetch(hash).unwrap()).collect();
    for ticket in &tickets {
        let (p, result, bytes) = wait(ticket);
        result.unwrap();
        assert_eq!(bytes, [7; BLOCK_SIZE]);
        assert!(p.send_ns <= p.first_byte_ns && p.first_byte_ns <= p.verified_ns);
    }
    assert_eq!(budget.usage().admitted, before.admitted);
    assert_eq!(metrics.report()["counters"]["max_inflight"], 4);
    drop((tickets, client, metrics));
    peer.join().unwrap();
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn peer_down_short_reads_bad_hash_identity_and_status_never_publish_bytes() {
    for failure in 0..7 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let peer = if failure == 0 {
            drop(listener);
            None
        } else {
            Some(thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                let mut h = request(&mut socket);
                let mut frame = response(h, &[7; BLOCK_SIZE]);
                match failure {
                    1 => {
                        socket.write_all(&frame[..9]).unwrap();
                    }
                    2 => {
                        socket.write_all(&frame[..FRAME - 1]).unwrap();
                    }
                    3 => {
                        frame[HEADER] ^= 1;
                        socket.write_all(&frame).unwrap();
                    }
                    4 => {
                        h.id += 1;
                        frame = response(h, &[7; BLOCK_SIZE]);
                        socket.write_all(&frame).unwrap();
                    }
                    5 => {
                        Header {
                            bytes: HEADER,
                            status: MISSING,
                            ..h
                        }
                        .encode(&mut frame);
                        socket.write_all(&frame[..HEADER]).unwrap();
                    }
                    6 => {
                        thread::sleep(TIMEOUT + Duration::from_millis(150));
                    }
                    _ => unreachable!(),
                }
            }))
        };
        let budget = memory();
        let metrics = Metrics::new(&budget, 1).unwrap();
        let client = Client::new(address, &budget, metrics.clone()).unwrap();
        let ticket = client
            .fetch(*blake3::hash(&[7; BLOCK_SIZE]).as_bytes())
            .unwrap();
        let started = Instant::now();
        let (_, result, bytes) = wait(&ticket);
        assert!(result.is_err(), "case {failure}");
        assert_eq!(bytes, [0xa5; BLOCK_SIZE]);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(metrics.report()["counters"]["errors"], 1);
        drop((ticket, client, metrics));
        if let Some(peer) = peer {
            peer.join().unwrap();
        }
        assert_eq!(budget.usage().current, Amount::default());
    }
}

#[test]
fn cancellation_retains_transport_slot_until_reply() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let peer = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let first = request(&mut socket);
        let second = request(&mut socket);
        assert_ne!(first.id, second.id);
        socket
            .write_all(&response(first, &[7; BLOCK_SIZE]))
            .unwrap();
        socket
            .write_all(&response(second, &[7; BLOCK_SIZE]))
            .unwrap();
    });
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let client = Client::new(address, &budget, metrics.clone()).unwrap();
    let hash = *blake3::hash(&[7; BLOCK_SIZE]).as_bytes();
    drop(client.fetch(hash).unwrap());
    let ticket = client.fetch(hash).unwrap();
    wait(&ticket).1.unwrap();
    drop((ticket, client, metrics));
    peer.join().unwrap();
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn has_checks_bounds_before_connecting() {
    let address = "127.0.0.1:1".parse().unwrap();
    assert!(has(address, &[], &mut []).is_err());
    assert!(has(address, &[[0; 32]; MAX_HASHES + 1], &mut [0; 17]).is_err());
    assert!(has(address, &[[0; 32]], &mut []).is_err());
}

#[test]
fn failed_connection_is_replaced_for_the_next_request() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut first, _) = listener.accept().unwrap();
        request(&mut first);
        drop(first);
        let (mut second, _) = listener.accept().unwrap();
        let h = request(&mut second);
        second.write_all(&response(h, &[7; BLOCK_SIZE])).unwrap();
    });
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let client = Client::new(address, &budget, metrics.clone()).unwrap();
    let hash = *blake3::hash(&[7; BLOCK_SIZE]).as_bytes();
    let first = client.fetch(hash).unwrap();
    assert!(wait(&first).1.is_err());
    drop(first);
    let second = client.fetch(hash).unwrap();
    wait(&second).1.unwrap();
    drop(second);
    assert_eq!(metrics.report()["counters"]["connections"], 2);
    drop((client, metrics));
    server.join().unwrap();
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn coalesced_missing_and_success_frames_keep_their_own_payloads() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(TIMEOUT)).unwrap();
        let first = request(&mut socket);
        let second = request(&mut socket);
        let mut joined = vec![0; HEADER + FRAME];
        Header {
            status: MISSING,
            bytes: HEADER,
            ..first
        }
        .encode(&mut joined);
        joined[HEADER..].copy_from_slice(&response(second, &[7; BLOCK_SIZE]));
        socket.write_all(&joined).unwrap();
    });
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let client = Client::with_inline(address, &budget, metrics.clone(), 20).unwrap();
    let hash = *blake3::hash(&[7; BLOCK_SIZE]).as_bytes();
    let first = client.fetch(hash).unwrap();
    let second = client.fetch(hash).unwrap();
    assert_eq!(
        wait(&first).1.unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
    wait(&second).1.unwrap();
    drop((first, second, client, metrics));
    server.join().unwrap();
    assert_eq!(budget.usage().current, Amount::default());
}

#[test]
fn inline_get_has_a_bounded_poll_and_retains_slow_reply_ownership() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (release, released) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let first = request(&mut socket);
        socket
            .write_all(&response(first, &[7; BLOCK_SIZE]))
            .unwrap();
        let second = request(&mut socket);
        released.recv_timeout(TIMEOUT).unwrap();
        socket
            .write_all(&response(second, &[7; BLOCK_SIZE]))
            .unwrap();
    });
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let client = Client::with_inline(address, &budget, metrics.clone(), 20).unwrap();
    let hash = *blake3::hash(&[7; BLOCK_SIZE]).as_bytes();
    let first = client.fetch(hash).unwrap();
    wait(&first).1.unwrap();
    drop(first);
    let second = client.fetch(hash).unwrap();
    // fetch returned before the peer is allowed to reply; no blocking read.
    assert!(!second.ready());
    assert_eq!(metrics.report()["counters"]["inline_attempts"], 1);
    release.send(()).unwrap();
    wait(&second).1.unwrap();
    drop((second, client, metrics));
    server.join().unwrap();
    assert_eq!(budget.usage().current, Amount::default());
}

fn four_member_config() -> Config {
    let get: Vec<std::net::SocketAddr> = (0..4)
        .map(|i| format!("127.0.0.1:{}", 17_390 + i).parse().unwrap())
        .collect();
    let bulk: Vec<std::net::SocketAddr> = (0..4)
        .map(|i| format!("127.0.0.1:{}", 17_394 + i).parse().unwrap())
        .collect();
    Config {
        peer_listen: Some(get[0]),
        peer_get_member: get,
        fleet: crate::fleet::Config {
            peer_node: Some(0),
            peer_k: 1,
            peer_bulk_listen: Some(bulk[0]),
            peer_bulk_member: bulk,
            ..crate::fleet::Config::default()
        },
        ..Config::default()
    }
}

fn nofile() -> libc::rlimit {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes one initialized rlimit through a valid pointer.
    let status = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) };
    assert_eq!(status, 0);
    limit
}

#[test]
#[ignore = "subprocess entry point for the descriptor-limit regression"]
fn descriptor_limit_child() {
    let limit = libc::rlimit {
        rlim_cur: std::env::var("CAS_PEER_NOFILE_SOFT")
            .unwrap()
            .parse()
            .unwrap(),
        rlim_max: std::env::var("CAS_PEER_NOFILE_HARD")
            .unwrap()
            .parse()
            .unwrap(),
    };
    // SAFETY: setrlimit reads one initialized rlimit; lowering this process's
    // own limits needs no privilege.
    assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limit) }, 0);
    let budget = memory();
    let metrics = Metrics::new(&budget, 1).unwrap();
    let result = Router::new(&four_member_config(), &budget, metrics, None);
    let limit = nofile();
    let required = 3 * client::CLIENT_DESCRIPTORS + 1024;
    match std::env::var("CAS_PEER_NOFILE_EXPECT").unwrap().as_str() {
        "raised" => {
            let router = result.unwrap();
            assert!(limit.rlim_cur >= required, "{}", limit.rlim_cur);
            assert!(limit.rlim_max == libc::RLIM_INFINITY || limit.rlim_cur == limit.rlim_max);
            // Shutdown is nonblocking until each client worker has exited.
            let until = Instant::now() + Duration::from_secs(10);
            loop {
                match router.shutdown() {
                    Ok(()) => break,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until, "peer workers did not stop");
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        }
        "rejected" => {
            let error = result.err().expect("hard limit below requirement");
            assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("hard RLIMIT_NOFILE"), "{error}");
            assert_eq!(limit.rlim_cur, 1024);
        }
        other => panic!("unknown expectation {other}"),
    }
}

/// The child lowers its own descriptor limit from the environment before
/// anything in it opens files. No `pre_exec` hook: with one, `Command` must
/// `fork()` this large multi-threaded test binary, and between fork and exec
/// the child holds a copy of every open description, including the
/// flock-held roots and stores of tests running in parallel (locks live on
/// the description, so the parent's close does not release them). That
/// window is milliseconds and made concurrent tests fail with `WouldBlock`.
/// Without the hook `Command` uses `posix_spawn` (vfork), whose window is
/// microseconds and whose caller is suspended until the exec.
fn descriptor_child(soft: libc::rlim_t, hard: libc::rlim_t, expect: &str) -> bool {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "peer::tests::descriptor_limit_child",
            "--ignored",
            "--nocapture",
        ])
        .env("CAS_PEER_NOFILE_EXPECT", expect)
        .env("CAS_PEER_NOFILE_SOFT", soft.to_string())
        .env("CAS_PEER_NOFILE_HARD", hard.to_string());
    command.status().unwrap().success()
}

/// Services commonly start with a 1024-descriptor soft limit, below even one
/// GET client's preallocated eventfds. A four-member router must raise the
/// soft limit itself, and fail clearly when the hard limit is too low.
#[test]
fn peer_clients_raise_the_default_descriptor_soft_limit() {
    let hard = nofile().rlim_max;
    let required = 3 * client::CLIENT_DESCRIPTORS + 1024;
    assert!(
        hard == libc::RLIM_INFINITY || hard >= required,
        "test host hard RLIMIT_NOFILE {hard} cannot run a four-member fleet"
    );
    assert!(descriptor_child(1024, hard, "raised"));
    assert!(descriptor_child(1024, 1024, "rejected"));
}
