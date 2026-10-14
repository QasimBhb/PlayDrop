//! Smoke test of the host over loopback: SAS pairing, a mixed send (streamed files plus
//! bundle), ping, grants, unpair (protocol.md 5, 6, 7).

mod common;

use std::time::Duration;

use common::{pair, start_host, temp_root, write_random_file};
use playdrop_core::host::{HostEvent, TransferDirection, TransferStatus};
use playdrop_core::net::BodyReader;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pair_send_ping_unpair() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;

    let src = root.path().join("src").join("Holiday");
    std::fs::create_dir_all(src.join("empty")).unwrap();
    write_random_file(&src.join("big.bin"), 5 * 1024 * 1024 + 123, 7);
    write_random_file(&src.join("small1.txt"), 1000, 8);
    write_random_file(&src.join("small2.txt"), 70_000, 9);
    std::fs::write(src.join("zero.txt"), b"").unwrap();

    let peer = b.host.device_id();
    let mut handle = a.host.send(&peer, vec![src.clone()]).await.unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(60), handle.wait())
        .await
        .expect("send finished in time")
        .expect("send succeeded");
    assert_eq!(summary.bytes, 5 * 1024 * 1024 + 123 + 1000 + 70_000);
    let done = b
        .events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::TransferComplete { .. })
        })
        .await;
    let HostEvent::TransferComplete {
        direction,
        saved,
        folder,
        roots,
        ..
    } = done
    else {
        unreachable!()
    };
    assert_eq!(direction, TransferDirection::Receive);
    assert_eq!(folder.as_deref(), Some(b.recv_dir.as_path()));
    let mut saved = saved;
    saved.sort();
    let mut want: Vec<_> = ["big.bin", "small1.txt", "small2.txt", "zero.txt"]
        .iter()
        .map(|n| b.recv_dir.join("Holiday").join(n))
        .collect();
    want.sort();
    assert_eq!(
        saved, want,
        "every received file is reported where it landed"
    );
    assert_eq!(roots.len(), 4);
    let big_root = roots.iter().find(|r| r.path == "Holiday/big.bin").unwrap();
    assert_eq!(
        big_root.root,
        summary
            .roots
            .iter()
            .find(|r| r.path == "Holiday/big.bin")
            .unwrap()
            .root,
        "both sides report the same verified root"
    );
    for (host, dir) in [
        (&a.host, TransferDirection::Send),
        (&b.host, TransferDirection::Receive),
    ] {
        let list = host.transfers();
        let t = list.iter().find(|t| t.id == summary.id).unwrap();
        assert_eq!(t.direction, dir);
        assert_eq!(t.state, TransferStatus::Complete);
        assert_eq!(t.bytes, summary.bytes);
        assert_eq!(t.file_count, 4);
        // A late UI sees where received files landed; senders report none.
        let got: std::collections::BTreeSet<_> = t.saved.iter().collect();
        let receive = dir == TransferDirection::Receive;
        assert_eq!(got.len(), if receive { 4 } else { 0 });
        assert!(got.iter().all(|p| want.contains(p)));
        assert_eq!(t.folder.is_some(), receive);
    }
    for name in ["big.bin", "small1.txt", "small2.txt", "zero.txt"] {
        let want = std::fs::read(src.join(name)).unwrap();
        let got = std::fs::read(b.recv_dir.join("Holiday").join(name)).unwrap();
        assert!(want == got, "{name} differs");
    }
    assert!(b.recv_dir.join("Holiday").join("empty").is_dir());

    a.host.ping(&peer, Some("hi")).unwrap();
    b.events
        .wait_for(
            Duration::from_secs(5),
            |e| matches!(e, HostEvent::Ping { message: Some(m), .. } if m == "hi"),
        )
        .await;

    a.host.unpair(&peer).await.unwrap();
    b.events
        .wait_for(Duration::from_secs(5), |e| {
            matches!(e, HostEvent::Unpaired { .. })
        })
        .await;
    assert_eq!(a.host.peers().len(), 0);
    assert_eq!(b.host.peers().len(), 0);

    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// SR-8: the last-seen time is written to the pin, so it is still shown after a restart
/// while the peer is offline.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn last_seen_survives_restart() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    a.events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::PeerOnline { .. })
        })
        .await;
    assert!(b.host.peers()[0].last_seen.is_some());
    b.host.shutdown().await;
    a.events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::PeerOffline { .. })
        })
        .await;
    let before = a.host.peers()[0].last_seen.expect("seen while paired");
    // No pause: shutdown itself writes the newest time to the pin.
    let a = a.restart(root.path()).await;
    let p = &a.host.peers()[0];
    assert!(!p.online);
    assert_eq!(p.last_seen, Some(before));
    a.host.shutdown().await;
}

/// SR-13: an unpaired connection reaches only the session stream (`403 not_paired`), and a
/// transfer ID is bound to the device that offered it: another paired device gets `404`
/// (protocol.md 6.1, 7.1).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names, reason = "a, b, c are the three hosts")]
async fn requests_are_bound_to_the_tls_device() {
    use playdrop_core::identity::DeviceIdentity;
    use playdrop_core::net::{dial, DialOptions};
    use playdrop_core::tls;

    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    let mut c = start_host(root.path(), "charlie", 0).await;
    pair(&mut a, &mut b).await;
    b.events.clear();
    pair(&mut c, &mut b).await;

    // Offers from A wait for the user on B, so the ID stays known.
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();
    let src = root.path().join("doc.bin");
    write_random_file(&src, 4096, 1);
    let _handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let offer = b
        .events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::TransferOffer { .. })
        })
        .await;
    let HostEvent::TransferOffer { id, .. } = offer else {
        unreachable!()
    };

    // C, a paired device, dials B with its own key and asks about A's transfer.
    let c_identity = DeviceIdentity::load_or_create(&c.data_dir).unwrap();
    let b_spki = *b.host.device_id().spki_hash();
    let cfg = tls::build_client_config(&c_identity, b_spki);
    let conn = dial(&[b.host.local_addr()], &DialOptions::new(cfg, Some(b_spki)))
        .await
        .unwrap();
    let path = format!("/v1/transfers/{id}");
    let (s, body) = status_of(&conn, "GET", &path).await;
    assert_eq!(s, 404, "{body}");
    assert!(body.contains("not_found"));
    let (s, _) = status_of(&conn, "DELETE", &path).await;
    assert_eq!(s, 404);

    // An unknown device in "Add device" mode reaches only the session stream.
    b.host.open_add_device_window().unwrap();
    let stranger = DeviceIdentity::generate().unwrap();
    let cfg = tls::build_client_config_unpaired(&stranger);
    let conn = dial(&[b.host.local_addr()], &DialOptions::new(cfg, None))
        .await
        .unwrap();
    let (s, body) = status_of(&conn, "GET", &path).await;
    assert_eq!(s, 403, "{body}");
    assert!(body.contains("not_paired"));
    b.host.close_add_device_window();

    // A itself still sees its offer as pending.
    b.host.respond_offer(&id, false).await.unwrap();
    a.host.shutdown().await;
    b.host.shutdown().await;
    c.host.shutdown().await;
}

/// Sends a bodiless request and returns the status and body text.
async fn status_of(
    conn: &playdrop_core::net::Connection,
    method: &str,
    path: &str,
) -> (u16, String) {
    let req = http::Request::builder()
        .method(method)
        .uri(path)
        .body(())
        .unwrap();
    let (resp, _send) = conn.request(req, true).await.unwrap();
    let resp = resp.await.unwrap();
    let status = resp.status().as_u16();
    let body = BodyReader::new(resp.into_body())
        .read_to_limit(65536)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&body).into_owned())
}

/// QR pairing (protocol.md 6.3, SR-9): the displayer confirms the scanner with one tap, and
/// the one-time secret cannot be reused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn qr_pairing_and_single_use_secret() {
    use playdrop_core::host::PairingPromptKind;

    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "displayer", 0).await;
    let mut b = start_host(root.path(), "scanner", 0).await;
    let uri = a.host.show_qr().unwrap();
    assert!(uri.starts_with("playdrop://pair?id="));
    b.host.scan_qr(&uri).await.unwrap();
    let t = Duration::from_secs(20);
    a.events
        .wait_for(t, |e| {
            matches!(
                e,
                HostEvent::PairingPrompt {
                    prompt: PairingPromptKind::ScannerConfirm,
                    ..
                }
            )
        })
        .await;
    a.host.pairing_respond(true);
    a.events
        .wait_for(t, |e| matches!(e, HostEvent::Paired { .. }))
        .await;
    b.events
        .wait_for(t, |e| matches!(e, HostEvent::Paired { .. }))
        .await;
    assert_eq!(a.host.peers().len(), 1);
    assert_eq!(b.host.peers().len(), 1);

    // The secret was used up: a third device scanning the same code is refused.
    let mut c = start_host(root.path(), "late", 0).await;
    let refused = c.host.scan_qr(&uri).await;
    if refused.is_ok() {
        c.events
            .wait_for(t, |e| matches!(e, HostEvent::PairingFailed { .. }))
            .await;
    }
    assert_eq!(c.host.peers().len(), 0);
    a.host.shutdown().await;
    b.host.shutdown().await;
    c.host.shutdown().await;
}

/// Gap 10 and gap 2: an offer lists its file names, both sides list it as `offered`, and
/// the receiver can cancel it by ID; the sender then sees a cancel, not a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names, reason = "a, b are the hosts")]
async fn offer_names_and_cancel_by_id() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();

    let src = root.path().join("src").join("Docs");
    std::fs::create_dir_all(&src).unwrap();
    write_random_file(&src.join("one.pdf"), 4096, 1);
    write_random_file(&src.join("two.txt"), 10, 2);
    let mut handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let offer = b
        .events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::TransferOffer { .. })
        })
        .await;
    let HostEvent::TransferOffer {
        id,
        files,
        file_count,
        ..
    } = offer
    else {
        unreachable!()
    };
    assert_eq!(file_count, 2);
    let mut names: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["Docs/one.pdf", "Docs/two.txt"]);
    let b_list = b.host.transfers();
    assert_eq!(b_list.len(), 1);
    assert_eq!(b_list[0].state, TransferStatus::Offered);
    assert_eq!(b_list[0].peer, a_id.to_hex());
    assert_eq!(a.host.transfers()[0].state, TransferStatus::Offered);

    b.host.cancel_transfer(&id).await.unwrap();
    assert!(
        b.host.cancel_transfer(&id).await.is_err(),
        "already cancelled"
    );
    let r = tokio::time::timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("sender stopped in time");
    assert!(r.is_err());
    b.events
        .wait_for(Duration::from_secs(5), |e| {
            matches!(e, HostEvent::TransferFailed { .. })
        })
        .await;
    assert_eq!(b.host.transfers()[0].state, TransferStatus::Cancelled);
    assert_eq!(a.host.transfers()[0].state, TransferStatus::Cancelled);
    assert!(b.recv_dir.read_dir().map_or(true, |mut d| d
        .all(|e| e.unwrap().file_name() == ".playdrop-partial")));

    // A sender-side cancel by ID of a new pending offer.
    let other = root.path().join("src").join("x.bin");
    write_random_file(&other, 100, 3);
    let mut handle = a.host.send(&b.host.device_id(), vec![other]).await.unwrap();
    let id = handle.id().to_owned();
    a.host.cancel_transfer(&id).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(10), handle.wait())
        .await
        .expect("sender stopped in time");
    assert!(r.is_err());
    let t = a.host.transfers().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(t.state, TransferStatus::Cancelled);
    assert!(a.host.cancel_transfer("nope").await.is_err());

    a.host.shutdown().await;
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Gap 5: one clipboard change goes to every online peer with `clipboardOut`, the echo
/// rule applies once, a push that reached nobody does not suppress the next one, and text
/// over 1 MiB is refused (protocol.md 5.7, SR-24).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names, reason = "a, b are the hosts")]
async fn clipboard_push_to_all_with_echo_rule() {
    use playdrop_core::Error;

    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let (a_id, b_id) = (a.host.device_id(), b.host.device_id());
    let mut g = a.host.grants(&b_id).unwrap();
    g.clipboard_out = false;
    a.host.set_grants(&b_id, g).await.unwrap();
    let mut g = b.host.grants(&a_id).unwrap();
    g.clipboard_in = true;
    g.clipboard_out = true;
    b.host.set_grants(&a_id, g).await.unwrap();

    let r = a.host.push_clipboard("hello", false).unwrap();
    assert_eq!(r.skipped_no_grant, 1);
    assert_eq!(r.sent_to, []);

    let mut g = a.host.grants(&b_id).unwrap();
    g.clipboard_out = true;
    a.host.set_grants(&b_id, g).await.unwrap();
    let r = a.host.push_clipboard("hello", false).unwrap();
    assert_eq!(
        r.sent_to,
        [b_id],
        "an unsent clip is not remembered as sent"
    );
    assert!(!r.skipped_echo);
    let ev = b
        .events
        .wait_for(Duration::from_secs(5), |e| {
            matches!(e, HostEvent::Clipboard { .. })
        })
        .await;
    let HostEvent::Clipboard { text, .. } = ev else {
        unreachable!()
    };
    assert_eq!(text.0.as_deref(), Some("hello"));

    assert!(a.host.push_clipboard("hello", false).unwrap().skipped_echo);
    // B applied "hello"; its clipboard watcher then reports the same text: no echo back.
    assert!(b.host.push_clipboard("hello", false).unwrap().skipped_echo);
    assert!(!b.host.send_clipboard(&a_id, "hello", false).unwrap());

    // Over 32 KiB goes as a blob now; over 1 MiB is not synced (SR-24).
    let big = "x".repeat(1024 * 1024 + 1);
    assert!(matches!(
        a.host.push_clipboard(&big, false),
        Err(Error::TooLarge(_))
    ));
    assert!(matches!(
        a.host.send_clipboard(&b_id, &big, false),
        Err(Error::TooLarge(_))
    ));
    assert!(a.host.send_clipboard(&b_id, "world", true).unwrap());

    // Gap 6: diagnostics on demand.
    let d = a.host.diagnose();
    assert!(d.bind_ok);
    assert_eq!(d.port, a.host.local_addr().port());
    assert_eq!(d.device_id, a_id.to_hex());
    assert!(!d.multicast.enabled, "discovery is off in loopback tests");
    assert!(!d.mdns.enabled);
    assert_eq!(d.peers.len(), 1);
    assert!(d.peers[0].online);
    assert_eq!(d.peers[0].peer, b_id.to_hex());
    let json = serde_json::to_string(&d).unwrap();
    let back: playdrop_core::host::Diagnostics = serde_json::from_str(&json).unwrap();
    assert_eq!(back, d);

    b.host.shutdown().await;
    a.events
        .wait_for(Duration::from_secs(5), |e| {
            matches!(e, HostEvent::PeerOffline { .. })
        })
        .await;
    a.host.network_changed();
    assert!(a.host.connect(&b_id).await.is_err());
    let d = a.host.diagnose();
    assert!(!d.peers[0].online);
    let fail = d.peers[0]
        .last_result
        .clone()
        .expect("dial failure recorded");
    assert_eq!(fail.path, playdrop_core::host::DiscoveryPath::LastKnown);
    assert!(fail.at > 0);
    a.host.shutdown().await;
}

/// Free space that answers the offer and accept checks, then fails like a disk that went
/// away, so the receiver's next write fails locally while the body is still arriving.
#[derive(Debug, Default)]
struct FailingDisk {
    calls: std::sync::atomic::AtomicU32,
}

impl playdrop_core::transfer::FreeSpace for FailingDisk {
    fn space(
        &self,
        _path: &std::path::Path,
    ) -> std::io::Result<playdrop_core::transfer::SpaceInfo> {
        // Call 1 checks the offer, call 2 the accept; every later call is a chunk write.
        if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Ok(playdrop_core::transfer::SpaceInfo {
                available: 1 << 50,
                total: 1 << 50,
            })
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "storage refused",
            ))
        }
    }
}

/// A receiver whose storage fails mid-upload answers the `PUT` with an error, so the
/// sender fails within seconds instead of taking the reset for a lost connection and
/// resuming into the same failure forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receiver_storage_failure_fails_the_send_promptly() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = common::start_host_with(root.path(), "bravo", 0, |cfg| {
        cfg.free_space = std::sync::Arc::new(FailingDisk::default());
    })
    .await;
    pair(&mut a, &mut b).await;

    let src = root.path().join("src").join("one.bin");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    write_random_file(&src, 2 * 1024 * 1024, 11);
    let peer = b.host.device_id();
    let mut handle = a.host.send(&peer, vec![src]).await.unwrap();
    let err = tokio::time::timeout(Duration::from_secs(15), handle.wait())
        .await
        .expect("the send ends instead of retrying forever")
        .expect_err("the receiver could not store the file");
    assert!(
        err.to_string().contains("500"),
        "the receiver's answer reaches the sender: {err}"
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}
