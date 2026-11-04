//! The reach gates R1 to R4 of `docs/design/iroh-reach.md` (protocol.md 12, SR-35 to SR-40):
//! two loopback hosts and an in-process relay, nothing contacted beyond this machine. R5 is
//! `bench/reach-netns.sh`.
//!
//! "Across networks" is made by restarting both hosts on new ports after they met: their stored
//! LAN addresses then refuse at once, so a dial reaches reach, and with `relay_only` no IP socket
//! exists for it to go direct over: relay only is the default (`ReachConfig::direct` off).
//!
//! Run with `cargo test -p playdrop-core --features iroh-test --test phase12_reach`.

#![cfg(feature = "iroh-test")]

mod common;

use std::any::Any;
use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use common::{file_root, init_logs, pair, start_host_with, temp_root, write_random_file, TestHost};
use iroh::endpoint::{presets, ConnectionError, QuicTransportConfig, VarInt};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, EndpointAddr, RelayMode, RelayUrl, SecretKey};
use playdrop_core::host::{HostEvent, PathOutcome, TransferDirection, TransferStatus};
use playdrop_core::identity::DeviceIdentity;
use playdrop_core::reach::{
    self, is_reach_addr, secret_key_for, wire, ReachConfig, ReachPathKind, ALPN,
};
use playdrop_core::tls;

const T: Duration = Duration::from_secs(45);
const CHUNK: u32 = playdrop_core::constants::DEFAULT_CHUNK_SIZE;

/// The in-process relay; it stops when dropped.
struct Relay {
    url: String,
    _server: Box<dyn Any + Send>,
}

async fn relay() -> Relay {
    let (_map, url, server) = iroh::test_utils::run_relay_server()
        .await
        .expect("relay starts");
    Relay {
        url: url.to_string(),
        _server: Box::new(server),
    }
}

/// The default, relay only: nothing can go direct. Retries fast, tests do not wait 15 s.
fn relay_only(relay: &Relay) -> ReachConfig {
    let mut c = ReachConfig::new(relay.url.clone());
    c.insecure_relay_tls = true;
    c.retry_every = Duration::from_millis(500);
    c
}

async fn start(root: &Path, name: &str, port: u16, reach: Option<ReachConfig>) -> TestHost {
    start_host_with(root, name, port, move |cfg| {
        cfg.reach = reach;
        cfg.path_timing.recheck_every = Duration::from_millis(300);
        cfg.path_timing.same_address_recheck = Duration::from_millis(300);
    })
    .await
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(started.elapsed() < T, "timed out waiting: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_online(h: &mut TestHost) -> std::net::SocketAddr {
    let ev = h
        .events
        .wait_for(T, |e| matches!(e, HostEvent::PeerOnline { .. }))
        .await;
    let HostEvent::PeerOnline { addr, .. } = ev else {
        unreachable!()
    };
    addr
}

/// Two hosts that met on the "LAN" and turned the `remote` grant on, each holding the other's
/// endpoint.
async fn met(root: &Path, r: &Relay) -> (TestHost, TestHost) {
    let mut a = start(root, "alpha", 0, Some(relay_only(r))).await;
    let mut b = start(root, "bravo", 0, Some(relay_only(r))).await;
    pair(&mut a, &mut b).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    a.host.set_remote_grant(&b_id, true).await.unwrap();
    b.host.set_remote_grant(&a_id, true).await.unwrap();
    wait_until("both hold the other's endpoint", || {
        a.host.reach_stored_for_test(&b_id).is_some()
            && b.host.reach_stored_for_test(&a_id).is_some()
    })
    .await;
    (a, b)
}

/// Stops both and starts them on new ports: their stored LAN addresses are dead.
async fn across_networks(root: &Path, r: &Relay, a: TestHost, b: TestHost) -> (TestHost, TestHost) {
    a.host.shutdown().await;
    b.host.shutdown().await;
    drop((a, b));
    let a = start(root, "alpha", 0, Some(relay_only(r))).await;
    let b = start(root, "bravo", 0, Some(relay_only(r))).await;
    (a, b)
}

/// Online over reach, on the connection alpha dialled: bravo never dials.
async fn online_alpha_dialled(root: &Path, r: &Relay) -> (TestHost, TestHost) {
    let (a, b) = met(root, r).await;
    a.host.shutdown().await;
    b.host.shutdown().await;
    drop((a, b));
    let mut quiet = relay_only(r);
    quiet.never_dial = true;
    let mut b = start(root, "bravo", 0, Some(quiet)).await;
    let mut a = start(root, "alpha", 0, Some(relay_only(r))).await;
    assert!(is_reach_addr(wait_online(&mut a).await));
    assert!(is_reach_addr(wait_online(&mut b).await));
    (a, b)
}

async fn online_over_reach(root: &Path, r: &Relay) -> (TestHost, TestHost) {
    let (a, b) = met(root, r).await;
    let (mut a, mut b) = across_networks(root, r, a, b).await;
    let addr_a = wait_online(&mut a).await;
    let addr_b = wait_online(&mut b).await;
    assert!(is_reach_addr(addr_a), "{addr_a}");
    assert!(is_reach_addr(addr_b), "{addr_b}");
    (a, b)
}

async fn complete(h: &mut TestHost, id: &str, dir: TransferDirection) {
    h.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferComplete { id: i, direction, .. }
                if i == id && *direction == dir)
        })
        .await;
}

/// What a client sends first: the HTTP/2 preface and an empty SETTINGS frame.
const H2_START: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\x00\x00\x00\x04\x00\x00\x00\x00\x00";

/// Runs the inner TLS as `cfg` over a reach stream and checks nothing is served: the dialler's
/// half can finish, as in TLS 1.3 the server judges the client certificate after the client's
/// last flight, but no HTTP/2 byte must come back.
async fn assert_not_served(
    cfg: std::sync::Arc<rustls::ClientConfig>,
    spki: Option<[u8; 32]>,
    recv: iroh::endpoint::RecvStream,
    send: iroh::endpoint::SendStream,
    what: &str,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let io = tokio::io::join(recv, send);
    let Ok((mut stream, _)) = tls::connect(cfg, spki, "100::1".parse().unwrap(), io).await else {
        return;
    };
    let _ = stream.write_all(H2_START).await;
    let _ = stream.flush().await;
    let mut buf = [0u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .unwrap_or_else(|_| panic!("{what}: not refused in time"));
    assert!(!matches!(read, Ok(n) if n > 0), "{what}: served: {read:?}");
}

/// Stops a host and returns its device identity.
async fn stopped_identity(h: TestHost) -> DeviceIdentity {
    h.host.shutdown().await;
    let dir = h.data_dir.clone();
    drop(h);
    DeviceIdentity::load_or_create(&dir).unwrap()
}

/// A raw endpoint a test controls, relay only.
async fn raw_endpoint(r: &Relay, key: SecretKey) -> Endpoint {
    let url: RelayUrl = r.url.parse().unwrap();
    Endpoint::builder(presets::Minimal)
        .secret_key(key)
        .relay_mode(RelayMode::custom([url]))
        .clear_ip_transports()
        .clear_address_lookup()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .transport_config(QuicTransportConfig::default())
        .bind()
        .await
        .expect("raw endpoint binds")
}

fn addr_of(host: &TestHost, r: &Relay) -> EndpointAddr {
    let id = wire::parse_endpoint(&host.host.reach_endpoint_id().unwrap()).unwrap();
    EndpointAddr::new(id).with_relay_url(r.url.parse().unwrap())
}

/// The host closes a connection from `key` with the gate's code, without the dialler sending a
/// byte, and counts it as refused before TLS. Had the gate let it through, the host would sit
/// waiting for a first stream and then for a `ClientHello`.
async fn assert_refused_before_tls(host: &TestHost, r: &Relay, key: SecretKey, what: &str) {
    let before = host.host.reach_counters_for_test();
    let ep = raw_endpoint(r, key).await;
    let conn = ep
        .connect(addr_of(host, r), ALPN)
        .await
        .unwrap_or_else(|e| panic!("{what}: QUIC handshake: {e}"));
    let reason = tokio::time::timeout(Duration::from_secs(5), conn.closed())
        .await
        .unwrap_or_else(|_| panic!("{what}: not closed: it got past the gate"));
    let ConnectionError::ApplicationClosed(close) = &reason else {
        panic!("{what}: closed some other way: {reason}");
    };
    assert_eq!(
        close.error_code,
        VarInt::from_u32(0x5244),
        "{what}: the gate's code"
    );
    let after = host.host.reach_counters_for_test();
    assert_eq!(after.refused, before.refused + 1, "{what}: counted refused");
    assert_eq!(after.admitted, before.admitted, "{what}: nothing admitted");
    ep.close().await;
}

// --- R1 -------------------------------------------------------------------------------------

/// R1: with only the relay transport, a file arrives with a matching hash and the clipboard
/// crosses, and both ends say honestly that this is the relay (SR-39).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r1_file_and_clipboard_cross_the_relay() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, mut b) = online_over_reach(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());

    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("across.bin");
    let size = 16 * 1024 * 1024;
    write_random_file(&file, size, 0x12_0001);
    let expected = file_root(&file, CHUNK);
    let started = Instant::now();
    let handle = a.host.send(&b_id, vec![file]).await.unwrap();
    let id = handle.id().to_owned();
    complete(&mut a, &id, TransferDirection::Send).await;
    complete(&mut b, &id, TransferDirection::Receive).await;
    println!(
        "R1: {} MiB over the relay at {:.1} MiB/s",
        size / (1024 * 1024),
        common::mib_per_s(size, started.elapsed())
    );
    assert_eq!(file_root(&b.recv_dir.join("across.bin"), CHUNK), expected);

    assert!(a
        .host
        .send_clipboard(&b_id, "over the relay", false)
        .unwrap());
    let ev = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
    let HostEvent::Clipboard { text, .. } = ev else {
        unreachable!()
    };
    assert_eq!(text.0.as_deref(), Some("over the relay"));

    // SR-39: the status line, asserted.
    for host in [&a, &b] {
        let paths = host.host.reach_paths();
        assert_eq!(paths.len(), 1, "{paths:?}");
        assert_eq!(paths[0].kind, ReachPathKind::Relay);
        let line = paths[0].status_line();
        assert!(line.contains("on the relay"), "{line}");
        assert!(line.contains("slow path"), "{line}");
        assert!(line.contains(&paths[0].label), "{line}");
    }

    // protocol.md 12: never stored as last-known, and it stays out of the reports.
    for (host, peer) in [(&a, b_id), (&b, a_id)] {
        let summary = host.host.peers();
        let s = summary
            .iter()
            .find(|s| s.device_id == peer.to_hex())
            .unwrap();
        assert!(s.online);
        assert!(s.last_known.iter().all(|x| !is_reach_addr(*x)), "{s:?}");
        let diag = host.host.diagnose();
        assert!(
            diag.peers
                .iter()
                .flat_map(|p| &p.last_known)
                .all(|x| !is_reach_addr(*x)),
            "{diag:?}"
        );
        for report in host.host.path_reports() {
            let line = report.summary_line("peer", 0);
            assert!(!line.contains("100::"), "{line}");
        }
    }
}

/// R1: a send killed mid-way resumes over the relay and does not start again from 0.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r1_a_killed_send_resumes_over_the_relay() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, mut b) = online_over_reach(root.path(), &r).await;
    let b_id = b.host.device_id();

    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("movie.bin");
    let size: u64 = 128 * 1024 * 1024;
    write_random_file(&file, size, 0x12_0002);
    let expected = file_root(&file, CHUNK);

    let handle = a.host.send(&b_id, vec![file]).await.unwrap();
    let id = handle.id().to_owned();
    let started = Instant::now();
    while handle.bytes_sent() < 48 * 1024 * 1024 {
        assert!(started.elapsed() < T, "send stalled");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    a.host.shutdown().await;
    drop(a);

    // What the receiver held when the sender died.
    let list = |b: &TestHost| {
        b.host
            .transfers()
            .into_iter()
            .find(|t| t.id == id && t.direction == TransferDirection::Receive)
            .map_or(0, |t| t.bytes)
    };
    wait_until("the receiver holds part of the file", || {
        list(&b) >= 8 * 1024 * 1024
    })
    .await;
    let held = list(&b);
    assert!(held < size, "the stop landed mid-stream");
    b.events.clear();

    // Watch what the receiver reports held: a restart at 0 would take it back to nothing.
    let mut a = start(root.path(), "alpha", 0, Some(relay_only(&r))).await;
    // It came back over reach, or this proves nothing about reach.
    let back = wait_online(&mut a).await;
    assert!(is_reach_addr(back), "the resume ran on {back}");
    assert_eq!(a.host.reach_paths().len(), 1, "alpha is on the relay");
    let mut smallest = list(&b);
    while !b.host.transfers().iter().any(|t| {
        t.id == id
            && t.direction == TransferDirection::Receive
            && t.state == TransferStatus::Complete
    }) {
        smallest = smallest.min(list(&b));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(started.elapsed() < T * 2, "resume stalled");
    }
    complete(&mut a, &id, TransferDirection::Send).await;
    complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("movie.bin"), CHUNK), expected);
    assert_eq!(
        a.host.reach_paths().len(),
        1,
        "still on the relay at the end"
    );
    println!("R1 resume: receiver held {held} bytes, never below {smallest}");
    assert!(
        smallest >= held,
        "the receiver fell back to {smallest} of {held}: the send restarted"
    );
}

// --- R2 -------------------------------------------------------------------------------------

/// R2: an unknown endpoint, an unpinned one and a pinned one with the grant off are closed before
/// any TLS, on one host, and the pinned session with the grant on keeps working throughout.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_the_gate_closes_everyone_else_before_tls() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, mut b) = online_over_reach(root.path(), &r).await;
    let b_id = b.host.device_id();

    // Unknown: a key nobody ever stored.
    assert_refused_before_tls(
        &a,
        &r,
        SecretKey::from_bytes(&[0x21; 32]),
        "unknown endpoint",
    )
    .await;

    // A real device's real endpoint that is not paired with this host.
    let stranger = DeviceIdentity::generate().unwrap();
    assert_refused_before_tls(&a, &r, secret_key_for(&stranger), "unpinned endpoint").await;

    // An ALPN this host does not speak: QUIC itself refuses, nothing is even counted.
    let ep = raw_endpoint(&r, SecretKey::from_bytes(&[0x22; 32])).await;
    let refused = tokio::time::timeout(
        Duration::from_secs(10),
        ep.connect(addr_of(&a, &r), b"playdrop/reach/2"),
    )
    .await
    .expect("refused in time");
    assert!(refused.is_err(), "another ALPN got a connection");
    ep.close().await;

    // The session is untouched.
    assert!(a.host.is_online(&b_id));
    assert!(a.host.send_clipboard(&b_id, "still here", false).unwrap());
    b.events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;

    // Pinned, grant off: bravo's own key once alpha turned bravo's grant off. Turning it off
    // also closes what the grant let in.
    a.host.set_remote_grant(&b_id, false).await.unwrap();
    wait_until("the reach connection closed with the grant", || {
        !a.host.is_online(&b_id)
    })
    .await;
    // The real bravo goes away: a relay serves one holder of a key at a time.
    let b_key = secret_key_for(&stopped_identity(b).await);
    assert_refused_before_tls(&a, &r, b_key.clone(), "pinned with the grant off").await;
    assert!(a.host.reach_stored_for_test(&b_id).is_some());
    assert!(!a.host.remote_grant(&b_id));

    // Unpinned after having been granted: unpair forgets the endpoint too.
    a.host.set_remote_grant(&b_id, true).await.unwrap();
    a.host.unpair(&b_id).await.unwrap();
    assert!(a.host.reach_stored_for_test(&b_id).is_none());
    assert_refused_before_tls(&a, &r, b_key, "unpaired since").await;
}

/// R2: the right Endpoint ID with the wrong device key gets past the gate, which trusts nothing,
/// and fails the inner TLS (SR-37). Also a pinned device whose grant is off cannot borrow another
/// peer's Endpoint ID to come in (SR-36).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_a_forged_endpoint_fails_the_inner_tls() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, b) = met(root.path(), &r).await;
    a.events.clear();

    // Someone holding bravo's endpoint key, but with a device key of their own. The real bravo
    // goes away first: a relay serves one holder of a key at a time.
    let b_key = secret_key_for(&stopped_identity(b).await);
    let thief = DeviceIdentity::generate().unwrap();
    let ep = raw_endpoint(&r, b_key.clone()).await;
    let before = a.host.reach_counters_for_test();
    let conn = ep.connect(addr_of(&a, &r), ALPN).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    // The dialler's half can finish: in TLS 1.3 the server judges the client certificate after
    // the client's last flight. It must then be cut off, with nothing served.
    let a_spki = DeviceIdentity::load_or_create(&a.data_dir)
        .unwrap()
        .spki_hash();
    let cfg = tls::build_client_config(&thief, a_spki);
    assert_not_served(
        cfg,
        Some(a_spki),
        recv,
        send,
        "stranger on a stolen endpoint key",
    )
    .await;
    let after = a.host.reach_counters_for_test();
    assert_eq!(after.admitted, before.admitted + 1, "the gate let it past");
    tokio::time::sleep(Duration::from_millis(200)).await;
    a.events.drain();
    assert!(
        !a.events.seen.iter().any(|e| matches!(
            e,
            HostEvent::PeerOnline { addr, .. } if is_reach_addr(*addr)
        )),
        "no reach session from a stranger"
    );
    ep.close().await;

    // A device pinned here, with bravo's Endpoint ID stored for bravo: its own certificate is
    // good, but this ID was not stored for it, and its grant is off.
    let mut c = start(root.path(), "charlie", 0, None).await;
    pair(&mut a, &mut c).await;
    let c_id = c.host.device_id();
    assert!(!a.host.remote_grant(&c_id));
    let c_identity = {
        c.host.shutdown().await;
        let dir = c.data_dir.clone();
        drop(c);
        DeviceIdentity::load_or_create(&dir).unwrap()
    };
    a.events.clear();
    // bravo's key again, so the gate sees a granted ID. Its old connection is gone with `ep`.
    let ep = raw_endpoint(&r, b_key.clone()).await;
    let conn = ep.connect(addr_of(&a, &r), ALPN).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let cfg = tls::build_client_config(&c_identity, a_spki);
    assert_not_served(
        cfg,
        Some(a_spki),
        recv,
        send,
        "pinned device on another's endpoint",
    )
    .await;
    ep.close().await;
}

/// R2: a reach connection during an open "Add device" window gets no pairing, although the same
/// client on TCP does get as far as an unpaired session (the positive control).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_no_pairing_over_reach_even_with_the_window_open() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    // The real bravo goes away: a relay serves one holder of a key at a time.
    let b_key = secret_key_for(&stopped_identity(b).await);
    a.host.open_add_device_window().unwrap();

    let stranger = DeviceIdentity::generate().unwrap();
    let cfg = tls::build_client_config_unpaired(&stranger);

    // Positive control: on TCP the window lets this client in, so it can read the server's
    // HTTP/2 settings frame.
    let tcp = tokio::net::TcpStream::connect(a.host.local_addr())
        .await
        .unwrap();
    let (mut lan, _) = tls::connect_tcp(cfg.clone(), None, tcp).await.unwrap();
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        lan.write_all(H2_START).await.unwrap();
        lan.flush().await.unwrap();
        let mut buf = [0u8; 64];
        let n = tokio::time::timeout(Duration::from_secs(5), lan.read(&mut buf))
            .await
            .unwrap()
            .expect("the open window serves an unpaired TCP client");
        assert!(n > 0, "window open: an unpaired TCP client gets HTTP/2");
    }

    // Reach, same window, same client, past the gate on a granted endpoint.
    let ep = raw_endpoint(&r, b_key).await;
    let conn = ep.connect(addr_of(&a, &r), ALPN).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    assert_not_served(
        cfg,
        None,
        recv,
        send,
        "unpaired client with the window open",
    )
    .await;
    ep.close().await;
    assert_eq!(a.host.peers().len(), 1, "nobody was paired");
}

/// R2: one connection, one bidirectional stream (SR-38). A second stream gets no credit and the
/// pinned session on the first keeps working.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r2_a_second_stream_is_refused_and_the_session_lives_on() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, mut b) = online_over_reach(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    // Whichever side dialled holds a connection it may ask on; the accepting side gets no credit
    // either, as the dialler's settings allow nothing back.
    let wait = Duration::from_secs(2);
    assert!(
        !a.host.reach_second_stream_for_test(&b_id, wait).await,
        "alpha got a second stream"
    );
    assert!(
        !b.host.reach_second_stream_for_test(&a_id, wait).await,
        "bravo got a second stream"
    );
    assert!(a.host.is_online(&b_id) && b.host.is_online(&a_id));
    a.events.clear();
    b.events.clear();
    assert!(a.host.send_clipboard(&b_id, "one stream", false).unwrap());
    b.events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
    assert!(b.host.send_clipboard(&a_id, "both ways", false).unwrap());
    a.events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
}

/// A `reach` frame from a peer this host has not granted is refused and not stored, hostile
/// values are ignored with the session left alone, and the order in which the two sides turn the
/// grant on does not matter (protocol.md 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_frame_needs_the_grant_on_the_receiving_side() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let mut a = start(root.path(), "alpha", 0, Some(relay_only(&r))).await;
    let mut b = start(root.path(), "bravo", 0, Some(relay_only(&r))).await;
    pair(&mut a, &mut b).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let b_endpoint = b.host.reach_endpoint_id().unwrap();

    // Nothing is said while no grant is on (SR-35).
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.host.reach_stored_for_test(&b_id).is_none());
    assert!(b.host.reach_stored_for_test(&a_id).is_none());

    // A real frame from bravo, unasked: alpha has not granted it, so it is refused, and says so.
    assert!(b
        .host
        .reach_send_frame_for_test(&a_id, Some(b_endpoint.clone()), Some(r.url.clone())));
    b.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::PeerRefused { code, frame_type, .. }
                if code == "forbidden" && frame_type.as_deref() == Some("reach"))
        })
        .await;
    assert!(a.host.reach_stored_for_test(&b_id).is_none());

    // With the grant on, values that break the rules are ignored and the session lives on.
    let long = format!("https://{}.example.org", "a".repeat(300));
    let bad: [(Option<&str>, Option<&str>); 7] = [
        (Some("not hex"), Some(&r.url)),
        (Some(&b_endpoint), None),
        (None, Some(&r.url)),
        (Some(&b_endpoint), Some("ftp://relay.example.org")),
        (Some(&b_endpoint), Some("http://8.8.8.8:3340")),
        (Some(&b_endpoint), Some(&long)),
        (Some(""), Some("")),
    ];
    a.host.set_remote_grant(&b_id, true).await.unwrap();
    for (e, u) in bad {
        let sent =
            b.host
                .reach_send_frame_for_test(&a_id, e.map(str::to_owned), u.map(str::to_owned));
        assert!(sent);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        a.host.reach_stored_for_test(&b_id).is_none(),
        "a bad value was stored"
    );
    assert!(
        a.host.is_online(&b_id) && b.host.is_online(&a_id),
        "bad values cost the session"
    );

    // Alpha's frame went to a bravo without the grant. Bravo grants: each ends up with the other's.
    b.host.set_remote_grant(&a_id, true).await.unwrap();
    wait_until("both stored whatever the order", || {
        a.host.reach_stored_for_test(&b_id).is_some()
            && b.host.reach_stored_for_test(&a_id).is_some()
    })
    .await;
    let (endpoint, relay_url) = b.host.reach_stored_for_test(&a_id).unwrap();
    assert_eq!(endpoint, a.host.reach_endpoint_id().unwrap());
    assert!(wire::parse_relay(&relay_url).is_ok());

    // Taking the grant back makes the other side forget.
    a.host.set_remote_grant(&b_id, false).await.unwrap();
    wait_until("bravo forgot alpha's endpoint", || {
        b.host.reach_stored_for_test(&a_id).is_none()
    })
    .await;
    assert!(a.host.is_online(&b_id), "the LAN session is not affected");
}

/// A paired peer cannot make this host dial someone else: an Endpoint ID held for another pin, or
/// this host's own, is ignored (protocol.md 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_frame_naming_someone_elses_endpoint_is_ignored() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut h, v) = met(root.path(), &r).await;
    let v_id = v.host.device_id();
    let v_endpoint = v.host.reach_endpoint_id().unwrap();
    let mut m = start(root.path(), "mallory", 0, Some(relay_only(&r))).await;
    h.events.clear();
    pair(&mut h, &mut m).await;
    let (h_id, m_id) = (h.host.device_id(), m.host.device_id());
    h.host.set_remote_grant(&m_id, true).await.unwrap();

    let m_says = |endpoint: String| {
        assert!(m
            .host
            .reach_send_frame_for_test(&h_id, Some(endpoint), Some(r.url.clone())));
    };
    m_says(v_endpoint.clone());
    m_says(h.host.reach_endpoint_id().unwrap());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        h.host.reach_stored_for_test(&m_id),
        None,
        "M's claim was stored"
    );
    assert_eq!(h.host.reach_stored_for_test(&v_id).unwrap().0, v_endpoint);
    // The drop is visible to the user, by label, with no ID in it (T46, SR-40).
    let notes = h.host.reach_notes();
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("mallory") && notes[0].contains("cannot be reached"),
        "{notes:?}"
    );
    assert!(!notes[0].contains(&v_endpoint), "{notes:?}");

    // Positive control: its own endpoint is taken, and the note goes.
    m_says(m.host.reach_endpoint_id().unwrap());
    wait_until("mallory's own endpoint stored", || {
        h.host.reach_stored_for_test(&m_id).is_some()
    })
    .await;
    assert_eq!(h.host.reach_notes(), Vec::<String>::new());
}

/// A dial that fails its inner TLS does not cut the working session that dialler already has: the
/// older connection is replaced only after the inner TLS and `admit` (protocol.md 12, SR-38).
/// Before this, one paired peer naming V's Endpoint ID made V drop H again and again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_dial_does_not_cut_the_session_it_comes_from() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, mut b) = online_alpha_dialled(root.path(), &r).await;
    let b_endpoint = b.host.reach_endpoint_id().unwrap();
    a.events.clear();
    b.events.clear();
    for _ in 0..5 {
        assert!(
            a.host.reach_dial_wrong_key_for_test(&b_endpoint).await,
            "the wrong key must fail the handshake"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (name, h) in [("alpha", &mut a), ("bravo", &mut b)] {
        h.events.drain();
        let offline = h
            .events
            .seen
            .iter()
            .filter(|e| matches!(e, HostEvent::PeerOffline { .. }))
            .count();
        assert_eq!(offline, 0, "{name} went offline over a dial that failed");
    }
    let b_id = b.host.device_id();
    assert!(a.host.send_clipboard(&b_id, "still here", false).unwrap());
    b.events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
}

/// A revoke over a reach session reaches the peer, even with a send filling the connection, so it
/// stops dialling (protocol.md 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_over_a_reach_session_reaches_the_peer_during_a_send() {
    init_logs();
    let top = temp_root();
    let r = relay().await;
    let src = top.path().join("big.bin");
    write_random_file(&src, 48 * 1024 * 1024, 0x12_0006);
    for round in 0..5 {
        let dir = top.path().join(format!("round{round}"));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = online_over_reach(&dir, &r).await;
        let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
        let handle = a.host.send(&b_id, vec![src.clone()]).await.unwrap();
        wait_until("bytes moving", || handle.bytes_sent() > 2 * 1024 * 1024).await;
        a.host.set_remote_grant(&b_id, false).await.unwrap();
        wait_until("the reach connection closed with the grant", || {
            !a.host.is_online(&b_id)
        })
        .await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            b.host.reach_stored_for_test(&a_id),
            None,
            "round {round}: the forget frame was lost"
        );
        a.host.shutdown().await;
        b.host.shutdown().await;
    }
}

/// `told` stays on after a revoke until a later session has said it again, as the first frame can
/// be lost with its connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_later_session_says_the_revoke_again() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, b) = met(root.path(), &r).await;
    let b_id = b.host.device_id();
    let file = a.data_dir.join("reach.json");
    let told = |file: &Path| {
        let v: serde_json::Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        v["peers"][b_id.to_hex()]["told"].as_bool()
    };
    assert_eq!(told(&file), Some(true));
    a.host.set_remote_grant(&b_id, false).await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        told(&file),
        Some(true),
        "told cleared while it may have been lost"
    );

    // A new session on the LAN with a bravo that names no relay: it lists no `reach` capability,
    // so the frame cannot go, and that must not count as done.
    let (name, port, data) = (b.name.clone(), b.port, a.data_dir.clone());
    b.host.shutdown().await;
    drop(b);
    a.events.clear();
    let quiet = start(root.path(), &name, port, None).await;
    wait_online(&mut a).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(
        told(&data.join("reach.json")),
        Some(true),
        "marked done with nothing sent"
    );

    // With a relay named again it can hear it, and now it is done.
    quiet.host.shutdown().await;
    drop(quiet);
    a.events.clear();
    let _b = start(root.path(), &name, port, Some(relay_only(&r))).await;
    wait_online(&mut a).await;
    wait_until("told cleared after the session said it again", || {
        told(&data.join("reach.json")) == Some(false)
    })
    .await;
}

/// A dialler whose peer's gate keeps refusing it asks less and less often, so a lost forget frame
/// does not leave it knocking every retry for good.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_dialler_backs_off() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    // Bravo is away when alpha revokes, so it never hears, and it still holds alpha's endpoint.
    b.host.shutdown().await;
    drop(b);
    a.host.set_remote_grant(&b_id, false).await.unwrap();
    a.host.shutdown().await;
    drop(a);
    // New ports: bravo's stored LAN address for alpha is dead.
    let a = start(root.path(), "alpha", 0, Some(relay_only(&r))).await;
    let mut cfg = relay_only(&r);
    cfg.retry_every = Duration::from_millis(100);
    let b = start(root.path(), "bravo", 0, Some(cfg)).await;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let _ = b.host.connect(&a_id).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let dials = b.host.reach_counters_for_test().dials;
    let refused = a.host.reach_counters_for_test().refused;
    println!("refused dialler: {dials} dials in 5 s, {refused} refused");
    assert!(refused >= 2, "the gate refused it: {refused}");
    assert!(
        dials <= 7,
        "with a 100 ms retry and no back-off this is dozens: {dials}"
    );
}

/// A connection in its inner handshake when the grant goes does not live on: the revoke's sweep
/// cannot see it yet, so it is asked at `connection_opened` (SR-14).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_catches_a_connection_still_in_its_handshake() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let b_id = b.host.device_id();
    let a_spki = DeviceIdentity::load_or_create(&a.data_dir)
        .unwrap()
        .spki_hash();
    let b_identity = stopped_identity(b).await;
    let ep = raw_endpoint(&r, secret_key_for(&b_identity)).await;
    let conn = ep.connect(addr_of(&a, &r), ALPN).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let cfg = tls::build_client_config(&b_identity, a_spki);
    let io = tokio::io::join(recv, send);
    let (stream, _) = tls::connect(cfg, Some(a_spki), "100::1".parse().unwrap(), io)
        .await
        .expect("bravo's own key passes the inner TLS");
    // Past `admit`, waiting for the HTTP/2 preface, and not yet tracked: then the grant goes.
    tokio::time::sleep(Duration::from_millis(400)).await;
    a.host.set_remote_grant(&b_id, false).await.unwrap();

    let answered = tokio::time::timeout(Duration::from_secs(10), async {
        let (send, driver) = h2::client::handshake(stream).await.ok()?;
        tokio::spawn(driver);
        let mut send = send.ready().await.ok()?;
        let req = http::Request::get("/v1/outbox").body(()).unwrap();
        let (resp, _) = send.send_request(req, true).ok()?;
        resp.await.ok().map(|r| r.status())
    })
    .await
    .expect("closed, not left hanging");
    assert_eq!(answered, None, "served after the grant went");
    ep.close().await;
}

/// SR-14: a revoke holds even when the disk does not: the frame goes, the connections close, and
/// the failed save is reported after.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_failed_save_does_not_stop_a_revoke() {
    use std::os::unix::fs::PermissionsExt;
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = online_over_reach(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let dir = a.data_dir.clone();
    let mode = |m| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(m)).unwrap();
    mode(0o500);
    let result = a.host.set_remote_grant(&b_id, false).await;
    assert!(result.is_err(), "the save cannot have worked: {result:?}");
    assert!(!a.host.remote_grant(&b_id), "off in memory all the same");
    wait_until("closed with the grant", || !a.host.is_online(&b_id)).await;
    wait_until("bravo heard", || {
        b.host.reach_stored_for_test(&a_id).is_none()
    })
    .await;
    mode(0o700);
}

/// A TLS and HTTP/2 server with `identity` that answers every request with `409`, standing in for
/// a peer on a LAN path that refuses the session. Requests it saw are counted.
fn refusing_peer(
    identity: &DeviceIdentity,
    client: playdrop_core::identity::DeviceId,
    listener: tokio::net::TcpListener,
) {
    let window = Arc::new(AtomicBool::new(false));
    let pins = Arc::new(RwLock::new(HashSet::from([client])));
    let config = tls::build_server_config(identity, pins.clone(), window.clone());
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let (config, pins, window) = (config.clone(), pins.clone(), window.clone());
            tokio::spawn(async move {
                let Ok((stream, _)) = tls::accept(config, pins.as_ref(), &window, tcp).await else {
                    return;
                };
                let Ok(mut h2c) = h2::server::handshake(stream).await else {
                    return;
                };
                while let Some(Ok((_, mut respond))) = h2c.accept().await {
                    let resp = http::Response::builder().status(409).body(()).unwrap();
                    let _ = respond.send_response(resp, true);
                }
            });
        }
    });
}

/// A host that dialled nothing and has its session on reach records `Failed`, not `Switched`, when
/// the LAN path it found refuses the session, and stays where it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_move_back_to_the_lan_is_not_recorded_as_moved() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, mut b) = online_alpha_dialled(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let a_identity = DeviceIdentity::load_or_create(&a.data_dir).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lan = listener.local_addr().unwrap();
    refusing_peer(&a_identity, b_id, listener);
    b.events.clear();
    b.host.verified_advert_for_test(&a_id, lan);
    wait_until("the check ends", || {
        b.host.path_reports().iter().any(|p| p.outcome.is_some())
    })
    .await;
    let outcome = b.host.path_reports()[0].outcome;
    assert_eq!(
        outcome,
        Some(PathOutcome::Failed),
        "the session did not move"
    );
    assert_eq!(b.host.reach_paths().len(), 1, "still on the relay");
    assert!(b.host.is_online(&a_id) && a.host.is_online(&b_id));
    b.events.drain();
    assert!(
        !b.events
            .seen
            .iter()
            .any(|e| matches!(e, HostEvent::PeerOffline { .. })),
        "the refused move cost the session"
    );
}

/// A grant that goes while this host's own reach dial is under way: the dial finishes its
/// handshakes, and `adopt` closes it instead of opening a session (SR-14).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoke_during_a_reach_dial_stops_the_session() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let b_identity = stopped_identity(b).await;
    wait_until("bravo gone", || !a.host.is_online(&b_id)).await;

    // Bravo as a raw endpoint, which holds the dial at its first stream until told to go on.
    let url: RelayUrl = r.url.parse().unwrap();
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key_for(&b_identity))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::custom([url]))
        .clear_ip_transports()
        .clear_address_lookup()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let incoming = tokio::time::timeout(T, ep.accept())
        .await
        .expect("alpha dials")
        .expect("endpoint open");
    let quic = incoming.await.unwrap();
    let (send, recv) = tokio::time::timeout(T, quic.accept_bi())
        .await
        .unwrap()
        .unwrap();

    // Mid-dial: the sweep of the revoke cannot see this connection.
    a.host.set_remote_grant(&b_id, false).await.unwrap();

    let window = Arc::new(AtomicBool::new(false));
    let pins = Arc::new(RwLock::new(HashSet::from([a_id])));
    let config = tls::build_server_config(&b_identity, pins.clone(), window.clone());
    let (stream, _) = tls::accept(config, pins.as_ref(), &window, tokio::io::join(recv, send))
        .await
        .expect("the handshake itself is good");
    // Alpha may hang up before this side finishes HTTP/2. What must not come is a request.
    if let Ok(mut h2c) = h2::server::handshake(stream).await {
        let request = tokio::time::timeout(Duration::from_secs(3), h2c.accept()).await;
        assert!(
            !matches!(&request, Ok(Some(Ok(_)))),
            "alpha sent a request on a dial it should have closed: {request:?}"
        );
    }
    assert!(!a.host.is_online(&b_id));
    ep.close().await;
}

/// A session that comes up on a reach connection after the grant went, before the revoke's sweep
/// got to it, is ended at once when placed (SR-14). The sweep is held back here so that only this
/// can end it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_placed_after_the_revoke_is_ended() {
    use playdrop_core::session::types::{Caps, DeviceType, Hello, Trust};
    use playdrop_core::session::{Frame, OutboundEncoder};
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let b_id = b.host.device_id();
    let b_identity = stopped_identity(b).await;
    a.host.shutdown().await;
    drop(a);
    let mut cfg = relay_only(&r);
    cfg.revoke_grace = Some(Duration::from_secs(120));
    let mut a = start(root.path(), "alpha", 0, Some(cfg)).await;
    let a_spki = DeviceIdentity::load_or_create(&a.data_dir)
        .unwrap()
        .spki_hash();

    let ep = raw_endpoint(&r, secret_key_for(&b_identity)).await;
    let conn = ep.connect(addr_of(&a, &r), ALPN).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let cfg = tls::build_client_config(&b_identity, a_spki);
    let io = tokio::io::join(recv, send);
    let (stream, _) = tls::connect(cfg, Some(a_spki), "100::1".parse().unwrap(), io)
        .await
        .unwrap();
    let (h2send, driver) = h2::client::handshake(stream).await.unwrap();
    let driver = tokio::spawn(driver);
    // Up and tracked at alpha, not yet a session: now the grant goes.
    let mut ready = h2send.ready().await.unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    a.host.set_remote_grant(&b_id, false).await.unwrap();

    let req = http::Request::post("/v1/session")
        .header(
            http::header::CONTENT_TYPE,
            "application/vnd.playdrop.frames",
        )
        .body(())
        .unwrap();
    let (_resp, mut body) = ready.send_request(req, false).unwrap();
    let hello = Frame::Hello(Hello {
        protocol_version: playdrop_core::constants::PROTOCOL_VERSION,
        device_id: b_identity.device_id().to_hex(),
        name: "bravo".into(),
        device_type: DeviceType::Desktop,
        port: 1,
        app: "test".into(),
        keep_alive: 60,
        trust: Trust::Paired,
        caps: Caps::default(),
    });
    let (_, bytes) = OutboundEncoder::default().encode(&hello, None).unwrap();
    body.send_data(bytes, false).unwrap();

    // The connection goes, long before the sweep. Nothing about this peer happened first.
    tokio::time::timeout(Duration::from_secs(10), driver)
        .await
        .expect("the connection closed long before the sweep")
        .unwrap()
        .ok();
    a.events.drain();
    let reported = a
        .events
        .seen
        .iter()
        .filter(|e| matches!(e, HostEvent::PeerOnline { .. }))
        .count();
    assert_eq!(reported, 0, "reported online though the grant had gone");
    assert!(!a.host.is_online(&b_id));
    ep.close().await;
}

/// A reach dial stuck on its way does not hold up a LAN dial, and when it does finish the LAN path
/// keeps its place: a peer never ends up with both (protocol.md 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lan_dial_is_not_held_up_by_a_reach_dial() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let b_identity = stopped_identity(b).await;
    wait_until("bravo gone", || !a.host.is_online(&b_id)).await;

    // Bravo as a raw endpoint, which holds the dial at its first stream.
    let url: RelayUrl = r.url.parse().unwrap();
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key_for(&b_identity))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::custom([url]))
        .clear_ip_transports()
        .clear_address_lookup()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let incoming = tokio::time::timeout(T, ep.accept())
        .await
        .expect("alpha dials")
        .expect("endpoint open");
    let quic = incoming.await.unwrap();
    let (send, recv) = tokio::time::timeout(T, quic.accept_bi())
        .await
        .unwrap()
        .unwrap();

    // A LAN path appears with that dial still stuck: a dial to it goes at once.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let lan = listener.local_addr().unwrap();
    refusing_peer(&b_identity, a_id, listener);
    a.host.verified_advert_for_test(&b_id, lan);
    tokio::time::timeout(Duration::from_secs(3), a.host.connect(&b_id))
        .await
        .expect("the LAN dial waited for the reach dial")
        .expect("the LAN dial works");

    // Now the reach dial finishes its handshakes: it must not take the LAN path's place.
    let window = Arc::new(AtomicBool::new(false));
    let pins = Arc::new(RwLock::new(HashSet::from([a_id])));
    let config = tls::build_server_config(&b_identity, pins.clone(), window.clone());
    let accepted = tls::accept(config, pins.as_ref(), &window, tokio::io::join(recv, send)).await;
    if let Ok((stream, _)) = accepted {
        if let Ok(mut h2c) = h2::server::handshake(stream).await {
            let request = tokio::time::timeout(Duration::from_secs(3), h2c.accept()).await;
            assert!(
                !matches!(&request, Ok(Some(Ok(_)))),
                "the reach dial was adopted beside the LAN path: {request:?}"
            );
        }
    }
    let chosen = a.host.path_reports()[0].chosen;
    assert_eq!(chosen, Some(lan), "the LAN path keeps its place");
    assert_eq!(
        a.host.reach_paths(),
        Vec::new(),
        "no reach connection in use"
    );
    ep.close().await;
}

/// A reach dial that works is used as soon as the LAN dial before it has failed, and does not wait
/// behind the next LAN dial for the dial lock: with a silent LAN address that one holds it for 5 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_finished_reach_dial_does_not_wait_for_the_next_lan_dial() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let data = a.data_dir.clone();
    a.host.shutdown().await;
    b.host.shutdown().await;
    drop((a, b));
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = sock.listen(1).unwrap();
    let silent = listener.local_addr().unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        let dial = tokio::net::TcpStream::connect(silent);
        if let Ok(Ok(c)) = tokio::time::timeout(Duration::from_millis(300), dial).await {
            held.push(c);
        }
    }
    // Alpha's only stored address for bravo is the silent one.
    let pins = data.join("pins.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&pins).unwrap()).unwrap();
    v["pins"][0]["last_known"] = serde_json::json!([silent.to_string()]);
    std::fs::write(&pins, serde_json::to_vec(&v).unwrap()).unwrap();

    let mut quiet = relay_only(&r);
    quiet.never_dial = true;
    let _b = start(root.path(), "bravo", 0, Some(quiet)).await;
    let started = Instant::now();
    let mut a = start(root.path(), "alpha", 0, Some(relay_only(&r))).await;
    let addr = wait_online(&mut a).await;
    let took = started.elapsed();
    println!("cold reach dial behind a silent LAN address: online after {took:?}");
    assert!(is_reach_addr(addr), "{addr}");
    assert!(
        took < Duration::from_millis(8500),
        "online after {took:?}: it waited for the next LAN dial"
    );
}

/// With a stale LAN address that drops packets and a peer that is away, dial tasks do not pile up
/// behind a slow reach dial and `connect` answers in about one dial. This runs for a minute:
/// measured before the fix, tasks grew by about 8 a minute and `connect` was blocked past 120 s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dial_tasks_do_not_pile_up_behind_a_reach_dial() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let b_id = b.host.device_id();
    a.host.shutdown().await;
    b.host.shutdown().await;
    drop((a, b));
    // A silent LAN candidate: a full accept queue, so the kernel drops further SYNs.
    let sock = tokio::net::TcpSocket::new_v4().unwrap();
    sock.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let listener = sock.listen(1).unwrap();
    let silent = listener.local_addr().unwrap();
    let mut held = Vec::new();
    for _ in 0..4 {
        let dial = tokio::net::TcpStream::connect(silent);
        if let Ok(Ok(c)) = tokio::time::timeout(Duration::from_millis(300), dial).await {
            held.push(c);
        }
    }
    let mut c = relay_only(&r);
    c.retry_every = reach::RETRY_EVERY;
    let a = start(root.path(), "alpha", 0, Some(c)).await;
    a.host.verified_advert_for_test(&b_id, silent);
    tokio::time::sleep(Duration::from_secs(15)).await;
    let early = a.host.tasks_for_test();
    tokio::time::sleep(Duration::from_secs(30)).await;
    let late = a.host.tasks_for_test();
    let dials = a.host.reach_counters_for_test().dials;
    let started = Instant::now();
    let answered = tokio::time::timeout(Duration::from_secs(30), a.host.connect(&b_id)).await;
    let took = started.elapsed();
    println!(
        "backlog: tasks {early} at 15 s, {late} at 45 s, {dials} reach dials, connect {took:?}"
    );
    assert!(answered.is_ok(), "connect still blocked after {took:?}");
    assert!(late <= early + 1, "dial tasks pile up: {early} then {late}");
    assert!(
        took < Duration::from_secs(22),
        "connect took {took:?}: more than one round of dials"
    );
}

/// An Endpoint ID with the wrong key behind it, as when a peer names one that is not its own: this
/// host backs off instead of asking the relay for it every 15 s for good (protocol.md 12).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dial_that_finds_the_wrong_key_backs_off() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let mut cfg = relay_only(&r);
    cfg.retry_every = Duration::from_millis(100);
    let mut h = start(root.path(), "alpha", 0, Some(cfg)).await;
    let mut m = start(root.path(), "mallory", 0, Some(relay_only(&r))).await;
    pair(&mut h, &mut m).await;
    let (h_id, m_id) = (h.host.device_id(), m.host.device_id());
    h.host.set_remote_grant(&m_id, true).await.unwrap();

    // Someone at that ID answers, with a device key that is not mallory's.
    let stranger = DeviceIdentity::generate().unwrap();
    let url: RelayUrl = r.url.parse().unwrap();
    let ep = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&[0x41; 32]))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::custom([url]))
        .clear_ip_transports()
        .clear_address_lookup()
        .ca_tls_config(CaTlsConfig::insecure_skip_verify())
        .bind()
        .await
        .unwrap();
    let id = wire::endpoint_hex(&ep.id());
    let window = Arc::new(AtomicBool::new(true));
    let pins = Arc::new(RwLock::new(HashSet::new()));
    let config = tls::build_server_config(&stranger, pins.clone(), window.clone());
    let accepter = ep.clone();
    tokio::spawn(async move {
        while let Some(incoming) = accepter.accept().await {
            let (config, pins, window) = (config.clone(), pins.clone(), window.clone());
            tokio::spawn(async move {
                let Ok(q) = incoming.await else { return };
                let Ok((send, recv)) = q.accept_bi().await else {
                    return;
                };
                let _ =
                    tls::accept(config, pins.as_ref(), &window, tokio::io::join(recv, send)).await;
            });
        }
    });
    assert!(m
        .host
        .reach_send_frame_for_test(&h_id, Some(id), Some(r.url.clone())));
    wait_until("the claim stored", || {
        h.host.reach_stored_for_test(&m_id).is_some()
    })
    .await;
    m.host.shutdown().await;
    drop(m);
    wait_until("mallory gone", || !h.host.is_online(&m_id)).await;
    let before = h.host.reach_counters_for_test().dials;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(5) {
        let _ = h.host.connect(&m_id).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let dials = h.host.reach_counters_for_test().dials - before;
    println!("wrong key: {dials} dials in 5 s");
    assert!(dials >= 2, "it did dial: {dials}");
    assert!(
        dials <= 7,
        "with a 100 ms retry and no back-off this is dozens: {dials}"
    );
    ep.close().await;
}

/// The rate limit counts stored changes only, and holds with no relay named on this host.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_change_limit_counts_changes_and_needs_no_relay() {
    init_logs();
    let root = temp_root();
    let mut a = start(root.path(), "alpha", 0, None).await;
    let mut b = start(root.path(), "bravo", 0, None).await;
    pair(&mut a, &mut b).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    a.host.set_remote_grant(&b_id, true).await.unwrap();
    let url = "https://relay.example.org";
    let value = |i: u8| wire::endpoint_hex(&SecretKey::from_bytes(&[i + 1; 32]).public());
    let say = |i: u8| {
        assert!(b
            .host
            .reach_send_frame_for_test(&a_id, Some(value(i)), Some(url.to_owned())));
    };
    // Repeats cost nothing: this many of the same value leave nine changes to go.
    for _ in 0..25 {
        say(0);
    }
    for i in 0..10 {
        say(i);
        wait_until("a change within the limit stored", || {
            a.host
                .reach_stored_for_test(&b_id)
                .is_some_and(|(e, _)| e == value(i))
        })
        .await;
    }
    say(10);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (stored, _) = a.host.reach_stored_for_test(&b_id).unwrap();
    assert_eq!(
        stored,
        value(9),
        "the eleventh change in a minute was stored"
    );
}

/// A peer naming another relay is not dialled: only the relay this user named is contacted (SR-35).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_naming_another_relay_is_not_dialled() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let b_endpoint = b.host.reach_endpoint_id().unwrap();
    // Nothing listens there, and with this rule nothing ever tries. Bravo's real frame can still
    // be on its way from when the two met, so it says this until the value stays.
    let other = "https://127.0.0.1:9";
    let names_other = |a: &TestHost| {
        a.host
            .reach_stored_for_test(&b_id)
            .is_some_and(|(_, u)| u.contains("127.0.0.1:9"))
    };
    let started = Instant::now();
    loop {
        assert!(b.host.reach_send_frame_for_test(
            &a_id,
            Some(b_endpoint.clone()),
            Some(other.to_owned())
        ));
        tokio::time::sleep(Duration::from_millis(500)).await;
        if names_other(&a) {
            break;
        }
        assert!(started.elapsed() < T, "alpha never stored the other relay");
    }
    b.host.shutdown().await;
    drop(b);
    wait_until("bravo gone", || !a.host.is_online(&b_id)).await;
    for _ in 0..10 {
        let _ = a.host.connect(&b_id).await;
        assert_eq!(
            a.host.reach_counters_for_test().dials,
            0,
            "dialled another relay"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(names_other(&a), "the test lost its setup");
    assert_eq!(
        a.host.reach_counters_for_test().dials,
        0,
        "dialled another relay"
    );
}

/// The label follows the connection in use, not what is stored: a peer's forget leaves the host on
/// the relay and it still says so (SR-39). Reports show the mask, not a tag of the Endpoint ID.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_label_follows_the_connection_and_reports_are_masked() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = online_alpha_dialled(root.path(), &r).await;
    let a_endpoint = wire::parse_endpoint(&a.host.reach_endpoint_id().unwrap()).unwrap();
    let b_endpoint = wire::parse_endpoint(&b.host.reach_endpoint_id().unwrap()).unwrap();
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());

    for h in [&a, &b] {
        let events = serde_json::to_string(&h.events.seen).unwrap();
        for id in [&a_endpoint, &b_endpoint] {
            let tag = reach::synthetic_addr(id).ip().to_string();
            assert!(
                !events.contains(&tag),
                "the tag {tag} in an event: {events}"
            );
        }
        assert!(events.contains("[100::]:0"), "{events}");
    }
    wait_until("alpha reports the path it dialled", || {
        a.host.path_reports().iter().any(|p| p.chosen.is_some())
    })
    .await;
    for p in a.host.path_reports() {
        assert_eq!(p.chosen, Some(reach::MASKED_ADDR), "{p:?}");
    }

    assert!(b.host.reach_send_frame_for_test(&a_id, None, None));
    wait_until("alpha forgot", || {
        a.host.reach_stored_for_test(&b_id).is_none()
    })
    .await;
    assert!(a.host.is_online(&b_id));
    assert_eq!(
        a.host.reach_paths().len(),
        1,
        "still on the relay, still says so"
    );
}

/// What a host that may hole-punch gives away (threat-model.md T45): an endpoint nobody paired,
/// holding nothing but this host's Endpoint ID, gets a UDP packet from this host at an address it
/// names, before the gate refuses it. So relay only is the default, and with it nothing is sent.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_host_that_asked_for_direct_probes_what_a_stranger_names() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    assert!(
        !ReachConfig::new("https://relay.example.org").direct,
        "relay only by default"
    );
    for direct in [false, true] {
        let mut cfg = relay_only(&r);
        cfg.direct = direct;
        let a = start(root.path(), "alpha", 0, Some(cfg)).await;
        let observer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let named = observer.local_addr().unwrap();
        let url: RelayUrl = r.url.parse().unwrap();
        let ep = Endpoint::builder(presets::Minimal)
            .secret_key(SecretKey::from_bytes(&[0x31 + u8::from(direct); 32]))
            .relay_mode(RelayMode::custom([url]))
            .clear_ip_transports()
            .bind_addr("127.0.0.1:0")
            .unwrap()
            .external_addr(named)
            .clear_address_lookup()
            .ca_tls_config(CaTlsConfig::insecure_skip_verify())
            .bind()
            .await
            .unwrap();
        let _conn = ep.connect(addr_of(&a, &r), ALPN).await;
        let mut buf = [0u8; 2048];
        let probe =
            tokio::time::timeout(Duration::from_secs(6), observer.recv_from(&mut buf)).await;
        println!(
            "direct {direct}: named address got {:?}",
            probe.as_ref().map(std::result::Result::is_ok)
        );
        assert_eq!(
            probe.is_ok(),
            direct,
            "direct {direct}: a packet at the address a stranger named: {probe:?}"
        );
        ep.close().await;
        a.host.shutdown().await;
    }
}

/// `pins.json` written by a host with the grant on and a peer's values stored still has only what
/// any build reads, and a new pairing does not inherit a grant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_pins_file_stays_compatible_with_builds_without_the_feature() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, b) = met(root.path(), &r).await;
    let b_id = b.host.device_id();
    let data = a.data_dir.clone();
    a.host.shutdown().await;
    drop(a);
    let text = std::fs::read_to_string(data.join("pins.json")).unwrap();
    for word in ["remote", "endpoint", "relay", "reach"] {
        assert!(!text.contains(word), "{word} in pins.json");
    }
    // `pin_store` refuses unknown fields, so this is the same check a default build runs.
    let store = playdrop_core::pin_store::PinStore::open(data.join("pins.json")).unwrap();
    assert!(store.get(&b_id).is_some());
}

// --- R3 -------------------------------------------------------------------------------------

/// R3: feature on, no relay named: no endpoint exists, nothing is told, nothing is dialled, even
/// with the grant on (SR-35). The dependency-tree half of R3 is in `bench/reach-netns.sh`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r3_no_relay_named_means_no_endpoint() {
    init_logs();
    let root = temp_root();
    let mut a = start(root.path(), "alpha", 0, None).await;
    let mut b = start(root.path(), "bravo", 0, None).await;
    assert!(a.host.reach_endpoint_id().is_none());
    assert!(b.host.reach_endpoint_id().is_none());
    pair(&mut a, &mut b).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let caps = a
        .host
        .peers()
        .into_iter()
        .find(|p| p.device_id == b_id.to_hex())
        .unwrap()
        .capabilities;
    assert!(
        !caps.iter().any(|c| c == reach::CAPABILITY),
        "no relay, no capability: {caps:?}"
    );
    a.host.set_remote_grant(&b_id, true).await.unwrap();
    b.host.set_remote_grant(&a_id, true).await.unwrap();
    assert!(a.host.remote_grant(&b_id), "the grant itself can be set");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(a.host.reach_stored_for_test(&b_id).is_none());
    assert_eq!(a.host.reach_paths(), Vec::new());
}

/// A relay URL that breaks the rules stops the host from starting, with a clear error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_relay_url_is_an_error_at_start() {
    let root = temp_root();
    let data = root.path().join("d");
    let mut cfg = playdrop_core::host::HostConfig::new(&data, root.path().join("r"), "x");
    cfg.port = 0;
    cfg.bind_ip = std::net::Ipv4Addr::LOCALHOST.into();
    cfg.discovery = false;
    cfg.reach = Some(ReachConfig::new("http://relay.example.org"));
    let e = playdrop_core::host::Host::start(cfg).await.err().unwrap();
    assert!(e.to_string().contains("reach-relay"), "{e}");
}

// --- R4 -------------------------------------------------------------------------------------

/// R4: with a working LAN path reach is never dialled. Bravo stays up while alpha restarts, so
/// alpha's first dial finds the LAN; bravo's own redial meanwhile fails on the LAN and may try
/// reach, which is what it is for, so from then on only changes count.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_a_working_lan_path_never_dials_reach() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (a, mut b) = met(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let a_port = a.port;
    a.host.shutdown().await;
    drop(a);
    b.events.clear();
    let mut a = start(root.path(), "alpha", a_port, Some(relay_only(&r))).await;
    let addr = wait_online(&mut a).await;
    assert!(!is_reach_addr(addr), "{addr}");
    wait_online(&mut b).await;
    assert!(a.host.remote_grant(&b_id) && b.host.remote_grant(&a_id));
    let bravo_then = b.host.reach_counters_for_test();
    assert_eq!(
        a.host.reach_counters_for_test().dials,
        0,
        "alpha dialled reach with a working LAN path"
    );
    // A send, a clipboard and several rounds of the dial loop later, still nothing.
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("lan.bin");
    write_random_file(&file, 1024 * 1024, 0x12_0004);
    let id = a
        .host
        .send(&b_id, vec![file])
        .await
        .unwrap()
        .id()
        .to_owned();
    complete(&mut a, &id, TransferDirection::Send).await;
    assert!(b.host.send_clipboard(&a_id, "lan", false).unwrap());
    tokio::time::sleep(Duration::from_secs(6)).await;
    let c = a.host.reach_counters_for_test();
    assert_eq!(c.dials, 0, "alpha dialled reach with a working LAN path");
    assert_eq!(c.admitted, 0, "nothing came in over reach");
    let now = b.host.reach_counters_for_test();
    assert_eq!(
        now.dials, bravo_then.dials,
        "bravo dialled reach while on the LAN"
    );
    assert!(
        a.host.reach_paths().is_empty(),
        "no slow-path label on a LAN"
    );
    assert!(
        b.host.reach_paths().is_empty(),
        "no slow-path label on a LAN"
    );
}

/// R4: a LAN path appearing moves the session to it with no offline, on the host that dialled
/// reach and on the host that was dialled, whichever hears the advert.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_a_lan_path_appearing_moves_the_session_with_no_offline() {
    for hears in ["alpha", "bravo"] {
        init_logs();
        let root = temp_root();
        let r = relay().await;
        let (mut a, mut b) = online_over_reach(root.path(), &r).await;
        let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
        assert_eq!(a.host.reach_paths().len(), 1);
        a.events.clear();
        b.events.clear();
        // One side hears the other's advert, as when the phone joins the Wi-Fi. Each host
        // listens on a real loopback port, so that is the LAN path.
        let (hearer, heard_addr, target) = if hears == "alpha" {
            (&a, b.host.local_addr(), b_id)
        } else {
            (&b, a.host.local_addr(), a_id)
        };
        hearer.host.verified_advert_for_test(&target, heard_addr);
        wait_until("no longer on reach", || {
            a.host.reach_paths().is_empty() && b.host.reach_paths().is_empty()
        })
        .await;
        // The session works on the new path and nobody saw it go offline.
        assert!(a
            .host
            .send_clipboard(&b_id, "back on the LAN", false)
            .unwrap());
        b.events
            .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
            .await;
        for (name, h) in [("alpha", &mut a), ("bravo", &mut b)] {
            h.events.drain();
            let offline = h
                .events
                .seen
                .iter()
                .filter(|e| matches!(e, HostEvent::PeerOffline { .. }))
                .count();
            assert_eq!(offline, 0, "{name} saw an offline, advert heard by {hears}");
        }
        assert!(a.host.is_online(&b_id) && b.host.is_online(&a_id));
        // And the address is a real one now, stored as last-known by the side that dialled.
        let lan_known = [(&a, b_id), (&b, a_id)].iter().any(|(h, p)| {
            h.host
                .peers()
                .iter()
                .find(|s| s.device_id == p.to_hex())
                .is_some_and(|s| {
                    s.last_known.first().is_some_and(|x| !is_reach_addr(*x))
                        && s.last_known.iter().all(|x| !is_reach_addr(*x))
                })
        });
        assert!(lan_known, "advert heard by {hears}");
        a.host.shutdown().await;
        b.host.shutdown().await;
    }
}

/// R4: the normal busy rule holds on the way back: while a send runs on the relay, a LAN path
/// appearing does not move it, and once the send is done the session moves.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_a_running_send_stays_put_until_it_is_done() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, mut b) = online_over_reach(root.path(), &r).await;
    let b_id = b.host.device_id();
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("busy.bin");
    write_random_file(&file, 128 * 1024 * 1024, 0x12_0005);
    a.events.clear();
    let handle = a.host.send(&b_id, vec![file]).await.unwrap();
    let id = handle.id().to_owned();
    wait_until("bytes moving", || handle.bytes_sent() > 1024 * 1024).await;

    a.host.verified_advert_for_test(&b_id, b.host.local_addr());
    tokio::time::sleep(Duration::from_secs(1)).await;
    a.events.drain();
    let finished = a
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::TransferComplete { id: i, .. } if *i == id));
    assert!(!finished, "the send outran the check; make the file bigger");
    assert_eq!(a.host.reach_paths().len(), 1, "moved during a send");

    complete(&mut a, &id, TransferDirection::Send).await;
    complete(&mut b, &id, TransferDirection::Receive).await;
    wait_until("back on the LAN after the send", || {
        a.host.reach_paths().is_empty()
    })
    .await;
    a.events.drain();
    let offline = a
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::PeerOffline { .. }));
    assert!(!offline, "no offline on the way back");
}

/// R4: the same on the side that receives and dialled nothing: while a transfer comes in over
/// reach it does not move to the LAN, and it does once the transfer is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn r4_a_receiving_host_stays_put_until_the_transfer_is_done() {
    init_logs();
    let root = temp_root();
    let r = relay().await;
    let (mut a, mut b) = online_alpha_dialled(root.path(), &r).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("busy.bin");
    write_random_file(&file, 128 * 1024 * 1024, 0x12_0007);
    b.events.clear();
    let handle = a.host.send(&b_id, vec![file]).await.unwrap();
    let id = handle.id().to_owned();
    wait_until("bytes moving", || handle.bytes_sent() > 8 * 1024 * 1024).await;

    b.host.verified_advert_for_test(&a_id, a.host.local_addr());
    tokio::time::sleep(Duration::from_secs(1)).await;
    b.events.drain();
    let finished = b
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::TransferComplete { id: i, .. } if *i == id));
    assert!(
        !finished,
        "the transfer outran the check; make the file bigger"
    );
    assert_eq!(b.host.reach_paths().len(), 1, "moved while receiving");

    complete(&mut a, &id, TransferDirection::Send).await;
    complete(&mut b, &id, TransferDirection::Receive).await;
    wait_until("back on the LAN after the transfer", || {
        b.host.reach_paths().is_empty()
    })
    .await;
    wait_until("the check reports the move", || {
        b.host
            .path_reports()
            .iter()
            .any(|p| p.outcome == Some(PathOutcome::Switched))
    })
    .await;
    b.events.drain();
    let offline = b
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::PeerOffline { .. }));
    assert!(!offline, "no offline on the way back");
}
