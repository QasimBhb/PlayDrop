//! Phase 8 host checks over loopback: a send is `offered` (and silent) until bytes move,
//! a declined offer reports no progress at all, and `files_done` on the send side counts
//! streamed files, bundle members and zero-length files at the right moments.

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{pair, start_host, temp_root, write_random_file};
use playdrop_core::host::{
    HostEvent, SendSource, SourceOpener, StreamSource, TransferDirection, TransferStatus,
};

const T: Duration = Duration::from_secs(30);

/// A stream source whose third and later opens wait until the test releases them. The
/// first open is the offer's metadata read and the second the resume check before
/// streaming; the third is the bundle reading the member, so the bundle stalls while the
/// streamed files finish. The count is tied to `transfer/sender.rs` (`check_source` in
/// `plan_resume`, then `FsOpener::open` for the member): an extra open there, or a
/// reconnect round, parks the sender at the gate and this test times out.
#[derive(Debug)]
struct GatedOpener {
    path: PathBuf,
    opens: AtomicU32,
    released: Arc<AtomicBool>,
}

impl SourceOpener for GatedOpener {
    fn open(&self) -> std::io::Result<std::fs::File> {
        if self.opens.fetch_add(1, Ordering::SeqCst) >= 2 {
            while !self.released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::fs::File::open(&self.path)
    }
}

/// Opens the gate when dropped, so a failing test never leaves a blocking thread behind.
struct Release(Arc<AtomicBool>);

impl Drop for Release {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The `files_done` of every send-side progress event for `id`, in order.
fn send_progress(events: &[HostEvent], id: &str) -> Vec<(u64, usize)> {
    events
        .iter()
        .filter_map(|e| match e {
            HostEvent::TransferProgress {
                id: i,
                direction: TransferDirection::Send,
                bytes,
                files_done,
                ..
            } if i == id => Some((*bytes, *files_done)),
            _ => None,
        })
        .collect()
}

/// A pending offer is `offered` with no progress events while the receiver decides; once
/// accepted it turns `active` only when bytes move. `files_done` counts the three streamed
/// files while the bundle is held back, then every file (bundle members and the empty
/// file included) in the last reading before `TransferComplete`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one send followed from offer to completion"
)]
async fn send_is_offered_until_streaming_and_counts_files_done() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();

    // Three streamed files, two bundled ones, one empty file, plus a bundled stream whose
    // reading the test holds back: seven files.
    let dir = root.path().join("src").join("Mix");
    std::fs::create_dir_all(&dir).unwrap();
    for (n, name) in ["a.bin", "b.bin", "c.bin"].iter().enumerate() {
        write_random_file(&dir.join(name), 300 * 1024, 30 + n as u64);
    }
    write_random_file(&dir.join("s1.txt"), 1000, 40);
    write_random_file(&dir.join("s2.txt"), 5000, 41);
    std::fs::write(dir.join("zero.txt"), b"").unwrap();
    let gated_path = root.path().join("src").join("gated.txt");
    write_random_file(&gated_path, 2000, 42);
    let released = Arc::new(AtomicBool::new(false));
    let _release = Release(Arc::clone(&released));
    let gated = StreamSource {
        name: "gated.txt".into(),
        size: 2000,
        mtime_ms: 0,
        mime: None,
        opener: Arc::new(GatedOpener {
            path: gated_path,
            opens: AtomicU32::new(0),
            released: Arc::clone(&released),
        }),
    };
    let file_count = 7;

    let mut handle = a
        .host
        .send_sources(
            &b.host.device_id(),
            vec![SendSource::Path(dir), SendSource::Stream(gated)],
            None,
        )
        .await
        .unwrap();
    let id = handle.id().to_owned();
    b.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::TransferOffer { id: oid, .. } if *oid == id),
        )
        .await;
    // Several ticker periods pass while the receiver decides.
    tokio::time::sleep(Duration::from_millis(1600)).await;
    a.events.drain();
    assert!(
        send_progress(&a.events.seen, &id).is_empty(),
        "no progress while the offer waits"
    );
    let snap = a.host.transfers().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(snap.state, TransferStatus::Offered);
    assert_eq!(snap.file_count, file_count);
    assert_eq!((snap.bytes, snap.files_done), (0, 0));

    b.host.respond_offer(&id, true).await.unwrap();
    // The streamed files finish while the bundle waits for its last member.
    a.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::TransferProgress { id: i, files_done: 3, .. } if *i == id),
        )
        .await;
    let first = send_progress(&a.events.seen, &id)[0];
    assert!(first.0 > 0, "the first reading comes with bytes moving");
    let snap = a.host.transfers().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(snap.state, TransferStatus::Active);
    assert!(
        0 < snap.files_done && snap.files_done < file_count,
        "mid-way the count is between 0 and {file_count}: {}",
        snap.files_done
    );
    assert_eq!(snap.files_done, 3, "the snapshot matches the last reading");

    released.store(true, Ordering::SeqCst);
    tokio::time::timeout(T, handle.wait())
        .await
        .expect("send finished in time")
        .expect("send succeeded");
    a.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferComplete { id: i, direction: TransferDirection::Send, .. } if *i == id)
        })
        .await;
    let complete_at = a
        .events
        .seen
        .iter()
        .position(|e| matches!(e, HostEvent::TransferComplete { id: i, direction: TransferDirection::Send, .. } if *i == id))
        .unwrap();
    let readings = send_progress(&a.events.seen[..complete_at], &id);
    assert!(
        readings.windows(2).all(|w| w[0].1 <= w[1].1),
        "files_done never goes back within a round: {readings:?}"
    );
    let total = 3 * 300 * 1024 + 1000 + 5000 + 2000;
    assert_eq!(
        *readings.last().unwrap(),
        (total, file_count),
        "the last reading before TransferComplete counts every file"
    );
    let snap = a.host.transfers().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(snap.state, TransferStatus::Complete);
    assert_eq!(snap.files_done, file_count);

    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A declined offer ends as cancelled on the sender without a single progress event.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(clippy::many_single_char_names, reason = "a, b are the hosts")]
async fn declined_offer_reports_no_progress() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "alpha", 0).await;
    let mut b = start_host(root.path(), "bravo", 0).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();

    let src = root.path().join("doc.bin");
    write_random_file(&src, 400 * 1024, 5);
    let mut handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let id = handle.id().to_owned();
    b.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::TransferOffer { id: oid, .. } if *oid == id),
        )
        .await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    b.host.respond_offer(&id, false).await.unwrap();
    let r = tokio::time::timeout(T, handle.wait())
        .await
        .expect("sender stopped in time");
    assert!(r.is_err());
    a.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferFailed { id: i, direction: TransferDirection::Send, .. } if *i == id)
        })
        .await;
    a.events.drain();
    assert!(
        send_progress(&a.events.seen, &id).is_empty(),
        "a declined offer never reports progress"
    );
    let snap = a.host.transfers().into_iter().find(|t| t.id == id).unwrap();
    assert_eq!(snap.state, TransferStatus::Cancelled);
    assert_eq!((snap.bytes, snap.files_done), (0, 0));

    a.host.shutdown().await;
    b.host.shutdown().await;
}
