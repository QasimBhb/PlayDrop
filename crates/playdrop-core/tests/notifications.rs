//! Notification mirroring over loopback (protocol.md 5.16, SR-41 to SR-44): nothing leaves
//! the phone without its `notificationsOut` grant; with it, `posted`, a replacing `posted`,
//! `removed` (also after the grant went off, SR-42) and the desktop's `dismiss` arrive as
//! events; a desktop with
//! `notificationsIn` off drops them; a phone with `notificationsOut` off ignores `dismiss`;
//! a connection carries at most 30 `posted`, 400 `removed` and 60 `dismiss` frames a
//! minute; each host sends and handles only its role's ops and announces the capability
//! only with a role (protocol.md section 4), which still works with a 0.10.0 peer that
//! listed it in both lists and handled every op; and an over-limit field is answered with
//! `bad_request`.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{pair, start_host_with, temp_root, TestHost};
use playdrop_core::host::{
    HostEvent, MirroredNotification, NotificationRole, NOTIFICATIONS_DISMISS_PER_MINUTE,
    NOTIFICATIONS_REMOVED_PER_MINUTE,
};
use playdrop_core::identity::DeviceId;
use playdrop_core::pin_store::Grants;
use playdrop_core::session::types::{Frame, Notification, NotificationPosted};
use playdrop_core::Error;

const T: Duration = Duration::from_secs(20);

/// A host with notification role `role`.
async fn start_role(root: &Path, name: &str, role: NotificationRole) -> TestHost {
    start_host_with(root, name, 0, move |cfg| cfg.notifications = role).await
}

/// Whether `h` lists `notifications` in the `in` list of its only peer.
fn peer_lists_notifications(h: &TestHost, peer: &DeviceId) -> bool {
    h.host
        .peers()
        .into_iter()
        .find(|p| p.device_id == peer.to_hex())
        .expect("peer known")
        .capabilities
        .iter()
        .any(|c| c == "notifications")
}

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
    let mut phone = start_role(root.path(), "phone", NotificationRole::Source).await;
    let mut desk = start_role(root.path(), "desk", NotificationRole::Sink).await;
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
    // `removed` has its own, much higher limit.
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

/// Each role sends only its own ops and drops the other role's; a host with no role lists
/// no `notifications` and neither sends nor handles any (protocol.md section 4, 5.16).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted exchange, read top to bottom"
)]
async fn roles_announce_send_and_handle_only_their_own_ops() {
    common::init_logs();
    let root = temp_root();
    let mut phone = start_role(root.path(), "phone", NotificationRole::Source).await;
    let mut desk = start_role(root.path(), "desk", NotificationRole::Sink).await;
    let mut cli = start_role(root.path(), "cli", NotificationRole::Off).await;
    pair(&mut phone, &mut desk).await;
    // The first pairing's prompts must not answer the second one's waits.
    phone.events.clear();
    pair(&mut cli, &mut phone).await;
    let (phone_id, desk_id, cli_id) = (
        phone.host.device_id(),
        desk.host.device_id(),
        cli.host.device_id(),
    );
    // Both mirror in every direction the grants allow, so only the role decides below.
    for (h, peer) in [(&phone, &desk_id), (&desk, &phone_id), (&phone, &cli_id)] {
        set(h, peer, |g| g.notifications_out = true).await;
    }
    set(&cli, &phone_id, |g| g.notifications_out = true).await;

    // Announced only with a role.
    assert!(peer_lists_notifications(&desk, &phone_id));
    assert!(peer_lists_notifications(&phone, &desk_id));
    assert!(!peer_lists_notifications(&phone, &cli_id));
    assert!(peer_lists_notifications(&cli, &phone_id));

    let key = phone.host.notification_key("0|role|1|null|10123");

    // A host never sends the other role's ops, nor any op without a role.
    for r in [
        desk.host
            .send_notification(&phone_id, note(&key, "from desk")),
        desk.host.send_notification_removed(&phone_id, key.clone()),
        phone.host.send_notification_dismiss(&desk_id, key.clone()),
        cli.host
            .send_notification(&phone_id, note(&key, "from cli")),
        cli.host.send_notification_removed(&phone_id, key.clone()),
        cli.host.send_notification_dismiss(&phone_id, key.clone()),
    ] {
        assert!(matches!(r, Err(Error::Forbidden(_))), "{r:?}");
    }
    // A source sends nothing to a peer that did not announce the capability.
    assert!(matches!(
        phone.host.send_notification(&cli_id, note(&key, "to cli")),
        Err(Error::Forbidden(_))
    ));

    // What arrives anyway (a peer that is not PlayDrop, or a bug) is dropped in silence.
    let posted = |title: &str| {
        Frame::Notification(Notification::Posted(NotificationPosted {
            key: key.clone(),
            app: "Signal".into(),
            title: title.into(),
            text: String::new(),
            when: 1,
        }))
    };
    phone.events.clear();
    desk.events.clear();
    cli.events.clear();
    assert!(desk
        .host
        .send_frame_for_test(&phone_id, posted("wrong way"), None));
    assert!(desk.host.send_frame_for_test(
        &phone_id,
        Frame::Notification(Notification::Removed { key: key.clone() }),
        None
    ));
    assert!(phone.host.send_frame_for_test(
        &desk_id,
        Frame::Notification(Notification::Dismiss { key: key.clone() }),
        None
    ));
    assert!(phone
        .host
        .send_frame_for_test(&cli_id, posted("to no role"), None));
    assert!(phone.host.send_frame_for_test(
        &cli_id,
        Frame::Notification(Notification::Dismiss { key: key.clone() }),
        None
    ));
    barrier(&desk, &mut phone, "after-wrong-way").await;
    barrier(&phone, &mut desk, "after-wrong-way").await;
    barrier(&phone, &mut cli, "after-no-role").await;
    let shown = |h: &TestHost| {
        h.events.seen.iter().any(|e| {
            matches!(
                e,
                HostEvent::NotificationPosted { .. }
                    | HostEvent::NotificationRemoved { .. }
                    | HostEvent::NotificationDismiss { .. }
            )
        })
    };
    assert!(!shown(&phone) && !shown(&desk) && !shown(&cli));
    // Dropped without an answer: the sender is not told to try again.
    for h in [&mut phone, &mut desk] {
        h.events.drain();
        assert!(!h
            .events
            .seen
            .iter()
            .any(|e| matches!(e, HostEvent::PeerRefused { .. })));
    }

    phone.host.shutdown().await;
    desk.host.shutdown().await;
    cli.host.shutdown().await;
}

/// A 0.10.0 host listed `notifications` in both lists and sent and handled every op. A new
/// source or sink announces exactly that, so such a peer keeps working in both roles:
/// emulated here by a host of the other role sending its frames as they are.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_peer_from_before_the_roles_works_in_both_roles() {
    common::init_logs();
    let root = temp_root();
    // An old phone (it lists both, like a sink does) and a new desktop.
    let mut old_phone = start_role(root.path(), "old-phone", NotificationRole::Sink).await;
    let mut desk = start_role(root.path(), "desk", NotificationRole::Sink).await;
    pair(&mut old_phone, &mut desk).await;
    let (old_phone_id, desk_id) = (old_phone.host.device_id(), desk.host.device_id());
    assert!(peer_lists_notifications(&desk, &old_phone_id));
    let key = old_phone.host.notification_key("0|old|1|null|10123");
    assert!(old_phone.host.send_frame_for_test(
        &desk_id,
        Frame::Notification(Notification::Posted(NotificationPosted {
            key: key.clone(),
            app: "Signal".into(),
            title: "from 0.10.0".into(),
            text: "hi".into(),
            when: 1,
        })),
        None
    ));
    desk.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::NotificationPosted { notification, .. }
                if notification.title == "from 0.10.0")
        })
        .await;
    assert!(old_phone.host.send_frame_for_test(
        &desk_id,
        Frame::Notification(Notification::Removed { key: key.clone() }),
        None
    ));
    desk.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::NotificationRemoved { key: k, .. } if *k == key),
        )
        .await;
    // The new desktop sends its dismiss to the old phone, which lists the capability in
    // `in`. (The host playing the old phone is a sink, which drops a dismiss; 0.10.0 acted
    // on it, so only the sending side is checked here.)
    desk.host
        .send_notification_dismiss(&old_phone_id, key.clone())
        .unwrap();

    // A new phone and an old desktop (it lists both, like a source does).
    let mut phone = start_role(root.path(), "phone", NotificationRole::Source).await;
    let mut old_desk = start_role(root.path(), "old-desk", NotificationRole::Source).await;
    pair(&mut phone, &mut old_desk).await;
    let (phone_id, old_desk_id) = (phone.host.device_id(), old_desk.host.device_id());
    assert!(peer_lists_notifications(&phone, &old_desk_id));
    set(&phone, &old_desk_id, |g| g.notifications_out = true).await;
    let key = phone.host.notification_key("0|new|1|null|10123");
    // The old desktop shows what the new phone sends: here a source host plays it, so the
    // frame is checked at the wire, where the old desktop would have taken it.
    phone
        .host
        .send_notification(&old_desk_id, note(&key, "to 0.10.0"))
        .unwrap();
    // The old desktop's dismiss reaches the new phone.
    assert!(old_desk.host.send_frame_for_test(
        &phone_id,
        Frame::Notification(Notification::Dismiss { key: key.clone() }),
        None
    ));
    phone
        .events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::NotificationDismiss { key: k, .. } if *k == key),
        )
        .await;

    for h in [old_phone, desk, phone, old_desk] {
        h.host.shutdown().await;
    }
}

/// `removed` and `dismiss` have their own per-connection limits (SR-44), each high enough
/// for any real burst: 400 and 60 a minute.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn removed_and_dismiss_are_rate_limited_per_connection() {
    common::init_logs();
    let root = temp_root();
    let mut phone = start_role(root.path(), "phone", NotificationRole::Source).await;
    let mut desk = start_role(root.path(), "desk", NotificationRole::Sink).await;
    pair(&mut phone, &mut desk).await;
    let (phone_id, desk_id) = (phone.host.device_id(), desk.host.device_id());
    set(&phone, &desk_id, |g| g.notifications_out = true).await;

    // The Android app clears up to 200 copies at once: that burst and more all arrive.
    let extra = 25;
    for i in 0..NOTIFICATIONS_REMOVED_PER_MINUTE + extra {
        let k = phone.host.notification_key(&format!("removed {i}"));
        phone.host.send_notification_removed(&desk_id, k).unwrap();
    }
    barrier(&phone, &mut desk, "after-removed").await;
    let removed = desk
        .events
        .seen
        .iter()
        .filter(|e| matches!(e, HostEvent::NotificationRemoved { .. }))
        .count();
    assert_eq!(removed, NOTIFICATIONS_REMOVED_PER_MINUTE);

    for i in 0..NOTIFICATIONS_DISMISS_PER_MINUTE + extra {
        let k = desk.host.notification_key(&format!("dismiss {i}"));
        desk.host.send_notification_dismiss(&phone_id, k).unwrap();
    }
    barrier(&desk, &mut phone, "after-dismiss").await;
    let dismissed = phone
        .events
        .seen
        .iter()
        .filter(|e| matches!(e, HostEvent::NotificationDismiss { .. }))
        .count();
    assert_eq!(dismissed, NOTIFICATIONS_DISMISS_PER_MINUTE);

    phone.host.shutdown().await;
    desk.host.shutdown().await;
}

/// A field over its limit (5.16) is refused with `error` `bad_request`, which names the
/// frame, and the notification is not shown.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_field_over_its_limit_is_refused_with_bad_request() {
    common::init_logs();
    let root = temp_root();
    let mut phone = start_role(root.path(), "phone", NotificationRole::Source).await;
    let mut desk = start_role(root.path(), "desk", NotificationRole::Sink).await;
    pair(&mut phone, &mut desk).await;
    let desk_id = desk.host.device_id();
    let key = phone.host.notification_key("0|big|1|null|10123");
    let too_long = [
        ("a".repeat(65), String::new(), String::new()),
        ("A".into(), "t".repeat(257), String::new()),
        ("A".into(), String::new(), "x".repeat(1025)),
    ];
    for (app, title, text) in too_long {
        phone.events.clear();
        assert!(phone.host.send_frame_for_test(
            &desk_id,
            Frame::Notification(Notification::Posted(NotificationPosted {
                key: key.clone(),
                app,
                title,
                text,
                when: 1,
            })),
            None
        ));
        let ev = phone
            .events
            .wait_for(T, |e| matches!(e, HostEvent::PeerRefused { .. }))
            .await;
        let HostEvent::PeerRefused {
            code, frame_type, ..
        } = ev
        else {
            unreachable!()
        };
        assert_eq!(code, "bad_request");
        assert_eq!(frame_type.as_deref(), Some("notification"));
    }
    barrier(&phone, &mut desk, "after-big").await;
    assert_eq!(posted_titles(&desk), Vec::<String>::new());

    phone.host.shutdown().await;
    desk.host.shutdown().await;
}
