//! A clipboard push to chosen peers (`Host::push_clipboard_to`,
//! `Host::push_clipboard_image_to`; protocol.md 5.7, Phase 16): the chosen peers get it and
//! the others do not, the echo rule runs once for the whole push and only against what was
//! received (a clip sent to one device can then be sent to another), the sensitive flag
//! reaches every chosen peer, a peer that is not paired is reported for itself, and a peer
//! that cannot take images is counted for itself while the rest still get the image.

mod common;

use std::time::Duration;

use common::{pair, start_host, temp_root, TestHost};
use playdrop_core::host::HostEvent;
use playdrop_core::identity::DeviceId;
use playdrop_core::session::types::{CapabilitiesFrame, Capability, Caps, Frame};

const T: Duration = Duration::from_secs(20);

/// Sends a ping from `from` and waits until `to` has it: frames on one session arrive in
/// order, so everything `from` sent before has been handled by `to` too.
async fn barrier(from: &TestHost, to: &mut TestHost, tag: &str) {
    from.host
        .ping(&to.host.device_id(), Some(tag))
        .expect("ping");
    to.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::Ping { message: Some(m), .. } if m == tag),
        )
        .await;
}

/// Text clips `h` received, with their sensitive flag.
fn clips(h: &TestHost) -> Vec<(String, bool)> {
    h.events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::Clipboard {
                text, sensitive, ..
            } => Some((text.0.clone().unwrap_or_default(), *sensitive)),
            _ => None,
        })
        .collect()
}

fn images(h: &TestHost) -> usize {
    h.events
        .seen
        .iter()
        .filter(|e| matches!(e, HostEvent::ClipboardImage { .. }))
        .count()
}

/// `a` paired with three peers, each with `clipboardOut` on for it.
async fn four_hosts(root: &std::path::Path) -> (TestHost, Vec<TestHost>) {
    let mut a = start_host(root, "a", 0).await;
    let mut peers = Vec::new();
    for name in ["b", "c", "d"] {
        let mut p = start_host(root, name, 0).await;
        // The last pairing's prompts must not answer this one's waits.
        a.events.clear();
        pair(&mut a, &mut p).await;
        let id = p.host.device_id();
        let mut g = a.host.grants(&id).unwrap();
        g.clipboard_out = true;
        a.host.set_grants(&id, g).await.unwrap();
        peers.push(p);
    }
    (a, peers)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn text_goes_to_the_chosen_peers_once_and_sensitive() {
    common::init_logs();
    let root = temp_root();
    let (mut a, mut peers) = four_hosts(root.path()).await;
    let a_id = a.host.device_id();
    let ids: Vec<DeviceId> = peers.iter().map(|p| p.host.device_id()).collect();
    for p in &mut peers {
        p.events.clear();
    }

    // Two of three, plus one device that is not paired: the two get it, the third does
    // not, and the stranger is reported for itself.
    let stranger = DeviceId::from_spki_hash([9; 32]);
    let r = a
        .host
        .push_clipboard_to(&[ids[0], ids[1], stranger, ids[0]], "pick me", true)
        .unwrap();
    let mut sent = r.sent_to.clone();
    sent.sort_by_key(DeviceId::to_hex);
    let mut want = vec![ids[0], ids[1]];
    want.sort_by_key(DeviceId::to_hex);
    assert_eq!(sent, want);
    assert!(!r.skipped_echo);
    assert_eq!(r.skipped_not_paired, vec![stranger]);
    for (i, p) in peers.iter_mut().enumerate() {
        barrier(&a, p, "after-push").await;
        let got = clips(p);
        if i < 2 {
            // The sensitive flag reaches every chosen peer.
            assert_eq!(got, vec![("pick me".to_owned(), true)], "peer {i}");
        } else {
            assert!(got.is_empty(), "peer {i} was not chosen");
        }
    }

    // The same clip to another chosen device is what the user asked for, not an echo.
    let again = a
        .host
        .push_clipboard_to(&[ids[2]], "pick me", true)
        .unwrap();
    assert!(!again.skipped_echo);
    assert_eq!(again.sent_to, vec![ids[2]]);
    barrier(&a, &mut peers[2], "after-again").await;
    assert_eq!(clips(&peers[2]), vec![("pick me".to_owned(), true)]);
    // The automatic path still counts it as sent.
    assert!(a.host.push_clipboard("pick me", true).unwrap().skipped_echo);

    // A clip received from a peer is never forwarded, to chosen devices either.
    let mut g = peers[2].host.grants(&a_id).unwrap();
    g.clipboard_out = true;
    peers[2].host.set_grants(&a_id, g).await.unwrap();
    peers[2]
        .host
        .push_clipboard_to(&[a_id], "from d", false)
        .unwrap();
    a.events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
    let back = a
        .host
        .push_clipboard_to(&[ids[0]], "from d", false)
        .unwrap();
    assert!(back.skipped_echo && back.sent_to.is_empty());

    // An empty choice sends nothing and does not become "last sent".
    let none = a.host.push_clipboard_to(&[], "fresh", false).unwrap();
    assert!(none.sent_to.is_empty() && !none.skipped_echo);
    let fresh = a.host.push_clipboard_to(&[ids[2]], "fresh", false).unwrap();
    assert_eq!(fresh.sent_to, vec![ids[2]]);

    a.host.shutdown().await;
    for p in peers {
        p.host.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_image_skips_only_the_peer_that_cannot_take_it() {
    common::init_logs();
    let root = temp_root();
    let (mut a, mut peers) = four_hosts(root.path()).await;
    let a_id = a.host.device_id();
    let ids: Vec<DeviceId> = peers.iter().map(|p| p.host.device_id()).collect();

    // Peer b announces lists without `clipboard.image`, as a build without image support.
    let caps = Caps {
        incoming: vec![
            Capability::Ping,
            Capability::Clipboard,
            Capability::Blobs,
            Capability::Transfer,
        ],
        outgoing: vec![Capability::Ping, Capability::Clipboard],
    };
    assert!(peers[0].host.send_frame_for_test(
        &a_id,
        Frame::Capabilities(CapabilitiesFrame { caps }),
        None
    ));
    barrier(&peers[0], &mut a, "after-caps").await;
    for p in &mut peers {
        p.events.clear();
    }

    let png = vec![0x89u8; 50_000];
    let r = a
        .host
        .push_clipboard_image_to(&[ids[0], ids[1]], png, "image/png", false)
        .unwrap();
    assert_eq!(r.sent_to, vec![ids[1]]);
    assert_eq!(r.skipped_unsupported, 1);
    assert_eq!(r.skipped_not_paired, Vec::<DeviceId>::new());
    peers[1]
        .events
        .wait_for(T, |e| matches!(e, HostEvent::ClipboardImage { .. }))
        .await;
    for p in [0, 2] {
        barrier(&a, &mut peers[p], "after-image").await;
        assert_eq!(images(&peers[p]), 0, "peer {p}");
    }

    // A bad image fails the call whoever is chosen.
    assert!(a
        .host
        .push_clipboard_image_to(&[], vec![1, 2, 3], "text/plain", false)
        .is_err());

    a.host.shutdown().await;
    for p in peers {
        p.host.shutdown().await;
    }
}
