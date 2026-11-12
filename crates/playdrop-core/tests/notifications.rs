//! Notification mirroring over loopback (protocol.md 5.16, SR-41 to SR-44): nothing leaves
//! the phone without its `notificationsOut` grant; with it, `posted`, a replacing `posted`,
//! `removed` (also after the grant went off, SR-42) and the desktop's `dismiss` arrive as
//! events; a desktop with
//! `notificationsIn` off drops them; a phone with `notificationsOut` off ignores `dismiss`;
//! and a connection carries at most 30 `posted` frames a minute.

mod common;

use std::time::Duration;

use common::{pair, start_host, temp_root, TestHost};
use playdrop_core::host::{HostEvent, MirroredNotification};
use playdrop_core::identity::DeviceId;
use playdrop_core::pin_store::Grants;
use playdrop_core::Error;

const T: Duration = Duration::from_secs(20);

fn note(key: &str, title: &str) -> MirroredNotification {
    MirroredNotification {
        key: key.to_owned(),
        app: "Signal".into(),
        title: title.into(),
        text: "On my way".into(),
        when_ms: 1_762_900_000_000,
    }
}

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

fn posted_titles(h: &TestHost) -> Vec<String> {
    h.events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::NotificationPosted { notification, .. } => Some(notification.title.clone()),
            _ => None,
        })
        .collect()
}

async fn set(h: &TestHost, peer: &DeviceId, f: impl FnOnce(&mut Grants)) {
    let mut g = h.host.grants(peer).expect("paired");
    f(&mut g);
    h.host.set_grants(peer, g).await.expect("grants stored");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted exchange, read top to bottom"
)]
async fn notifications_need_grants_replace_remove_dismiss_and_rate_limit() {
    common::init_logs();
    let root = temp_root();
    let mut phone = start_host(root.path(), "phone", 0).await;
    let mut desk = start_host(root.path(), "desk", 0).await;
    pair(&mut phone, &mut desk).await;
    let desk_id = desk.host.device_id();
    let phone_id = phone.host.device_id();

    // Defaults after pairing (SR-41): show yes, mirror no, on both sides.
    for (h, peer) in [(&phone, &desk_id), (&desk, &phone_id)] {
        let g = h.host.grants(peer).unwrap();
        assert!(g.notifications_in && !g.notifications_out);
    }
    assert!(desk
        .host
        .peers()
        .remove(0)
        .capabilities
        .iter()
        .any(|c| c == "notifications"));

    let key = phone
        .host
        .notification_key("0|org.thoughtcrime|7|null|10123");
    assert_eq!(
        key,
        phone
            .host
            .notification_key("0|org.thoughtcrime|7|null|10123")
    );
    assert_eq!(key.len(), 32);

    // 1. Without `notificationsOut` nothing is sent.
    let off = phone
        .host
        .send_notification(&desk_id, note(&key, "first"))
        .unwrap_err();
    assert!(matches!(off, Error::Forbidden(_)), "{off:?}");
    barrier(&phone, &mut desk, "after-off").await;
    assert_eq!(posted_titles(&desk), Vec::<String>::new());

    // 2. With it: posted, a replacement with the same key, removed.
    set(&phone, &desk_id, |g| g.notifications_out = true).await;
    // A key that did not come from `notification_key` is refused before sending.
    assert!(matches!(
        phone
            .host
            .send_notification(&desk_id, note("NOT-A-KEY", "bad")),
        Err(Error::BadRequest(_))
    ));
    phone
        .host
        .send_notification(&desk_id, note(&key, "first"))
        .unwrap();
    let ev = desk
        .events
        .wait_for(T, |e| matches!(e, HostEvent::NotificationPosted { .. }))
        .await;
    let HostEvent::NotificationPosted { peer, notification } = ev else {
        unreachable!()
    };
    assert_eq!(peer, phone_id.to_hex());
    assert_eq!(notification, note(&key, "first"));
    phone
        .host
        .send_notification(&desk_id, note(&key, "second"))
        .unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::NotificationPosted { notification, .. }
                if notification.key == key && notification.title == "second")
        })
        .await;
    phone
        .host
        .send_notification_removed(&desk_id, key.clone())
        .unwrap();
    desk.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::NotificationRemoved { key: k, .. } if *k == key),
        )
        .await;

    // 3. Dismiss from the desktop reaches the phone, which holds `notificationsOut`.
    desk.host
        .send_notification_dismiss(&phone_id, key.clone())
        .unwrap();
    let ev = phone
        .events
        .wait_for(T, |e| matches!(e, HostEvent::NotificationDismiss { .. }))
        .await;
    assert!(
        matches!(&ev, HostEvent::NotificationDismiss { peer, key: k } if *peer == desk_id.to_hex() && *k == key)
    );

    // A phone that stopped mirroring ignores a dismiss and says so once.
    set(&phone, &desk_id, |g| g.notifications_out = false).await;
    phone.events.clear();
    let key2 = phone.host.notification_key("0|other|1|null|10123");
    desk.host
        .send_notification_dismiss(&phone_id, key2.clone())
        .unwrap();
    desk.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::PeerRefused { frame_type: Some(t), code, .. }
                if t == "notification" && code == "forbidden")
        })
        .await;
    barrier(&desk, &mut phone, "after-dismiss-off").await;
    assert!(!phone
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::NotificationDismiss { .. })));

    // With the grant off, `removed` still goes, so the desktop closes its copies (SR-42).
    desk.events.clear();
    phone
        .host
        .send_notification_removed(&desk_id, key.clone())
        .unwrap();
    desk.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::NotificationRemoved { key: k, .. } if *k == key),
        )
        .await;
    set(&phone, &desk_id, |g| g.notifications_out = true).await;

    // 4. The desktop with `notificationsIn` off drops what arrives and cannot dismiss.
    set(&desk, &phone_id, |g| g.notifications_in = false).await;
    desk.events.clear();
    phone.events.clear();
    phone
        .host
        .send_notification(&desk_id, note(&key2, "hidden"))
        .unwrap();
    phone
        .host
        .send_notification_removed(&desk_id, key2.clone())
        .unwrap();
    phone
        .events
        .wait_for(T, |e| {
            matches!(e, HostEvent::PeerRefused { frame_type: Some(t), code, .. }
                if t == "notification" && code == "forbidden")
        })
        .await;
    barrier(&phone, &mut desk, "after-in-off").await;
    assert!(!desk.events.seen.iter().any(|e| matches!(
        e,
        HostEvent::NotificationPosted { .. } | HostEvent::NotificationRemoved { .. }
    )));
    assert!(matches!(
        desk.host.send_notification_dismiss(&phone_id, key2.clone()),
        Err(Error::Forbidden(_))
    ));
    set(&desk, &phone_id, |g| g.notifications_in = true).await;

    // 5. At most 30 `posted` a minute per connection (SR-44). Two were handled above
    // ("first", "second"; the hidden one never reached the handler), so of 40 more only
    // the first 28 arrive.
    desk.events.clear();
    for i in 0..40 {
        let k = phone.host.notification_key(&format!("burst {i}"));
        phone
            .host
            .send_notification(&desk_id, note(&k, &format!("burst {i}")))
            .unwrap();
    }
    // `removed` is not rate limited.
    phone
        .host
        .send_notification_removed(&desk_id, key.clone())
        .unwrap();
    desk.events
        .wait_for(T, |e| matches!(e, HostEvent::NotificationRemoved { .. }))
        .await;
    barrier(&phone, &mut desk, "after-burst").await;
    let got = posted_titles(&desk);
    let want: Vec<String> = (0..28).map(|i| format!("burst {i}")).collect();
    assert_eq!(got, want);

    phone.host.shutdown().await;
    desk.host.shutdown().await;
}
