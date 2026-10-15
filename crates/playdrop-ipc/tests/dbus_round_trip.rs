//! D-Bus transport end to end on a private `dbus-daemon` (Linux only).
//!
//! Skips with a message when `dbus-daemon` is not installed, so CI without it still passes.
//! The server runs inside this test binary, so the client's `/proc/<pid>/exe` check is turned
//! off with `ClientOptions::verify_exe`; one case keeps it on and expects a refusal.
#![cfg(target_os = "linux")]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use playdrop_core::host::{Base64Bytes, HostEvent, OutboxItem, Secret};
use playdrop_ipc::client::{Client, ClientOptions, ConnectError};
use playdrop_ipc::server::{ServeError, Server, ServerOptions};
use playdrop_ipc::{
    DaemonEvent, ErrorCode, Event, IpcError, Ok, Request, RequestHandler, Response, Settings,
    Status, DBUS_INTERFACE, DBUS_NAME, DBUS_PATH, IPC_VERSION,
};
use tokio::sync::broadcast;

/// A private bus, killed on drop.
struct Bus {
    child: Child,
    address: String,
}

impl Bus {
    fn start() -> Option<Self> {
        let mut child = match Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Result::Ok(child) => child,
            Err(e) => {
                eprintln!("skipping: cannot start dbus-daemon ({e})");
                return None;
            }
        };
        let stdout = child.stdout.take().expect("piped stdout");
        let mut line = String::new();
        BufReader::new(stdout)
            .read_line(&mut line)
            .expect("read bus address");
        let address = line.trim().to_owned();
        assert!(!address.is_empty(), "dbus-daemon printed no address");
        Some(Self { child, address })
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn status() -> Status {
    Status {
        device_id: "cd".repeat(32),
        short_id: "K3QD-7XPA".into(),
        addr: "127.0.0.1:42187".parse().expect("addr"),
        daemon_version: "test".into(),
        ipc_version: IPC_VERSION,
        settings: Settings {
            receive_dir: "/tmp/in".into(),
            device_name: "alpha".into(),
            clipboard_sync: true,
            clipboard_images: true,
            clipboard_watch_supported: true,
            remote_input_supported: false,
            remote_input_reason: None,
            autostart: false,
            autostart_refused: None,
            notifications: true,
        },
        add_device_window_open: false,
        locked: false,
        input_from: vec![],
    }
}

fn outbox_item() -> OutboxItem {
    OutboxItem {
        id: "o1".into(),
        peer: "ab".repeat(32),
        file_count: 3,
        total_bytes: 4096,
        note: Some("for later".into()),
        created_at_ms: 1_760_000_000_000,
    }
}

/// Answers `Status` and the outbox requests; echoes `SendFiles` and `QueueOutbox` paths one
/// per line inside the error message, after the op name and the peer.
fn handler() -> Arc<dyn RequestHandler> {
    Arc::new(|request: Request| async move {
        match request {
            Request::Status => Response::Ok(Ok::Status(status())),
            Request::Outbox => Response::Ok(Ok::Outbox(vec![outbox_item()])),
            Request::RemoveOutbox { id } if id == "o1" => Response::Ok(Ok::Done),
            Request::RemoveOutbox { .. } => Response::err(ErrorCode::NotFound, "no such item"),
            Request::PullOutbox { peer } => Response::Ok(Ok::OutboxPulled { ids: vec![peer] }),
            Request::SendFiles { peer, paths } => echo("send_files", peer, &paths),
            Request::QueueOutbox { peer, paths } => echo("queue_outbox", peer, &paths),
            _ => Response::err(ErrorCode::InvalidState, "not in this test"),
        }
    })
}

fn echo(op: &str, peer: String, paths: &[PathBuf]) -> Response {
    let mut lines = vec![op.to_owned(), peer];
    lines.extend(paths.iter().map(|p| p.display().to_string()));
    Response::Err(IpcError::new(ErrorCode::NotFound, lines.join("\n")))
}

fn client_options(bus: &Bus, verify_exe: bool) -> ClientOptions {
    ClientOptions {
        verify_exe,
        bus_address: Some(bus.address.clone()),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dbus_round_trip() {
    let Some(bus) = Bus::start() else { return };
    let server_options = ServerOptions {
        bus_address: Some(bus.address.clone()),
        daemon_version: Some("9.9.9-test".into()),
    };

    // Nobody owns the name and no activation file exists on this bus.
    let none = Client::connect_with(client_options(&bus, false)).await;
    assert!(
        matches!(none, Err(ConnectError::NotRunning)),
        "expected NotRunning, got {:?}",
        none.err()
    );

    let (events, _) = broadcast::channel(64);
    let server = Server::start_with(handler(), events.clone(), server_options.clone())
        .await
        .expect("server starts");

    // Single instance.
    let second = Server::start_with(handler(), events.clone(), server_options.clone()).await;
    assert!(
        matches!(second, Err(ServeError::AlreadyRunning)),
        "expected AlreadyRunning, got {:?}",
        second.err()
    );

    // The exe check refuses a daemon that is not `playdropd` (here: the test binary).
    let strict = Client::connect_with(client_options(&bus, true)).await;
    assert!(
        matches!(strict, Err(ConnectError::Unverified(_))),
        "expected Unverified, got {:?}",
        strict.err()
    );

    let client = Client::connect_with(client_options(&bus, false))
        .await
        .expect("client connects");
    assert_eq!(client.daemon_version(), "9.9.9-test");

    // Request round trip.
    let answer = client.request(Request::Status).await.expect("status");
    assert_eq!(answer, Ok::Status(status()));
    let refused = client.request(Request::Peers).await.expect_err("unhandled");
    assert_eq!(refused.code, ErrorCode::InvalidState);

    check_send_files(&client).await;
    check_outbox(&client).await;
    check_raw_refusals(&bus).await;
    check_host_events(&client, &events).await;

    // Events reach the client.
    let mut stream = client.events();
    events
        .send(Event::Daemon(DaemonEvent::LockChanged { locked: true }))
        .expect("send event");
    let got = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("event in time")
        .expect("stream open")
        .expect("not lagged");
    assert_eq!(
        got,
        Event::Daemon(DaemonEvent::LockChanged { locked: true })
    );

    // The daemon leaving ends the event stream and later requests fail cleanly.
    server.shutdown().await;
    let end = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("stream ends in time");
    assert!(end.is_none(), "stream should end when the daemon leaves");
    let gone = client.request(Request::Status).await.expect_err("gone");
    assert_eq!(gone.code, ErrorCode::Internal);

    // The name is free again.
    let restarted = Server::start_with(handler(), events, server_options)
        .await
        .expect("restart after shutdown");
    drop(restarted);
}

/// Files travel as descriptors and reach the handler as their real paths.
async fn check_send_files(client: &Client) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("photo.jpg");
    std::fs::write(&file, b"jpeg").expect("write");
    let folder = tempfile::tempdir().expect("tempdir");
    let expected: Vec<String> = [&file, &folder.path().to_path_buf()]
        .iter()
        .map(|p| {
            std::fs::canonicalize(p)
                .expect("canon")
                .display()
                .to_string()
        })
        .collect();

    let echoed = client
        .send_files("k3qd", vec![file.clone(), folder.path().to_path_buf()])
        .await
        .expect_err("echo comes back as an error");
    let mut lines = echoed.message.lines();
    assert_eq!(lines.next(), Some("send_files"));
    assert_eq!(lines.next(), Some("k3qd"));
    assert_eq!(lines.map(str::to_owned).collect::<Vec<_>>(), expected);

    // `request(SendFiles)` takes the same descriptor path.
    let echoed = client
        .request(Request::SendFiles {
            peer: "k3qd".into(),
            paths: vec![file.clone()],
        })
        .await
        .expect_err("echo");
    assert_eq!(echoed.message.lines().nth(2), Some(expected[0].as_str()));

    // A missing path fails on the client, before the bus.
    let missing = client
        .send_files("k3qd", vec![PathBuf::from("/nonexistent/playdrop-test")])
        .await
        .expect_err("missing");
    assert_eq!(missing.code, ErrorCode::NotFound);
}

/// Outbox requests round trip; queued files travel as descriptors through
/// `QueueOutboxFiles`, both from `queue_outbox` and from `request(QueueOutbox)`.
async fn check_outbox(client: &Client) {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("clip.mov");
    std::fs::write(&file, b"mov").expect("write");
    let expected = std::fs::canonicalize(&file)
        .expect("canon")
        .display()
        .to_string();

    let echoed = client
        .queue_outbox("iphone", vec![file.clone()])
        .await
        .expect_err("echo");
    let lines: Vec<&str> = echoed.message.lines().collect();
    assert_eq!(lines, ["queue_outbox", "iphone", expected.as_str()]);
    let echoed = client
        .request(Request::QueueOutbox {
            peer: "iphone".into(),
            paths: vec![file],
        })
        .await
        .expect_err("echo");
    assert_eq!(echoed.message.lines().nth(2), Some(expected.as_str()));
    let empty = client
        .queue_outbox("iphone", Vec::new())
        .await
        .expect_err("empty");
    assert_eq!(empty.code, ErrorCode::BadRequest);

    let list = client.request(Request::Outbox).await.expect("outbox");
    assert_eq!(list, Ok::Outbox(vec![outbox_item()]));
    let removed = client
        .request(Request::RemoveOutbox { id: "o1".into() })
        .await
        .expect("remove");
    assert_eq!(removed, Ok::Done);
    let missing = client
        .request(Request::RemoveOutbox { id: "zz".into() })
        .await
        .expect_err("unknown item");
    assert_eq!(missing.code, ErrorCode::NotFound);
    let pulled = client
        .request(Request::PullOutbox { peer: "mac".into() })
        .await
        .expect("pull");
    assert_eq!(
        pulled,
        Ok::OutboxPulled {
            ids: vec!["mac".into()]
        }
    );
}

/// Phase 3 host events reach a client intact: image clips (base64 on the wire), outbox
/// notices, find and presence.
async fn check_host_events(client: &Client, events: &broadcast::Sender<Event>) {
    let mut stream = client.events();
    let sent = [
        Event::Host(HostEvent::ClipboardImage {
            peer: "ab".repeat(32),
            mime: "image/png".into(),
            data: Secret(Base64Bytes(vec![0x89, b'P', b'N', b'G', 0, 255])),
            sensitive: true,
        }),
        Event::Host(HostEvent::OutboxReady {
            peer: "ab".repeat(32),
            count: 2,
            bytes: 734_003_200,
            ids: vec!["o1".into(), "o2".into()],
        }),
        Event::Host(HostEvent::Find {
            peer: "ab".repeat(32),
            start: true,
        }),
        Event::Host(HostEvent::Presence {
            peer: "ab".repeat(32),
            state: "asleep".into(),
        }),
    ];
    for event in &sent {
        events.send(event.clone()).expect("send event");
    }
    for event in sent {
        let got = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("event in time")
            .expect("stream open")
            .expect("not lagged");
        assert_eq!(got, event);
    }
}

/// A raw caller passing a non-file handle, a path-based send or garbage is refused.
async fn check_raw_refusals(bus: &Bus) {
    let raw = zbus::connection::Builder::address(bus.address.as_str())
        .expect("address")
        .build()
        .await
        .expect("raw connection");
    let null = std::fs::File::open("/dev/null").expect("open null");
    for method in ["SendFiles", "QueueOutboxFiles"] {
        let reply = raw
            .call_method(
                Some(DBUS_NAME),
                DBUS_PATH,
                Some(DBUS_INTERFACE),
                method,
                &("k3qd", vec![zbus::zvariant::Fd::from(&null)]),
            )
            .await
            .expect("call");
        assert_bad_request(&reply);
    }

    let file = tempfile::NamedTempFile::new().expect("tempfile");
    let path_send = serde_json::to_string(&Request::SendFiles {
        peer: "k3qd".into(),
        paths: vec![file.path().to_path_buf()],
    })
    .expect("json");
    let path_queue = serde_json::to_string(&Request::QueueOutbox {
        peer: "k3qd".into(),
        paths: vec![file.path().to_path_buf()],
    })
    .expect("json");
    for body in [path_send.as_str(), path_queue.as_str(), "{not json"] {
        let reply = raw
            .call_method(
                Some(DBUS_NAME),
                DBUS_PATH,
                Some(DBUS_INTERFACE),
                "Request",
                &(body,),
            )
            .await
            .expect("call");
        assert_bad_request(&reply);
    }
}

fn assert_bad_request(reply: &zbus::Message) {
    let body: String = reply.body().deserialize().expect("string");
    let response: Response = serde_json::from_str(&body).expect("json");
    assert_eq!(response.error_code(), Some(ErrorCode::BadRequest));
}
