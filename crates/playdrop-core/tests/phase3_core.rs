//! Phase 3 core API freeze over loopback (`docs/core-api.md`): long clipboard text through
//! a blob (protocol.md 5.15), a `Staged` receiver with a sink (section 2), sending from
//! stream openers (section 3), the outbox (7.8) and battery at session start (5.8).

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{file_root, pair, start_host, start_host_with, temp_root, write_random_file};
use playdrop_core::host::{
    DeliverItem, HostEvent, ReceiveSink, ReceiveTarget, SendSource, SinkError, SourceOpener,
    StreamSource, TransferDirection,
};

const T: Duration = Duration::from_secs(30);

/// protocol.md 5.7 and 5.15: a 100 KiB clip goes as a blob, is fetched, verified and
/// arrives intact; the receiver then treats it as applied (echo rule).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn long_clipboard_text_arrives_through_a_blob() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let mut rng = common::Rng(42);
    let text: String = (0..100 * 1024)
        .map(|_| char::from(b'a' + u8::try_from(rng.below(26)).unwrap()))
        .collect();
    let r = a.host.push_clipboard(&text, false).unwrap();
    assert_eq!(r.sent_to, [b.host.device_id()]);
    let ev = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { .. }))
        .await;
    let HostEvent::Clipboard { text: got, .. } = ev else {
        unreachable!()
    };
    assert!(got.0.as_deref() == Some(text.as_str()), "clip differs");
    assert!(b.host.push_clipboard(&text, false).unwrap().skipped_echo);

    // An image goes the same way and arrives as `ClipboardImage`.
    let png = vec![0x89u8; 300_000];
    let r = a
        .host
        .push_clipboard_image(png.clone(), "image/png", false)
        .unwrap();
    assert_eq!(r.sent_to.len(), 1);
    let ev = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::ClipboardImage { .. }))
        .await;
    let HostEvent::ClipboardImage { data, mime, .. } = ev else {
        unreachable!()
    };
    assert_eq!(mime, "image/png");
    assert!(data.0 .0 == png, "image differs");
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A sink that copies every delivered file into a folder, as an Android host would copy
/// into `MediaStore`.
#[derive(Debug)]
struct CopySink {
    dir: PathBuf,
    got: Mutex<Vec<DeliverItem>>,
}

impl ReceiveSink for CopySink {
    fn deliver(&self, item: DeliverItem, mut file: std::fs::File) -> Result<String, SinkError> {
        let dest = item.path.iter().fold(self.dir.clone(), |p, c| p.join(c));
        std::fs::create_dir_all(dest.parent().unwrap())
            .map_err(|e| SinkError::new(e.to_string()))?;
        let mut out = std::fs::File::create(&dest).map_err(|e| SinkError::new(e.to_string()))?;
        std::io::copy(&mut file, &mut out).map_err(|e| SinkError::new(e.to_string()))?;
        self.got.lock().unwrap().push(item);
        Ok(format!("sink://{}", dest.display()))
    }

    fn make_dir(&self, path: Vec<String>) -> Result<(), SinkError> {
        let dest = path.iter().fold(self.dir.clone(), |p, c| p.join(c));
        std::fs::create_dir_all(dest).map_err(|e| SinkError::new(e.to_string()))
    }
}

fn same_tree(src: &Path, got: &Path, names: &[&str]) {
    for n in names {
        let want = std::fs::read(src.join(n)).unwrap();
        let have = std::fs::read(got.join(n)).unwrap_or_else(|e| panic!("{n}: {e}"));
        assert!(want == have, "{n} differs");
    }
}

/// `docs/core-api.md` section 2: a `Staged` receiver verifies under its staging dir, hands
/// every file (streamed, bundled, empty) and empty folder to the sink, and reports
/// `delivered`; `saved` and `folder` stay empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn staged_receiver_delivers_through_the_sink() {
    common::init_logs();
    let root = temp_root();
    let out = root.path().join("sink-out");
    let sink = Arc::new(CopySink {
        dir: out.clone(),
        got: Mutex::new(Vec::new()),
    });
    let staging = root.path().join("bravo").join("staging");
    let mut a = start_host(root.path(), "alpha", 0).await;
    let s2 = Arc::clone(&sink);
    let st2 = staging.clone();
    let mut b = start_host_with(root.path(), "bravo", 0, move |c| {
        c.receive = ReceiveTarget::Staged {
            staging_dir: st2,
            sink: s2,
        };
    })
    .await;
    pair(&mut a, &mut b).await;

    let src = root.path().join("src").join("Album");
    std::fs::create_dir_all(src.join("empty")).unwrap();
    write_random_file(&src.join("big.bin"), 3 * 1024 * 1024 + 17, 1);
    write_random_file(&src.join("a.txt"), 900, 2);
    write_random_file(&src.join("b.txt"), 70_000, 3);
    std::fs::write(src.join("zero.txt"), b"").unwrap();
    let mut h = a
        .host
        .send(&b.host.device_id(), vec![src.clone()])
        .await
        .unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(60), h.wait())
        .await
        .unwrap()
        .unwrap();
    let ev = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::TransferComplete { .. }))
        .await;
    let HostEvent::TransferComplete {
        saved,
        folder,
        delivered,
        roots,
        ..
    } = ev
    else {
        unreachable!()
    };
    assert!(saved.is_empty() && folder.is_none());
    assert_eq!(delivered.len(), 4);
    assert!(delivered.iter().all(|d| d.location.starts_with("sink://")));
    let names = ["big.bin", "a.txt", "b.txt", "zero.txt"];
    same_tree(&src, &out.join("Album"), &names);
    assert!(out.join("Album").join("empty").is_dir());
    let big = roots.iter().find(|r| r.path == "Album/big.bin").unwrap();
    assert_eq!(
        big.root.as_deref(),
        Some(file_root(&src.join("big.bin"), 1024 * 1024).as_str())
    );
    assert_eq!(
        summary
            .roots
            .iter()
            .find(|r| r.path == "Album/big.bin")
            .unwrap()
            .root,
        big.root
    );
    assert_eq!(sink.got.lock().unwrap().len(), 4);
    // Only the staging folder (and its finished records) is left under the staging dir.
    let left: Vec<_> = std::fs::read_dir(&staging)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left, [".playdrop-partial"]);
    let snap = b.host.transfers();
    assert_eq!(snap[0].delivered.len(), 4);
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Opens a temp file the way a share target opens a `content://` item.
#[derive(Debug)]
struct FileOpener {
    path: PathBuf,
    opens: Mutex<u32>,
}

impl SourceOpener for FileOpener {
    fn open(&self) -> std::io::Result<std::fs::File> {
        *self.opens.lock().unwrap() += 1;
        std::fs::File::open(&self.path)
    }
}

fn stream_of(path: &Path, name: &str) -> (StreamSource, Arc<FileOpener>) {
    let opener = Arc::new(FileOpener {
        path: path.to_path_buf(),
        opens: Mutex::new(0),
    });
    let src = StreamSource {
        name: name.into(),
        size: std::fs::metadata(path).unwrap().len(),
        mtime_ms: 0,
        mime: Some("image/jpeg".into()),
        opener: opener.clone(),
    };
    (src, opener)
}

/// `docs/core-api.md` section 3: streams (one streamed, two bundled) next to a path, all
/// verified on the receiver; the openers were called for metadata and for the bytes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_from_stream_openers() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let dir = root.path().join("shared");
    std::fs::create_dir_all(&dir).unwrap();
    write_random_file(&dir.join("1"), 2 * 1024 * 1024 + 5, 11);
    write_random_file(&dir.join("2"), 1000, 12);
    write_random_file(&dir.join("3"), 5000, 13);
    write_random_file(&dir.join("plain.bin"), 400_000, 14);
    let (s1, o1) = stream_of(&dir.join("1"), "IMG_0001.jpg");
    let (s2, o2) = stream_of(&dir.join("2"), "note.txt");
    let (s3, _) = stream_of(&dir.join("3"), "tiny.jpg");
    let sources = vec![
        SendSource::Stream(s1),
        SendSource::Stream(s2),
        SendSource::Stream(s3),
        SendSource::Path(dir.join("plain.bin")),
    ];
    let mut h = a
        .host
        .send_sources(
            &b.host.device_id(),
            sources,
            Some("from the gallery".into()),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), h.wait())
        .await
        .unwrap()
        .unwrap();
    b.events
        .wait_for(T, |e| matches!(e, HostEvent::TransferComplete { .. }))
        .await;
    for (src, name) in [
        ("1", "IMG_0001.jpg"),
        ("2", "note.txt"),
        ("3", "tiny.jpg"),
        ("plain.bin", "plain.bin"),
    ] {
        let want = std::fs::read(dir.join(src)).unwrap();
        let got = std::fs::read(b.recv_dir.join(name)).unwrap();
        assert!(want == got, "{name} differs");
    }
    assert!(*o1.opens.lock().unwrap() >= 2, "metadata plus the PUT");
    assert!(
        *o2.opens.lock().unwrap() >= 2,
        "metadata plus the bundle member"
    );
    let t = &b.host.transfers()[0];
    assert_eq!(t.note.as_deref(), Some("from the gallery"));
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// protocol.md 7.8: A queues for B, B hears `outbox_ready`, pulls, every file verifies,
/// and the item is gone from A's outbox afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn outbox_queue_and_pull() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let src = root.path().join("src").join("Queued");
    std::fs::create_dir_all(&src).unwrap();
    write_random_file(&src.join("movie.bin"), 3 * 1024 * 1024 + 3, 21);
    write_random_file(&src.join("s1.txt"), 1234, 22);
    write_random_file(&src.join("s2.txt"), 99_000, 23);
    let b_id = b.host.device_id();
    let id = a
        .host
        .queue_outbox(
            &b_id,
            vec![SendSource::Path(src.clone())],
            Some("later".into()),
        )
        .await
        .unwrap();
    assert_eq!(a.host.outbox().len(), 1);
    assert_eq!(a.host.outbox()[0].file_count, 3);
    let ev = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::OutboxReady { .. }))
        .await;
    let HostEvent::OutboxReady { ids, count, .. } = ev else {
        unreachable!()
    };
    assert_eq!((count, ids.clone()), (1, vec![id.clone()]));
    // Persisted: only paths, so it is on disk.
    assert!(a
        .data_dir
        .join("outbox")
        .join(format!("{id}.json"))
        .is_file());

    let started = b.host.pull_outbox(&a.host.device_id()).await.unwrap();
    assert_eq!(started, std::slice::from_ref(&id));
    let ev = b
        .events
        .wait_for(Duration::from_secs(60), |e| {
            matches!(
                e,
                HostEvent::TransferComplete { .. } | HostEvent::TransferFailed { .. }
            )
        })
        .await;
    let HostEvent::TransferComplete { direction, .. } = ev else {
        panic!("pull failed: {ev:?}");
    };
    assert_eq!(direction, TransferDirection::Receive);
    same_tree(
        &src,
        &b.recv_dir.join("Queued"),
        &["movie.bin", "s1.txt", "s2.txt"],
    );
    // The puller's DELETE empties the holder's outbox.
    let deadline = tokio::time::Instant::now() + T;
    while !a.host.outbox().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "item not deleted");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(!a
        .data_dir
        .join("outbox")
        .join(format!("{id}.json"))
        .exists());
    assert!(a.host.remove_outbox(&id).await.is_err());
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// protocol.md 5.8: a remembered battery reaches a peer when the session starts; a
/// `charging` change goes at once; presence is remembered too (5.10).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn battery_is_sent_at_session_start() {
    use playdrop_core::session::types::PresenceState;
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    a.host.set_battery(64, false, false);
    a.host.set_presence(PresenceState::Idle, None, None);
    pair(&mut a, &mut b).await;
    b.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Battery {
                    level: 64,
                    charging: false,
                    ..
                }
            )
        })
        .await;
    b.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::Presence { state, .. } if state == "idle"),
        )
        .await;
    a.host.set_battery(65, true, false);
    b.events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::Battery {
                    level: 65,
                    charging: true,
                    ..
                }
            )
        })
        .await;
    a.host.shutdown().await;
    b.host.shutdown().await;
}
