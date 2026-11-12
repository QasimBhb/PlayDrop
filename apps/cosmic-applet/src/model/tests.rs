// `assert!(x.is_empty())` reads better here than comparing with a typed empty array.
#![allow(clippy::assert_is_empty)]

use std::path::PathBuf;

use playdrop_ipc::types::{FileRoot, Grants, OfferedFile, PeerBattery};
use playdrop_ipc::{ConsentToken, Settings, UpdateOffer, UpdateState, UpdateStatus};

use super::*;

const NOW: u64 = 1_800_000_000;

fn id(n: u8) -> String {
    format!("{n:02x}").repeat(32)
}

fn peer(n: u8, label: &str, online: bool) -> PeerSummary {
    PeerSummary {
        device_id: id(n),
        short_id: format!("S{n}"),
        label: label.to_owned(),
        online,
        grants: Grants::default(),
        last_known: Vec::new(),
        paired_at: 1,
        last_seen: Some(NOW - 300),
        removed_by_peer: false,
        capabilities: if online {
            vec!["clipboard".into(), "transfer".into()]
        } else {
            Vec::new()
        },
        battery: None,
    }
}

fn status(input_from: &[&str]) -> Box<Status> {
    Box::new(Status {
        device_id: id(0xee),
        short_id: "SELF".into(),
        addr: "127.0.0.1:42187".parse().expect("addr"),
        daemon_version: "0.8.0".into(),
        ipc_version: playdrop_ipc::IPC_VERSION,
        settings: Settings {
            receive_dir: PathBuf::from("/tmp/in"),
            device_name: "desk".into(),
            clipboard_sync: true,
            clipboard_images: true,
            clipboard_watch_supported: true,
            remote_input_supported: true,
            remote_input_reason: None,
            autostart: false,
            autostart_refused: None,
            notifications: true,
            update_check: true,
        },
        add_device_window_open: false,
        locked: false,
        input_from: input_from.iter().map(|s| (*s).to_owned()).collect(),
    })
}

fn snapshot(tid: &str, n: u8, dir: TransferDirection, state: TransferStatus) -> TransferSnapshot {
    TransferSnapshot {
        id: tid.to_owned(),
        peer: id(n),
        short_id: format!("S{n}"),
        direction: dir,
        state,
        bytes: 0,
        total: 1000,
        bytes_per_sec: 0,
        files_done: 0,
        started_at: NOW - 10,
        file_count: 3,
        names: Vec::new(),
        names_more: false,
        note: None,
        saved: Vec::new(),
        folder: None,
        delivered: Vec::new(),
        resumable: false,
        failure: None,
    }
}

fn connected(peers: Vec<PeerSummary>, transfers: Vec<TransferSnapshot>) -> Model {
    let mut m = Model::new();
    let r = m.apply(
        Input::Connected {
            status: status(&[]),
            peers,
            transfers,
        },
        NOW,
    );
    assert_eq!(r, Refresh::NONE);
    m
}

fn host(e: HostEvent) -> Input {
    Input::Event(Box::new(Event::Host(e)))
}

fn daemon(e: DaemonEvent) -> Input {
    Input::Event(Box::new(Event::Daemon(e)))
}

fn progress(tid: &str, n: u8, bytes: u64, total: u64) -> Input {
    host(HostEvent::TransferProgress {
        id: tid.into(),
        peer: id(n),
        direction: TransferDirection::Send,
        bytes,
        total,
        bytes_per_sec: 10,
        files_done: 0,
    })
}

fn view(m: &Model) -> View {
    m.view(true, true)
}

// ---- daemon state ----------------------------------------------------------------------

#[test]
fn starts_connecting_with_the_offline_icon_and_no_devices() {
    let m = Model::new();
    let v = view(&m);
    assert_eq!(m.daemon(), &DaemonState::Connecting);
    assert_eq!(v.icon, PanelIcon::Offline);
    assert_eq!(v.status_line, "Connecting to PlayDrop...");
    assert_eq!(v.start, StartAction::Hidden);
    assert!(!v.show_devices);
    assert!(v.devices.is_empty());
}

#[test]
fn not_running_offers_start_only_when_a_daemon_binary_exists() {
    let mut m = Model::new();
    m.apply(Input::ConnectFailed(ConnectProblem::NotRunning), NOW);
    assert_eq!(m.view(true, true).start, StartAction::Available);
    assert_eq!(m.view(true, true).status_line, "PlayDrop is not running.");
    match m.view(false, true).start {
        StartAction::Unavailable(why) => assert!(why.contains("playdropd was not found")),
        other => panic!("expected a reason, got {other:?}"),
    }
    assert_eq!(m.view(true, true).tooltip, "PlayDrop is not running");
}

#[test]
fn other_connect_failures_say_why_and_offer_no_start() {
    let mut m = Model::new();
    m.apply(
        Input::ConnectFailed(ConnectProblem::Other("version mismatch".into())),
        NOW,
    );
    let v = view(&m);
    assert_eq!(v.status_line, "Cannot reach PlayDrop: version mismatch");
    assert_eq!(v.start, StartAction::Hidden);
    assert_eq!(v.icon, PanelIcon::Offline);
}

#[test]
fn daemon_gone_clears_everything_and_daemon_back_restores_it() {
    let mut m = connected(
        vec![peer(1, "Phone", true)],
        vec![snapshot(
            "t1",
            1,
            TransferDirection::Send,
            TransferStatus::Active,
        )],
    );
    m.apply(
        daemon(DaemonEvent::InputSession {
            peer: id(1),
            label: "Phone".into(),
            active: true,
        }),
        NOW,
    );
    m.set_busy(&id(1), Busy::Clipboard, true);
    assert_eq!(view(&m).icon, PanelIcon::Normal);

    m.apply(Input::Disconnected, NOW);
    let v = view(&m);
    assert_eq!(v.icon, PanelIcon::Offline);
    assert!(!v.show_devices);
    assert!(v.devices.is_empty());
    assert!(v.transfers.is_empty());
    assert!(v.input_banner.is_none());
    assert_eq!(m.daemon(), &DaemonState::NotRunning);

    // Events after the loss are stale and ignored.
    let r = m.apply(progress("t1", 1, 500, 1000), NOW);
    assert_eq!(r, Refresh::NONE);
    assert!(view(&m).transfers.is_empty());

    // Back: the new snapshot is the truth; the old busy mark is gone.
    let m2 = {
        m.apply(
            Input::Connected {
                status: status(&[]),
                peers: vec![peer(1, "Phone", true)],
                transfers: vec![],
            },
            NOW + 5,
        );
        m
    };
    let v = view(&m2);
    assert_eq!(v.icon, PanelIcon::Normal);
    assert_eq!(v.devices.len(), 1);
    assert_eq!(v.devices[0].clipboard, Action::Available);
    assert!(v.transfers.is_empty());
    assert_eq!(
        m2.daemon(),
        &DaemonState::Running {
            version: "0.8.0".into()
        }
    );
}

#[test]
fn peers_and_transfers_answers_are_ignored_while_disconnected() {
    let mut m = Model::new();
    m.apply(Input::Peers(vec![peer(1, "Phone", true)]), NOW);
    m.apply(
        Input::Transfers(vec![snapshot(
            "t1",
            1,
            TransferDirection::Send,
            TransferStatus::Active,
        )]),
        NOW,
    );
    m.apply(Input::Status(status(&["Phone"])), NOW);
    let v = view(&m);
    assert!(v.devices.is_empty() && v.transfers.is_empty() && v.input_banner.is_none());
}

// ---- devices ---------------------------------------------------------------------------

#[test]
fn devices_are_listed_online_first_then_by_label_ignoring_case() {
    let m = connected(
        vec![
            peer(1, "zebra", false),
            peer(2, "Tablet", true),
            peer(3, "apple", false),
            peer(4, "laptop", true),
        ],
        vec![],
    );
    let titles: Vec<String> = view(&m).devices.into_iter().map(|d| d.title).collect();
    assert_eq!(titles, ["laptop", "Tablet", "apple", "zebra"]);
    assert_eq!(
        view(&m).status_line,
        "PlayDrop is running. 2 of 4 devices online."
    );
    assert_eq!(view(&m).tooltip, "PlayDrop: 2 of 4 devices online");
}

#[test]
fn equal_labels_show_the_short_id_and_keep_a_stable_order() {
    let m = connected(vec![peer(2, "Phone", true), peer(1, "phone", true)], vec![]);
    let titles: Vec<String> = view(&m).devices.into_iter().map(|d| d.title).collect();
    assert_eq!(titles, ["phone (S1)", "Phone (S2)"]);
}

#[test]
fn no_devices_says_so() {
    let m = connected(vec![], vec![]);
    assert_eq!(
        view(&m).status_line,
        "PlayDrop is running. No paired devices yet."
    );
    let m = connected(vec![peer(1, "Phone", true)], vec![]);
    assert_eq!(
        view(&m).status_line,
        "PlayDrop is running. 1 of 1 device online."
    );
}

#[test]
fn offline_devices_offer_no_send_controls() {
    let m = connected(vec![peer(1, "Phone", false)], vec![]);
    let d = &view(&m).devices[0];
    assert_eq!(d.detail, "Offline");
    assert_eq!(d.clipboard, Action::Hidden);
    assert_eq!(d.files, Action::Hidden);
}

#[test]
fn a_missing_grant_or_capability_is_explained_not_hidden() {
    let mut no_grant = peer(1, "Phone", true);
    no_grant.grants.clipboard_out = false;
    let mut no_caps = peer(2, "Watch", true);
    no_caps.capabilities = vec!["ping".into()];
    let mut old_daemon = peer(3, "Old", true);
    old_daemon.capabilities = Vec::new();
    let m = connected(vec![no_grant, no_caps, old_daemon], vec![]);
    let v = view(&m);
    let by = |t: &str| {
        v.devices
            .iter()
            .find(|d| d.title == t)
            .expect("row")
            .clone()
    };
    assert_eq!(
        by("Phone").clipboard,
        Action::Unavailable("Clipboard sharing is off for this device.".into())
    );
    assert_eq!(by("Phone").files, Action::Available);
    assert_eq!(
        by("Watch").clipboard,
        Action::Unavailable("This device does not take clipboard text.".into())
    );
    assert_eq!(
        by("Watch").files,
        Action::Unavailable("This device does not take files.".into())
    );
    // No capability list at all: an older daemon. Do not hide working controls.
    assert_eq!(by("Old").clipboard, Action::Available);
    assert_eq!(by("Old").files, Action::Available);
}

#[test]
fn removed_by_peer_explains_and_offers_nothing() {
    let m = {
        let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
        m.apply(
            host(HostEvent::RemovedByPeer {
                peer: id(1),
                short_id: "S1".into(),
            }),
            NOW,
        );
        m
    };
    let d = &view(&m).devices[0];
    assert!(d.detail.starts_with("Forgot this computer"));
    assert_eq!(d.clipboard, Action::Hidden);
    assert_eq!(d.files, Action::Hidden);
}

#[test]
fn battery_shows_while_online_and_survives_a_peers_refresh() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(
        host(HostEvent::Battery {
            peer: id(1),
            level: 80,
            charging: true,
            low: false,
        }),
        NOW,
    );
    assert_eq!(view(&m).devices[0].detail, "Online, battery 80% (charging)");
    m.apply(Input::Peers(vec![peer(1, "Phone", true)]), NOW);
    assert_eq!(view(&m).devices[0].detail, "Online, battery 80% (charging)");
    // A level over 100 from a misbehaving peer is clamped.
    m.apply(
        host(HostEvent::Battery {
            peer: id(1),
            level: 250,
            charging: false,
            low: false,
        }),
        NOW,
    );
    assert_eq!(view(&m).devices[0].detail, "Online, battery 100%");
}

#[test]
fn going_offline_drops_battery_and_busy_and_coming_online_asks_for_peers() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(
        host(HostEvent::Battery {
            peer: id(1),
            level: 50,
            charging: false,
            low: false,
        }),
        NOW,
    );
    m.set_busy(&id(1), Busy::Files, true);
    let r = m.apply(
        host(HostEvent::PeerOffline {
            peer: id(1),
            short_id: "S1".into(),
        }),
        NOW,
    );
    assert_eq!(r, Refresh::NONE);
    let d = &view(&m).devices[0];
    assert_eq!(d.detail, "Offline");
    assert_eq!(d.files, Action::Hidden);

    let r = m.apply(
        host(HostEvent::PeerOnline {
            peer: id(1),
            short_id: "S1".into(),
            name: "Phone".into(),
            addr: "192.0.2.1:42187".parse().expect("addr"),
        }),
        NOW,
    );
    assert_eq!(r, Refresh::PEERS);
    let d = &view(&m).devices[0];
    assert!(d.online);
    // Not busy any more, even though it was before going offline.
    assert_eq!(d.files, Action::Available);
}

#[test]
fn pairing_and_unpairing_update_the_list() {
    let mut m = connected(
        vec![peer(1, "Phone", true), peer(2, "Tablet", false)],
        vec![],
    );
    let r = m.apply(
        host(HostEvent::Paired {
            peer: id(3),
            short_id: "S3".into(),
            name: "New".into(),
        }),
        NOW,
    );
    assert_eq!(r, Refresh::PEERS);
    m.apply(
        host(HostEvent::Unpaired {
            peer: id(2),
            short_id: "S2".into(),
        }),
        NOW,
    );
    let titles: Vec<String> = view(&m).devices.into_iter().map(|d| d.title).collect();
    assert_eq!(titles, ["Phone"]);
}

#[test]
fn busy_marks_replace_the_control_until_cleared() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_busy(&id(1), Busy::Clipboard, true);
    let d = &view(&m).devices[0];
    assert_eq!(d.clipboard, Action::Busy("Sending clipboard...".into()));
    assert_eq!(d.files, Action::Available);
    m.set_busy(&id(1), Busy::Clipboard, false);
    assert_eq!(view(&m).devices[0].clipboard, Action::Available);
}

// ---- transfers -------------------------------------------------------------------------

#[test]
fn progress_updates_the_percent_and_bar() {
    let mut m = connected(
        vec![peer(1, "Phone", true)],
        vec![snapshot(
            "t1",
            1,
            TransferDirection::Send,
            TransferStatus::Offered,
        )],
    );
    let v = view(&m);
    assert_eq!(v.transfers[0].title, "3 files to Phone");
    assert_eq!(
        v.transfers[0].detail,
        "Waiting for the other device to accept"
    );
    assert_eq!(v.transfers[0].progress, None);

    assert_eq!(m.apply(progress("t1", 1, 450, 1000), NOW), Refresh::NONE);
    let t = &view(&m).transfers[0];
    assert_eq!(t.detail, "Sending 45%");
    assert_eq!(t.progress, Some(0.45));

    m.apply(progress("t1", 1, 1000, 1000), NOW);
    assert_eq!(view(&m).transfers[0].detail, "Sending 100%");
}

#[test]
fn progress_for_an_unknown_transfer_adds_it_and_asks_for_the_list() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    let r = m.apply(progress("t9", 1, 10, 100), NOW);
    assert_eq!(r, Refresh::TRANSFERS);
    let t = &view(&m).transfers[0];
    assert_eq!(t.title, "files to Phone");
    assert_eq!(t.detail, "Sending 10%");
    // The list answer fills in the file count.
    let mut s = snapshot("t9", 1, TransferDirection::Send, TransferStatus::Active);
    s.bytes = 20;
    s.total = 100;
    s.file_count = 2;
    m.apply(Input::Transfers(vec![s]), NOW);
    let t = &view(&m).transfers[0];
    assert_eq!(t.title, "2 files to Phone");
    assert_eq!(t.detail, "Sending 20%");
}

#[test]
fn progress_after_completion_is_ignored() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(progress("t1", 1, 10, 100), NOW);
    m.apply(
        host(HostEvent::TransferComplete {
            id: "t1".into(),
            peer: id(1),
            direction: TransferDirection::Send,
            bytes: 100,
            elapsed_ms: 5,
            roots: vec![],
            bundle_root: None,
            saved: vec![],
            folder: None,
            delivered: vec![],
        }),
        NOW,
    );
    m.apply(progress("t1", 1, 50, 100), NOW);
    assert_eq!(view(&m).transfers[0].detail, "Sent");
}

#[test]
fn received_single_file_shows_its_name() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(
        host(HostEvent::TransferOffer {
            id: "r1".into(),
            peer: id(1),
            short_id: "S1".into(),
            files: vec![OfferedFile {
                path: "DCIM/photo.jpg".into(),
                size: 5,
            }],
            file_count: 1,
            total_bytes: 5,
            note: None,
        }),
        NOW,
    );
    let t = &view(&m).transfers[0];
    assert_eq!(t.title, "photo.jpg from Phone");
    assert_eq!(t.detail, "Waiting for you to accept in the PlayDrop window");

    m.apply(
        host(HostEvent::TransferComplete {
            id: "r1".into(),
            peer: id(1),
            direction: TransferDirection::Receive,
            bytes: 5,
            elapsed_ms: 5,
            roots: vec![FileRoot {
                path: "DCIM/photo.jpg".into(),
                size: 5,
                root: None,
            }],
            bundle_root: None,
            saved: vec![PathBuf::from("/home/u/Downloads/photo (1).jpg")],
            folder: None,
            delivered: vec![],
        }),
        NOW,
    );
    let t = &view(&m).transfers[0];
    assert_eq!(t.title, "photo.jpg from Phone");
    assert_eq!(t.detail, "Received");
}

#[test]
fn a_completion_seen_first_names_the_saved_file() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(
        host(HostEvent::TransferComplete {
            id: "r2".into(),
            peer: id(1),
            direction: TransferDirection::Receive,
            bytes: 5,
            elapsed_ms: 5,
            roots: vec![FileRoot {
                path: "a.txt".into(),
                size: 5,
                root: None,
            }],
            bundle_root: None,
            saved: vec![PathBuf::from("/in/a (1).txt")],
            folder: None,
            delivered: vec![],
        }),
        NOW,
    );
    assert_eq!(view(&m).transfers[0].title, "a (1).txt from Phone");
}

#[test]
fn files_the_applet_sent_are_named_by_it() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.note_sent("t1", &[PathBuf::from("/home/u/report.pdf")]);
    m.apply(progress("t1", 1, 1, 2), NOW);
    assert_eq!(view(&m).transfers[0].title, "report.pdf to Phone");
    // Several paths: no single name, the count comes from the snapshot.
    m.note_sent("t2", &[PathBuf::from("/a"), PathBuf::from("/b")]);
    m.apply(progress("t2", 1, 1, 2), NOW + 1);
    assert_eq!(view(&m).transfers[0].title, "files to Phone");
}

#[test]
fn failures_show_the_reason_and_ask_for_the_list() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(progress("t1", 1, 10, 100), NOW);
    let r = m.apply(
        host(HostEvent::TransferFailed {
            id: "t1".into(),
            peer: id(1),
            direction: TransferDirection::Send,
            reason: "the device went away".into(),
            code: playdrop_ipc::types::TransferFailure::default(),
        }),
        NOW,
    );
    assert_eq!(r, Refresh::TRANSFERS);
    let t = &view(&m).transfers[0];
    assert_eq!(t.detail, "Failed: the device went away");
    assert_eq!(t.progress, None);

    let mut s = snapshot("t1", 1, TransferDirection::Send, TransferStatus::Cancelled);
    s.note = Some("cancelled by you".into());
    m.apply(Input::Transfers(vec![s]), NOW);
    assert_eq!(view(&m).transfers[0].detail, "Cancelled: cancelled by you");
}

#[test]
fn active_transfers_come_first_then_recent_finished_capped() {
    let mut list = Vec::new();
    for i in 0..8u64 {
        let mut s = snapshot(
            &format!("done{i}"),
            1,
            TransferDirection::Receive,
            TransferStatus::Complete,
        );
        s.started_at = NOW - 100 + i;
        list.push(s);
    }
    let mut a = snapshot(
        "live-old",
        1,
        TransferDirection::Send,
        TransferStatus::Active,
    );
    a.started_at = NOW - 50;
    let mut b = snapshot(
        "live-new",
        1,
        TransferDirection::Send,
        TransferStatus::Active,
    );
    b.started_at = NOW - 5;
    list.push(a);
    list.push(b);
    let m = connected(vec![peer(1, "Phone", true)], list);
    let ids: Vec<String> = view(&m).transfers.into_iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        ["live-new", "live-old", "done7", "done6", "done5", "done4"]
    );
    assert_eq!(SHOW_FINISHED, 4);
}

#[test]
fn finished_transfers_expire_after_ten_minutes_but_active_ones_stay() {
    let mut done = snapshot("old", 1, TransferDirection::Send, TransferStatus::Complete);
    // Started long ago but only now seen finished: still shown.
    done.started_at = NOW - 3_600;
    let live = snapshot("live", 1, TransferDirection::Send, TransferStatus::Active);
    let mut m = connected(vec![peer(1, "Phone", true)], vec![done, live]);
    assert_eq!(view(&m).transfers.len(), 2);
    m.tick(NOW + 600);
    assert_eq!(view(&m).transfers.len(), 2);
    m.tick(NOW + 601);
    let ids: Vec<String> = view(&m).transfers.into_iter().map(|t| t.id).collect();
    assert_eq!(ids, ["live"]);
}

#[test]
fn the_model_keeps_a_bounded_number_of_finished_transfers() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    for i in 0..40u64 {
        let tid = format!("t{i}");
        m.apply(progress(&tid, 1, 1, 2), NOW + i);
        m.apply(
            host(HostEvent::TransferFailed {
                id: tid,
                peer: id(1),
                direction: TransferDirection::Send,
                reason: "x".into(),
                code: playdrop_ipc::types::TransferFailure::default(),
            }),
            NOW + i,
        );
    }
    assert_eq!(m.transfers.len(), 16);
    // The newest are the ones kept, and the popup shows the newest few.
    let ids: Vec<String> = view(&m).transfers.into_iter().map(|t| t.id).collect();
    assert_eq!(ids, ["t39", "t38", "t37", "t36"]);
}

#[test]
fn transfer_with_an_unknown_peer_uses_its_short_id() {
    let m = connected(
        vec![],
        vec![snapshot(
            "t1",
            7,
            TransferDirection::Receive,
            TransferStatus::Active,
        )],
    );
    assert_eq!(view(&m).transfers[0].title, "3 files from S7");
}

#[test]
fn percent_is_bounded_and_safe() {
    assert_eq!(percent(0, 0), 0);
    assert_eq!(percent(5, 10), 50);
    assert_eq!(percent(20, 10), 100);
    assert_eq!(percent(u64::MAX, u64::MAX), 100);
    assert_eq!(percent(1, 3), 33);
}

// ---- remote input ----------------------------------------------------------------------

#[test]
fn input_sessions_from_status_and_events_show_and_end() {
    let mut m = Model::new();
    m.apply(
        Input::Connected {
            status: status(&["Phone"]),
            peers: vec![peer(1, "Phone", true)],
            transfers: vec![],
        },
        NOW,
    );
    assert_eq!(
        view(&m).input_banner.as_deref(),
        Some("Remote input from Phone")
    );
    // The event for the same session replaces the label-only entry, not duplicates it.
    m.apply(
        daemon(DaemonEvent::InputSession {
            peer: id(1),
            label: "Phone".into(),
            active: true,
        }),
        NOW,
    );
    m.apply(
        daemon(DaemonEvent::InputSession {
            peer: id(2),
            label: "Tablet".into(),
            active: true,
        }),
        NOW,
    );
    assert_eq!(
        view(&m).input_banner.as_deref(),
        Some("Remote input from Phone, Tablet")
    );
    m.apply(
        daemon(DaemonEvent::InputSession {
            peer: id(1),
            label: "Phone".into(),
            active: false,
        }),
        NOW,
    );
    assert_eq!(
        view(&m).input_banner.as_deref(),
        Some("Remote input from Tablet")
    );
    // A status after a lag that lists nobody ends them all.
    m.apply(Input::Status(status(&[])), NOW);
    assert!(view(&m).input_banner.is_none());
}

// ---- misc events -----------------------------------------------------------------------

#[test]
fn lock_state_shows_a_note() {
    let mut m = connected(vec![], vec![]);
    m.apply(daemon(DaemonEvent::LockChanged { locked: true }), NOW);
    assert!(view(&m).locked_note.is_some());
    m.apply(daemon(DaemonEvent::LockChanged { locked: false }), NOW);
    assert!(view(&m).locked_note.is_none());
}

#[test]
fn host_restart_asks_for_everything_including_status() {
    let mut m = connected(vec![], vec![]);
    let r = m.apply(daemon(DaemonEvent::HostRestarted), NOW);
    assert_eq!(r, Refresh::ALL);
    assert!(r.status && r.peers && r.transfers);
    // The status that comes back is applied: here it reports a session.
    m.apply(Input::Status(status(&["Phone"])), NOW);
    assert_eq!(
        view(&m).input_banner.as_deref(),
        Some("Remote input from Phone")
    );
}

#[test]
fn events_the_applet_does_not_use_change_nothing() {
    let mut m = connected(
        vec![peer(1, "Phone", true)],
        vec![snapshot(
            "t1",
            1,
            TransferDirection::Send,
            TransferStatus::Active,
        )],
    );
    let before = view(&m);
    let ignored = [
        host(HostEvent::Ping {
            peer: id(1),
            message: Some("hi".into()),
        }),
        host(HostEvent::Presence {
            peer: id(1),
            state: "idle".into(),
        }),
        host(HostEvent::PairingFailed { reason: "x".into() }),
        host(HostEvent::Unsupported {
            peer: id(1),
            frame_type: "future".into(),
        }),
        // Events for a device the applet does not list.
        host(HostEvent::Battery {
            peer: id(9),
            level: 1,
            charging: false,
            low: true,
        }),
        host(HostEvent::PeerOffline {
            peer: id(9),
            short_id: "S9".into(),
        }),
        daemon(DaemonEvent::ClipboardPushed {
            sent_to: vec![id(1)],
        }),
        daemon(DaemonEvent::ClipboardApplied {
            peer: id(1),
            sensitive: false,
        }),
    ];
    for e in ignored {
        assert_eq!(m.apply(e, NOW), Refresh::NONE);
    }
    assert_eq!(view(&m), before);
}

// ---- notices ---------------------------------------------------------------------------

#[test]
fn clipboard_outcomes_are_plain_words() {
    assert_eq!(
        clipboard_outcome("Phone", true, None, false),
        "Clipboard sent to Phone."
    );
    assert_eq!(
        clipboard_outcome("Phone", false, Some(ClipSkip::NoPeers), false),
        "Clipboard not sent: no connected device has clipboard sharing turned on."
    );
    assert_eq!(
        clipboard_outcome("Phone", false, None, true),
        "Clipboard not sent: it matches the last clip sent or received."
    );
    assert_eq!(
        clipboard_outcome("Phone", false, None, false),
        "Clipboard not sent: the daemon sent nothing."
    );
}

#[test]
fn notices_show_until_cleared_and_labels_fall_back() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_notice("Clipboard sent to Phone.");
    assert_eq!(view(&m).notice.as_deref(), Some("Clipboard sent to Phone."));
    m.clear_notice();
    assert!(view(&m).notice.is_none());
    assert_eq!(m.label_of(&id(1)), "Phone");
    assert_eq!(m.label_of(&id(5)), "the device");
}

#[test]
fn open_window_says_why_when_missing() {
    let m = connected(vec![], vec![]);
    assert_eq!(m.view(true, true).open_window, WindowAction::Available);
    match m.view(true, false).open_window {
        WindowAction::Unavailable(why) => assert!(why.contains("playdrop-desktop")),
        WindowAction::Available => panic!("expected a reason"),
    }
}

// ---- starting the daemon ---------------------------------------------------------------

#[test]
fn start_shows_starting_instead_of_the_button_until_it_connects() {
    let mut m = Model::new();
    m.apply(Input::ConnectFailed(ConnectProblem::NotRunning), NOW);
    assert_eq!(view(&m).start, StartAction::Available);
    m.apply(Input::StartRequested, NOW);
    let v = view(&m);
    assert_eq!(v.status_line, "PlayDrop is starting...");
    assert_eq!(v.start, StartAction::Hidden);
    // The daemon answers "starting; try again" while its host comes up: still starting.
    m.apply(Input::ConnectFailed(ConnectProblem::Starting), NOW);
    assert_eq!(view(&m).start, StartAction::Hidden);
    m.set_notice("something earlier");
    m.apply(
        Input::Connected {
            status: status(&[]),
            peers: vec![],
            transfers: vec![],
        },
        NOW,
    );
    let v = view(&m);
    assert_eq!(v.icon, PanelIcon::Normal);
    assert!(v.notice.is_none(), "connect clears old notices");
}

#[test]
fn a_failed_start_says_so_and_offers_the_button_again() {
    let mut m = Model::new();
    m.apply(Input::ConnectFailed(ConnectProblem::NotRunning), NOW);
    m.apply(Input::StartRequested, NOW);
    m.apply(Input::StartFailed("playdropd was not found".into()), NOW);
    let v = view(&m);
    assert_eq!(
        v.notice.as_deref(),
        Some("PlayDrop did not start: playdropd was not found.")
    );
    assert_eq!(v.start, StartAction::Available);
    // A start request while connected changes nothing.
    let mut m = connected(vec![], vec![]);
    m.apply(Input::StartRequested, NOW);
    m.apply(Input::StartFailed("x".into()), NOW);
    assert_eq!(view(&m).icon, PanelIcon::Normal);
    assert!(view(&m).notice.is_none());
}

#[test]
fn connect_problems_have_plain_words() {
    assert_eq!(ConnectProblem::NotRunning.text(), "PlayDrop is not running");
    assert_eq!(
        ConnectProblem::Starting.text(),
        "PlayDrop is still starting"
    );
    assert_eq!(ConnectProblem::Other("x".into()).text(), "x");
}

// ---- transfer bookkeeping --------------------------------------------------------------

#[test]
fn a_refresh_does_not_bring_back_transfers_the_applet_pruned() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    // The daemon still lists 30 finished transfers; the applet keeps 16.
    let list: Vec<TransferSnapshot> = (0..30u64)
        .map(|i| {
            let mut s = snapshot(
                &format!("old{i:02}"),
                1,
                TransferDirection::Send,
                TransferStatus::Complete,
            );
            s.started_at = NOW - 100 + i;
            s
        })
        .collect();
    m.apply(Input::Transfers(list.clone()), NOW);
    assert_eq!(m.transfers.len(), 16);
    // A new one ends later.
    m.apply(progress("new", 1, 1, 2), NOW + 30);
    m.apply(
        host(HostEvent::TransferFailed {
            id: "new".into(),
            peer: id(1),
            direction: TransferDirection::Send,
            reason: "x".into(),
            code: playdrop_ipc::types::TransferFailure::default(),
        }),
        NOW + 30,
    );
    let mut with_new = list;
    let mut s = snapshot("new", 1, TransferDirection::Send, TransferStatus::Failed);
    s.started_at = NOW + 30;
    with_new.push(s);
    // The refresh lists the old ones again; the pruned ones must not come back stamped as
    // just changed and push the new one out.
    m.apply(Input::Transfers(with_new), NOW + 31);
    assert_eq!(m.transfers.len(), 16);
    let ids: Vec<String> = view(&m).transfers.into_iter().map(|t| t.id).collect();
    assert_eq!(ids[0], "new");
    assert!(m
        .transfers
        .contains_key(&("new".to_owned(), TransferDirection::Send)));
}

#[test]
fn a_snapshot_never_moves_a_finished_transfer_back() {
    let mut m = connected(
        vec![peer(1, "Phone", true)],
        vec![snapshot(
            "t1",
            1,
            TransferDirection::Send,
            TransferStatus::Active,
        )],
    );
    m.apply(
        host(HostEvent::TransferComplete {
            id: "t1".into(),
            peer: id(1),
            direction: TransferDirection::Send,
            bytes: 1000,
            elapsed_ms: 5,
            roots: vec![],
            bundle_root: None,
            saved: vec![],
            folder: None,
            delivered: vec![],
        }),
        NOW,
    );
    // A list fetched before the end arrives after it.
    let mut stale = snapshot("t1", 1, TransferDirection::Send, TransferStatus::Active);
    stale.bytes = 500;
    m.apply(Input::Transfers(vec![stale]), NOW);
    let t = &view(&m).transfers[0];
    assert_eq!(t.detail, "Sent");
    assert_eq!(t.progress, None);
}

#[test]
fn the_same_id_in_both_directions_is_two_transfers() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.apply(progress("same", 1, 1, 4), NOW);
    m.apply(
        host(HostEvent::TransferProgress {
            id: "same".into(),
            peer: id(1),
            direction: TransferDirection::Receive,
            bytes: 3,
            total: 4,
            bytes_per_sec: 1,
            files_done: 0,
        }),
        NOW,
    );
    let mut details: Vec<String> = view(&m).transfers.into_iter().map(|t| t.detail).collect();
    details.sort();
    assert_eq!(details, ["Receiving 75%", "Sending 25%"]);
}

// ---- password-manager clips (SR-24) ------------------------------------------------------

#[test]
fn a_sensitive_clip_asks_before_sending() {
    let mut m = connected(
        vec![peer(1, "Phone", true), peer(2, "Tablet", true)],
        vec![],
    );
    m.set_confirm_sensitive(Some(&id(1)));
    let v = view(&m);
    let phone = v.devices.iter().find(|d| d.title == "Phone").expect("row");
    assert_eq!(
        phone.confirm.as_deref(),
        Some("This clip came from a password manager. Send it to Phone?")
    );
    // The plain button gives way to the question; other devices are untouched.
    assert_eq!(phone.clipboard, Action::Hidden);
    let tablet = v.devices.iter().find(|d| d.title == "Tablet").expect("row");
    assert_eq!(tablet.confirm, None);
    assert_eq!(tablet.clipboard, Action::Available);
    // The device going offline drops the question.
    m.apply(
        Input::Peers(vec![peer(1, "Phone", false), peer(2, "Tablet", true)]),
        NOW,
    );
    assert_eq!(m.confirm_sensitive(), None);
    // And so does the daemon going away.
    m.set_confirm_sensitive(Some(&id(2)));
    m.apply(Input::Disconnected, NOW);
    assert_eq!(m.confirm_sensitive(), None);
}

#[test]
fn a_clipboard_send_ending_does_not_clear_an_open_file_chooser() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_busy(&id(1), Busy::Files, true);
    m.set_busy(&id(1), Busy::Clipboard, true);
    let d = &view(&m).devices[0];
    assert!(matches!(d.clipboard, Action::Busy(_)));
    assert!(matches!(d.files, Action::Busy(_)));
    m.set_busy(&id(1), Busy::Clipboard, false);
    let d = &view(&m).devices[0];
    assert_eq!(d.clipboard, Action::Available);
    assert_eq!(d.files, Action::Busy("Choosing what to send...".into()));
}

#[test]
fn going_offline_drops_the_password_manager_question_for_good() {
    let mut m = connected(
        vec![peer(1, "Phone", true), peer(2, "Tablet", true)],
        vec![],
    );
    m.set_confirm_sensitive(Some(&id(1)));
    m.apply(
        host(HostEvent::PeerOffline {
            peer: id(2),
            short_id: "S2".into(),
        }),
        NOW,
    );
    assert_eq!(
        m.confirm_sensitive(),
        Some(id(1).as_str()),
        "another device"
    );
    m.apply(
        host(HostEvent::PeerOffline {
            peer: id(1),
            short_id: "S1".into(),
        }),
        NOW,
    );
    assert_eq!(m.confirm_sensitive(), None);
    // Back online: the old question does not come back.
    m.apply(
        host(HostEvent::PeerOnline {
            peer: id(1),
            short_id: "S1".into(),
            name: "Phone".into(),
            addr: "192.0.2.1:42187".parse().expect("addr"),
        }),
        NOW,
    );
    m.apply(Input::Peers(vec![peer(1, "Phone", true)]), NOW);
    assert_eq!(m.confirm_sensitive(), None);
    assert_eq!(view(&m).devices[0].confirm, None);
    assert_eq!(view(&m).devices[0].clipboard, Action::Available);
}

#[test]
fn unpairing_drops_the_password_manager_question() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_confirm_sensitive(Some(&id(1)));
    m.set_busy(&id(1), Busy::Files, true);
    m.apply(
        host(HostEvent::Unpaired {
            peer: id(1),
            short_id: "S1".into(),
        }),
        NOW,
    );
    assert_eq!(m.confirm_sensitive(), None);
    assert!(view(&m).devices.is_empty());
}

#[test]
fn a_device_that_removed_this_computer_drops_the_password_manager_question() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_confirm_sensitive(Some(&id(1)));
    m.set_busy(&id(1), Busy::Files, true);
    m.apply(
        host(HostEvent::RemovedByPeer {
            peer: id(1),
            short_id: "S1".into(),
        }),
        NOW,
    );
    assert_eq!(m.confirm_sensitive(), None, "nothing can be sent to it");
    // A later list that still shows it online does not bring the question back.
    m.set_confirm_sensitive(Some(&id(1)));
    assert_eq!(m.confirm_sensitive(), None);
}

#[test]
fn an_automatic_not_running_answer_mid_start_keeps_the_start_button_hidden() {
    let mut m = Model::new();
    m.apply(Input::ConnectFailed(ConnectProblem::NotRunning), NOW);
    m.apply(Input::StartRequested, NOW);
    // An attempt already under way when "Start" was clicked answers "not running".
    m.apply(Input::ConnectFailed(ConnectProblem::NotRunning), NOW);
    let v = view(&m);
    assert_eq!(v.status_line, "PlayDrop is starting...");
    assert_eq!(
        v.start,
        StartAction::Hidden,
        "no second start while one runs"
    );
    // Only the start's own answer ends it.
    m.apply(Input::StartFailed("it did not answer".into()), NOW);
    assert_eq!(view(&m).start, StartAction::Available);
    // And after that, automatic answers count again.
    m.apply(Input::ConnectFailed(ConnectProblem::Other("x".into())), NOW);
    assert_eq!(view(&m).status_line, "Cannot reach PlayDrop: x");
}

#[test]
fn a_daemon_stuck_starting_is_shown_as_not_ready() {
    let mut m = Model::new();
    m.apply(Input::ConnectFailed(ConnectProblem::Starting), NOW);
    assert_eq!(view(&m).status_line, "PlayDrop is starting...");
    m.apply(Input::ConnectFailed(ConnectProblem::NotReady), NOW);
    let v = view(&m);
    assert!(v
        .status_line
        .starts_with("PlayDrop is running but not ready"));
    assert_eq!(v.start, StartAction::Hidden);
    assert_eq!(
        ConnectProblem::NotReady.text(),
        "PlayDrop is running but not ready"
    );
}

#[test]
fn no_question_is_asked_for_a_device_that_cannot_receive() {
    let mut m = connected(vec![peer(1, "Phone", false)], vec![]);
    m.set_confirm_sensitive(Some(&id(1)));
    assert_eq!(m.confirm_sensitive(), None);
    m.set_confirm_sensitive(Some(&id(9)));
    assert_eq!(m.confirm_sensitive(), None);
}

// ---- battery from the answer, the daemon-side clipboard, transfer names ------------------

fn with_battery(mut p: PeerSummary, level: u8, charging: bool) -> PeerSummary {
    p.battery = Some(PeerBattery {
        level,
        charging,
        low: false,
    });
    p
}

#[test]
fn battery_is_seeded_from_the_peers_answer() {
    let m = connected(vec![with_battery(peer(1, "Phone", true), 64, true)], vec![]);
    assert_eq!(view(&m).devices[0].detail, "Online, battery 64% (charging)");
    // A later answer replaces it, and a level over 100 is clamped.
    let mut m = m;
    m.apply(
        Input::Peers(vec![with_battery(peer(1, "Phone", true), 200, false)]),
        NOW,
    );
    assert_eq!(view(&m).devices[0].detail, "Online, battery 100%");
}

#[test]
fn an_answer_without_battery_behaves_as_before() {
    // An older daemon (or a device that has not reported): nothing shown at first...
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    assert_eq!(view(&m).devices[0].detail, "Online");
    // ...an event is kept across a refresh that carries none...
    m.apply(
        host(HostEvent::Battery {
            peer: id(1),
            level: 55,
            charging: false,
            low: false,
        }),
        NOW,
    );
    m.apply(Input::Peers(vec![peer(1, "Phone", true)]), NOW);
    assert_eq!(view(&m).devices[0].detail, "Online, battery 55%");
    // ...and an offline device never shows one, whatever the answer carries.
    m.apply(
        Input::Peers(vec![with_battery(peer(1, "Phone", false), 90, false)]),
        NOW,
    );
    assert_eq!(view(&m).devices[0].detail, "Offline");
}

#[allow(clippy::unnecessary_wraps)]
fn pushed(sent_to: Vec<String>, skipped: Option<ClipSkip>) -> Result<Answer, IpcError> {
    Ok(Answer::ClipboardPushed {
        sent_to,
        skipped_echo: skipped == Some(ClipSkip::Echo),
        skipped,
        consent: None,
    })
}

#[allow(clippy::unnecessary_wraps)]
fn refused(token: Option<&str>) -> Result<Answer, IpcError> {
    Ok(Answer::ClipboardPushed {
        sent_to: Vec::new(),
        skipped_echo: false,
        skipped: Some(ClipSkip::Sensitive),
        consent: token.map(ConsentToken::new),
    })
}

#[test]
fn send_clipboard_asks_the_daemon_to_read_it_first() {
    assert_eq!(
        local_clip_request(&id(1), false, None),
        Request::SendLocalClipboard {
            peer: Some(id(1)),
            allow_sensitive: false,
            consent: None,
        }
    );
}

#[test]
fn a_held_back_password_clip_asks_and_the_resend_carries_the_consent_and_token() {
    let token = "ab".repeat(16);
    assert_eq!(
        local_clip_result("Phone", refused(Some(&token))),
        LocalClip::NeedsConfirm(Some(ConsentToken::new(token.clone())))
    );
    // The question is the existing one, and it is dropped with the device.
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.set_confirm_sensitive(Some(&id(1)));
    let row = &view(&m).devices[0];
    assert_eq!(
        row.confirm.as_deref(),
        Some("This clip came from a password manager. Send it to Phone?")
    );
    // "Send" asks again with the consent and the token from the refusal.
    assert_eq!(
        local_clip_request(&id(1), true, Some(ConsentToken::new(token))),
        Request::SendLocalClipboard {
            peer: Some(id(1)),
            allow_sensitive: true,
            consent: Some(ConsentToken::new("ab".repeat(16))),
        }
    );
    assert_eq!(
        local_clip_result("Phone", pushed(vec![id(1)], None)),
        LocalClip::Done("Clipboard sent to Phone.".into())
    );
}

#[test]
fn a_second_refusal_asks_again_with_the_new_token() {
    // The clipboard changed between the question and the answer: not an error, not a send.
    assert_eq!(
        local_clip_result("Phone", refused(Some("cd".repeat(16).as_str()))),
        LocalClip::NeedsConfirm(Some(ConsentToken::new("cd".repeat(16))))
    );
}

#[test]
fn a_refusal_without_a_token_still_asks_and_resends_without_one() {
    assert_eq!(
        local_clip_result("Phone", refused(None)),
        LocalClip::NeedsConfirm(None)
    );
    assert_eq!(
        local_clip_request(&id(1), true, None),
        Request::SendLocalClipboard {
            peer: Some(id(1)),
            allow_sensitive: true,
            consent: None,
        }
    );
}

#[test]
fn other_skips_show_their_reason_sentence() {
    assert_eq!(
        local_clip_result("Phone", pushed(vec![], Some(ClipSkip::Empty))),
        LocalClip::Done("Clipboard not sent: the clipboard is empty.".into())
    );
    assert_eq!(
        local_clip_result("Phone", pushed(vec![], Some(ClipSkip::Unsupported))),
        LocalClip::Done(
            "Clipboard not sent: the connected devices cannot receive this kind of clip.".into()
        )
    );
    assert_eq!(
        local_clip_result("Phone", pushed(vec![], Some(ClipSkip::Echo))),
        LocalClip::Done("Clipboard not sent: it matches the last clip sent or received.".into())
    );
}

#[test]
fn an_older_daemon_or_an_unreadable_clipboard_falls_back_to_reading_it_here() {
    for code in [ErrorCode::BadRequest, ErrorCode::InvalidState] {
        assert_eq!(
            local_clip_result("Phone", Err(IpcError::new(code, "x"))),
            LocalClip::Fallback
        );
    }
    // Any other failure is the outcome, not a reason to read the clipboard again.
    assert_eq!(
        local_clip_result("Phone", Err(IpcError::new(ErrorCode::Internal, "boom"))),
        LocalClip::Done("Clipboard not sent: boom".into())
    );
}

fn named(tid: &str, names: &[&str], file_count: usize) -> TransferSnapshot {
    let mut s = snapshot(tid, 1, TransferDirection::Send, TransferStatus::Active);
    s.names = names.iter().map(|n| (*n).to_owned()).collect();
    s.names_more = false;
    s.file_count = file_count;
    s
}

#[test]
fn transfer_rows_show_the_names_the_daemon_lists() {
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["notes.txt"], 1)],
    );
    assert_eq!(view(&m).transfers[0].title, "notes.txt to Phone");
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["Photos"], 50)],
    );
    assert_eq!(view(&m).transfers[0].title, "Photos (50 files) to Phone");
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["a.pdf", "Photos"], 9)],
    );
    assert_eq!(
        view(&m).transfers[0].title,
        "a.pdf and Photos (9 files) to Phone"
    );
    // Exactly the files named: no count.
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["a.pdf", "b.pdf", "c.pdf"], 3)],
    );
    assert_eq!(
        view(&m).transfers[0].title,
        "a.pdf, b.pdf and c.pdf to Phone"
    );
    // Names left out: say so.
    let mut s = named("t1", &["a.pdf", "b.pdf", "c.pdf"], 10);
    s.names_more = true;
    let m = connected(vec![peer(1, "Phone", true)], vec![s]);
    assert_eq!(
        view(&m).transfers[0].title,
        "a.pdf, b.pdf, c.pdf and more (10 files) to Phone"
    );
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["a.pdf", "Photos"], 5001)],
    );
    assert_eq!(
        view(&m).transfers[0].title,
        "a.pdf and Photos (5,001 files) to Phone"
    );
}

#[test]
fn long_and_odd_names_stay_on_one_short_line() {
    let long = "x".repeat(200);
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &[&long, "line\nbreak\u{202e}.txt", &long], 3)],
    );
    let title = view(&m).transfers[0].title.clone();
    assert!(title.ends_with(" to Phone"), "{title}");
    assert!(!title.chars().any(char::is_control), "{title:?}");
    assert!(
        title.chars().count() <= WHAT_SHOWN + " to Phone".len(),
        "{title}"
    );
    assert!(title.contains("..."), "{title}");
    assert_eq!(elide("abcdef", 6), "abcdef");
    assert_eq!(elide("abcdefg", 6), "abc...");
    assert_eq!(elide("a\tb", 6), "a b");
    // Characters, not bytes: no cut inside a multi-byte letter.
    assert_eq!(elide("ééééééé", 6), "ééé...");
}

#[test]
fn rows_without_names_keep_the_count_wording() {
    let m = connected(vec![peer(1, "Phone", true)], vec![named("t1", &[], 3)]);
    assert_eq!(view(&m).transfers[0].title, "3 files to Phone");
}

#[test]
fn names_survive_a_refresh_that_lacks_them_and_an_offer_names_its_top_level() {
    let mut m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &["Photos"], 50)],
    );
    m.apply(Input::Transfers(vec![named("t1", &[], 50)]), NOW);
    assert_eq!(view(&m).transfers[0].title, "Photos (50 files) to Phone");
    let file = |p: &str| OfferedFile {
        path: p.to_owned(),
        size: 1,
    };
    m.apply(
        host(HostEvent::TransferOffer {
            id: "r1".into(),
            peer: id(1),
            short_id: "S1".into(),
            files: vec![file("Trip/a.jpg"), file("Trip/b.jpg"), file("todo.txt")],
            file_count: 3,
            total_bytes: 3,
            note: None,
        }),
        NOW,
    );
    let row = view(&m)
        .transfers
        .into_iter()
        .find(|t| t.id == "r1")
        .expect("offer row");
    assert_eq!(row.title, "Trip and todo.txt (3 files) from Phone");
}

#[test]
fn a_single_file_in_a_folder_keeps_its_own_name_across_a_refresh() {
    let mut m = connected(vec![peer(1, "Phone", true)], vec![]);
    m.note_sent("t1", &[PathBuf::from("/home/me/Photos/a.jpg")]);
    let mut s = named("t1", &["Photos"], 1);
    s.names = vec!["Photos".into()];
    m.apply(Input::Transfers(vec![s.clone()]), NOW);
    assert_eq!(view(&m).transfers[0].title, "a.jpg to Phone");
    m.apply(Input::Transfers(vec![s]), NOW);
    assert_eq!(view(&m).transfers[0].title, "a.jpg to Phone");
}

#[test]
fn battery_from_a_new_daemon_is_authoritative_and_from_an_old_one_is_not() {
    assert!(!answer_has_battery("0.8.0"));
    assert!(answer_has_battery("0.9.0"));
    assert!(answer_has_battery("0.10.0"));
    assert!(answer_has_battery("1.0.0"));
    assert!(!answer_has_battery("garbage"));
    assert!(!answer_has_battery(""));
    for (version, want) in [("0.9.0", "Online"), ("0.8.0", "Online, battery 55%")] {
        let mut st = status(&[]);
        st.daemon_version = version.into();
        let mut m = Model::new();
        m.apply(
            Input::Connected {
                status: st,
                peers: vec![peer(1, "Phone", true)],
                transfers: vec![],
            },
            NOW,
        );
        m.apply(
            host(HostEvent::Battery {
                peer: id(1),
                level: 55,
                charging: false,
                low: false,
            }),
            NOW,
        );
        // A refresh without a report (missed offline and online events).
        m.apply(Input::Peers(vec![peer(1, "Phone", true)]), NOW);
        assert_eq!(view(&m).devices[0].detail, want);
    }
}

#[test]
fn invisible_and_bidi_characters_are_dropped_before_measuring() {
    assert_eq!(elide("a\u{200B}b\u{202E}c\u{FEFF}d\u{2066}e", 5), "abcde");
    // They do not use up the budget.
    assert_eq!(elide("\u{200B}\u{200B}abcdef", 6), "abcdef");
}

#[test]
fn the_count_is_never_cut_off_by_long_names() {
    let a = "a".repeat(40);
    let b = "b".repeat(40);
    let m = connected(
        vec![peer(1, "Phone", true)],
        vec![named("t1", &[&a, &b], 5001)],
    );
    let title = view(&m).transfers[0].title.clone();
    assert!(title.ends_with(" (5,001 files) to Phone"), "{title}");
    assert!(title.contains("..."), "{title}");
    assert!(
        title.chars().count() <= WHAT_SHOWN + " to Phone".len(),
        "{title}"
    );
}

#[test]
fn more_invisible_characters_go_and_line_separators_become_spaces() {
    assert_eq!(
        elide("a\u{AD}b\u{34F}c\u{61C}d\u{180E}e\u{2060}f\u{2064}g", 20),
        "abcdefg"
    );
    assert_eq!(elide("a\u{2028}b\u{2029}c", 20), "a b c");
}

fn update_status(state: UpdateState) -> UpdateStatus {
    UpdateStatus {
        state,
        last_checked_ms: None,
        last_failed_ms: None,
        last_failure: None,
        running_version: "0.11.0".into(),
        unavailable: None,
    }
}

fn available(version: &str) -> UpdateStatus {
    update_status(UpdateState::Available {
        offer: UpdateOffer {
            version: version.into(),
            page_url: "https://example.invalid/r".into(),
            can_install: true,
            reason: None,
        },
    })
}

#[test]
fn an_available_update_shows_one_line_and_other_phases_show_nothing() {
    let mut m = connected(vec![], vec![]);
    assert_eq!(view(&m).update_line, None);
    m.apply(Input::Update(Box::new(available("0.12.0"))), 0);
    assert_eq!(
        view(&m).update_line.as_deref(),
        Some("PlayDrop 0.12.0 is available")
    );
    for state in [
        UpdateState::Off,
        UpdateState::Idle,
        UpdateState::Checking,
        UpdateState::UpToDate,
        UpdateState::Downloading {
            version: "0.12.0".into(),
            done: 1,
            total: 2,
        },
        UpdateState::Other,
    ] {
        m.apply(Input::Update(Box::new(available("0.12.0"))), 0);
        m.apply(Input::Update(Box::new(update_status(state))), 0);
        assert_eq!(view(&m).update_line, None);
    }
}

#[test]
fn the_update_line_follows_events_live() {
    let mut m = connected(vec![], vec![]);
    m.apply(daemon(DaemonEvent::UpdateChanged(available("0.12.0"))), 0);
    assert!(view(&m).update_line.is_some());
    // "Later" in the window hides it again.
    m.apply(
        daemon(DaemonEvent::UpdateChanged(update_status(UpdateState::Idle))),
        0,
    );
    assert_eq!(view(&m).update_line, None);
}

#[test]
fn the_update_line_goes_with_the_daemon_and_needs_a_connection() {
    let mut m = Model::new();
    m.apply(Input::Update(Box::new(available("0.12.0"))), 0);
    assert_eq!(view(&m).update_line, None);
    let mut m = connected(vec![], vec![]);
    m.apply(Input::Update(Box::new(available("0.12.0"))), 0);
    m.apply(Input::Disconnected, 0);
    assert_eq!(view(&m).update_line, None);
    m.apply(
        Input::Connected {
            status: status(&[]),
            peers: vec![],
            transfers: vec![],
        },
        0,
    );
    assert_eq!(view(&m).update_line, None);
}

#[test]
fn a_long_offered_version_is_cut() {
    let mut m = connected(vec![], vec![]);
    m.apply(Input::Update(Box::new(available(&"9".repeat(80)))), 0);
    let line = view(&m).update_line.expect("line");
    assert!(line.chars().count() < 60);
}
