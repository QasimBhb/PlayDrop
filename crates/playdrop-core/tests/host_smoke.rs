//! Smoke test of the host over loopback: SAS pairing, a mixed send (streamed files plus
//! bundle), ping, grants, unpair (protocol.md 5, 6, 7).

mod common;

use std::time::Duration;

use common::{pair, start_host, temp_root, write_random_file};
use playdrop_core::host::HostEvent;
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
    b.events
        .wait_for(Duration::from_secs(10), |e| {
            matches!(e, HostEvent::TransferComplete { .. })
        })
        .await;
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
