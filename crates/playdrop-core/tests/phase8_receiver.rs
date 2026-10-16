//! Phase 8 receiver behaviour through the public transfer API (no network):
//!
//! - bundle members leave staging while the bundle streams, before the root arrives;
//! - a restart of the bundle at offset 0 never delivers a member twice;
//! - a root mismatch fails the bundle but keeps the members already placed;
//! - a delivery or placement I/O error keeps the verified data, is retried by the next
//!   upload request (answered `offset_mismatch` at the full size) or by a load, and never
//!   downloads anything again;
//! - `files_done` counts files as they reach their place;
//! - over two real hosts, a send survives a placement failure on the receiver once it
//!   clears, without sending the bundle again;
//! - an ignored benchmark showing that a slow sink overlaps the stream.
//!
//! Run the benchmark with
//! `cargo test -p playdrop-core --test phase8_receiver -- --ignored --nocapture`.

mod common;

use std::collections::HashMap;
use std::io::Read as _;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use playdrop_core::transfer::bundle::{MemberReader, OpenFuture};
use playdrop_core::transfer::manifest::new_transfer_id;
use playdrop_core::transfer::{
    bundle_size, root_hash, AcceptOptions, BundleInfo, BundleLayout, BundleWriter, ChunkEncoder,
    ChunkHash, DeliverItem, FileEntry, FileState, FreeSpace, Manifest, MemberOpener, PutError,
    ReceiveSink, SinkBinding, SinkError, SpaceInfo, SyncPolicy, TransferReceiver, TransferState,
};
use playdrop_core::Error;
use tokio::io::AsyncReadExt as _;

const CS: u32 = 64 * 1024;

#[derive(Debug)]
struct Plenty;

impl FreeSpace for Plenty {
    fn space(&self, _path: &Path) -> std::io::Result<SpaceInfo> {
        Ok(SpaceInfo {
            available: 1 << 50,
            total: 1 << 50,
        })
    }
}

fn plenty() -> Arc<dyn FreeSpace> {
    Arc::new(Plenty)
}

/// Deterministic bytes.
fn bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x.to_le_bytes()[0]
        })
        .collect()
}

/// A manifest of `sizes` (`true` = in the bundle) under folder `T`, with its contents.
fn manifest(sizes: &[(usize, bool)]) -> (Manifest, Vec<Vec<u8>>) {
    let contents: Vec<Vec<u8>> = sizes
        .iter()
        .enumerate()
        .map(|(i, &(s, _))| bytes(i as u64 + 1, s))
        .collect();
    let mut m = Manifest {
        id: new_transfer_id(),
        chunk_size: CS,
        total_bytes: contents.iter().map(|c| c.len() as u64).sum(),
        note: None,
        files: sizes
            .iter()
            .enumerate()
            .map(|(i, &(s, b))| FileEntry {
                fid: u32::try_from(i).unwrap(),
                path: vec!["T".into(), format!("f{i}.dat")],
                size: s as u64,
                mtime: 1_700_000_000_000,
                mime: None,
                in_bundle: b,
            })
            .collect(),
        dirs: Vec::new(),
        bundle: None,
    };
    if m.files.iter().any(|f| f.in_bundle) {
        m.bundle = Some(BundleInfo {
            size: bundle_size(&m),
        });
    }
    (m, contents)
}

struct Mem(Vec<Vec<u8>>);

impl MemberOpener for Mem {
    fn open(&self, fid: u32, off: u64) -> OpenFuture {
        let d = self.0[fid as usize][usize::try_from(off).unwrap()..].to_vec();
        Box::pin(async move { Ok(Box::new(std::io::Cursor::new(d)) as MemberReader) })
    }
}

async fn bundle_of(m: &Manifest, contents: &[Vec<u8>]) -> Vec<u8> {
    let layout = Arc::new(BundleLayout::new(m).unwrap());
    let mut w = BundleWriter::new(layout, Arc::new(Mem(contents.to_vec())), 0).unwrap();
    let mut v = Vec::new();
    w.read_to_end(&mut v).await.unwrap();
    v
}

fn hashes(data: &[u8]) -> Vec<ChunkHash> {
    data.chunks(CS as usize)
        .map(|c| *blake3::hash(c).as_bytes())
        .collect()
}

fn root_of(data: &[u8]) -> ChunkHash {
    root_hash(data.len() as u64, CS, &hashes(data))
}

/// Chunk records for `data[start..end]`.
async fn body(data: &[u8], start: u64, end: u64) -> Vec<u8> {
    let s = usize::try_from(start).unwrap();
    let e = usize::try_from(end).unwrap();
    let prior = hashes(&data[..s]);
    let mut enc = ChunkEncoder::new(&data[s..e], start, end, CS, prior).unwrap();
    let mut out = Vec::new();
    enc.read_to_end(&mut out).await.unwrap();
    out
}

/// Sends `data[start..end]` of the bundle in one request; `root` goes in the trailer.
async fn put_bundle(
    rx: &TransferReceiver,
    data: &[u8],
    start: u64,
    end: u64,
    root: Option<ChunkHash>,
) -> Result<FileState, PutError> {
    let token = rx.state().bundle.unwrap().token.unwrap();
    let mut up = rx.begin_bundle(&token, start, end - 1).await?;
    up.receive(&body(data, start, end).await[..]).await?;
    Ok(up.finish(root).await?.state)
}

/// Sends the whole of streamed file `fid`.
async fn put_file(rx: &TransferReceiver, fid: u32, data: &[u8]) -> Result<FileState, PutError> {
    let token = rx.state().files[fid as usize].token.clone().unwrap();
    let size = data.len() as u64;
    let mut up = rx.begin_file(fid, &token, 0, size - 1).await?;
    up.receive(&body(data, 0, size).await[..]).await?;
    Ok(up.finish(Some(root_of(data))).await?.state)
}

/// A sink that counts deliveries per path and can sleep in each one.
#[derive(Debug, Default)]
struct CountingSink {
    got: Mutex<HashMap<String, (usize, Vec<u8>)>>,
    delay: Duration,
}

impl CountingSink {
    fn count(&self) -> usize {
        self.got.lock().unwrap().values().map(|v| v.0).sum()
    }
}

impl ReceiveSink for CountingSink {
    fn deliver(&self, item: DeliverItem, mut file: std::fs::File) -> Result<String, SinkError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let mut v = Vec::new();
        file.read_to_end(&mut v).unwrap();
        let key = item.path.join("/");
        let mut got = self.got.lock().unwrap();
        let e = got.entry(key.clone()).or_default();
        e.0 += 1;
        e.1 = v;
        Ok(format!("content://test/{key}"))
    }

    fn make_dir(&self, _path: Vec<String>) -> Result<(), SinkError> {
        Ok(())
    }
}

fn staged(sink: Arc<CountingSink>, policy: SyncPolicy) -> AcceptOptions {
    AcceptOptions {
        sink: Some(SinkBinding {
            sink,
            peer: "ab".repeat(32),
        }),
        sync: policy,
        ..AcceptOptions::default()
    }
}

/// A checkpoint after every chunk, so members become durable chunk by chunk.
fn every_chunk() -> SyncPolicy {
    SyncPolicy {
        bytes: u64::from(CS),
        interval: Duration::from_secs(3600),
    }
}

/// Waits until `f` holds, for at most 10 s (the delivery worker runs on its own thread).
async fn until(mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(10), "timed out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Eight bundle members of 40,000 bytes: about five 64 KiB chunks in all, so the first
/// three chunks hold four whole members.
fn eight_members() -> (Manifest, Vec<Vec<u8>>) {
    manifest(&[(40_000, true); 8])
}

#[tokio::test]
async fn bundle_members_reach_the_sink_before_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, contents) = eight_members();
    let sink = Arc::new(CountingSink::default());
    let rx = TransferReceiver::accept(
        tmp.path(),
        m.clone(),
        plenty(),
        staged(sink.clone(), every_chunk()),
    )
    .await
    .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let part = 3 * u64::from(CS);
    assert_eq!(
        put_bundle(&rx, &bundle, 0, part, None).await.unwrap(),
        FileState::Receiving
    );
    // Members 0 to 3 lie wholly below the durable offset: they go out with no root yet.
    until(|| sink.count() == 4).await;
    until(|| rx.files_done() == 4).await;
    for i in 0..4 {
        let got = sink.got.lock().unwrap();
        assert_eq!(got[&format!("T/f{i}.dat")].1, contents[i]);
    }
    assert_eq!(rx.state().bundle.unwrap().state, FileState::Receiving);

    let total = bundle.len() as u64;
    assert_eq!(
        put_bundle(&rx, &bundle, part, total, Some(root_of(&bundle)))
            .await
            .unwrap(),
        FileState::Verified
    );
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    assert_eq!(rx.files_done(), 8);
    let got = sink.got.lock().unwrap();
    assert_eq!(got.len(), 8);
    for (i, c) in contents.iter().enumerate() {
        assert_eq!(
            got[&format!("T/f{i}.dat")],
            (1, c.clone()),
            "member {i} once"
        );
    }
    assert_eq!(rx.delivered().len(), 8);
}

#[tokio::test]
async fn a_bundle_restart_at_zero_never_delivers_a_member_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, contents) = eight_members();
    let sink = Arc::new(CountingSink::default());
    let rx = TransferReceiver::accept(
        tmp.path(),
        m.clone(),
        plenty(),
        staged(sink.clone(), every_chunk()),
    )
    .await
    .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    put_bundle(&rx, &bundle, 0, 3 * u64::from(CS), None)
        .await
        .unwrap();
    until(|| sink.count() == 4).await;
    // A sender that lost its chunk hashes starts the bundle again at 0.
    let total = bundle.len() as u64;
    assert_eq!(
        put_bundle(&rx, &bundle, 0, total, Some(root_of(&bundle)))
            .await
            .unwrap(),
        FileState::Verified
    );
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    let got = sink.got.lock().unwrap();
    for (i, c) in contents.iter().enumerate() {
        assert_eq!(
            got[&format!("T/f{i}.dat")],
            (1, c.clone()),
            "member {i} once"
        );
    }
    let delivered = rx.delivered();
    assert_eq!(delivered.len(), 8);
    assert_eq!(delivered[0].location, "content://test/T/f0.dat");
}

#[tokio::test]
async fn a_root_mismatch_keeps_the_members_already_placed() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = eight_members();
    let opts = AcceptOptions {
        sync: every_chunk(),
        ..AcceptOptions::default()
    };
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), opts)
        .await
        .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    put_bundle(&rx, &bundle, 0, 3 * u64::from(CS), None)
        .await
        .unwrap();
    until(|| rx.files_done() == 4).await;
    let r = put_bundle(&rx, &bundle, 3 * u64::from(CS), total, Some([7; 32])).await;
    assert!(
        matches!(r, Err(PutError::Core(Error::RootMismatch))),
        "{r:?}"
    );
    assert_eq!(rx.state().bundle.unwrap().state, FileState::Failed);
    assert_eq!(rx.transfer_state(), TransferState::Failed);
    for (i, c) in contents.iter().enumerate().take(4) {
        let p = root.join("T").join(format!("f{i}.dat"));
        assert_eq!(
            &std::fs::read(&p).unwrap(),
            c,
            "chunk-verified member {i} stays"
        );
    }
}

#[tokio::test]
async fn a_bundle_delivery_error_is_retried_without_a_new_download() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = eight_members();
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), AcceptOptions::default())
        .await
        .unwrap();
    // A plain file where the folder `T` must go: every placement fails.
    std::fs::write(root.join("T"), b"in the way").unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    let r = put_bundle(&rx, &bundle, 0, total, Some(root_of(&bundle))).await;
    assert!(r.is_err(), "{r:?}");
    let st = rx.state();
    let b = st.bundle.unwrap();
    assert_eq!(b.state, FileState::Receiving, "the data stays");
    assert_eq!(b.verified_offset, total);
    assert_eq!(st.state, TransferState::Active);

    // The obstacle goes away; a sender seeing `receiving` at the full offset restarts at 0
    // and is told where the receiver stands instead. Nothing is downloaded again.
    std::fs::remove_file(root.join("T")).unwrap();
    let token = rx.state().bundle.unwrap().token.unwrap();
    let r = rx.begin_bundle(&token, 0, total - 1).await;
    assert!(
        matches!(r, Err(PutError::Core(Error::OffsetMismatch { expected })) if expected == total),
        "{r:?}"
    );
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    for (i, c) in contents.iter().enumerate() {
        assert_eq!(
            &std::fs::read(root.join("T").join(format!("f{i}.dat"))).unwrap(),
            c
        );
    }
    assert_eq!(rx.saved_paths().len(), 8);
}

#[tokio::test]
async fn a_bundle_delivery_error_is_retried_by_a_load() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = eight_members();
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), AcceptOptions::default())
        .await
        .unwrap();
    std::fs::write(root.join("T"), b"in the way").unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    put_bundle(&rx, &bundle, 0, total, Some(root_of(&bundle)))
        .await
        .unwrap_err();
    drop(rx);
    std::fs::remove_file(root.join("T")).unwrap();
    let rx = TransferReceiver::load(&root, &m.id, plenty(), SyncPolicy::default())
        .await
        .unwrap();
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    for (i, c) in contents.iter().enumerate() {
        assert_eq!(
            &std::fs::read(root.join("T").join(format!("f{i}.dat"))).unwrap(),
            c
        );
    }
}

#[tokio::test]
async fn a_file_placement_error_is_retried_without_a_new_download() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = manifest(&[(3 * CS as usize + 11, false)]);
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), AcceptOptions::default())
        .await
        .unwrap();
    std::fs::write(root.join("T"), b"in the way").unwrap();
    let r = put_file(&rx, 0, &contents[0]).await;
    assert!(r.is_err(), "{r:?}");
    let size = contents[0].len() as u64;
    let f = &rx.state().files[0];
    assert_eq!((f.state, f.verified_offset), (FileState::Receiving, size));

    std::fs::remove_file(root.join("T")).unwrap();
    let token = f.token.clone().unwrap();
    let r = rx.begin_file(0, &token, 0, size - 1).await;
    assert!(
        matches!(r, Err(PutError::Core(Error::OffsetMismatch { expected })) if expected == size),
        "{r:?}"
    );
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    assert_eq!(
        std::fs::read(root.join("T").join("f0.dat")).unwrap(),
        contents[0]
    );
    assert_eq!(rx.files_done(), 1);
}

#[tokio::test]
async fn files_done_counts_each_kind_of_file() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = manifest(&[
        (2 * CS as usize, false), // streamed
        (0, false),               // empty, created at completion
        (500, false),             // skipped
        (40_000, true),
        (40_000, true),
        (40_000, true),
    ]);
    let opts = AcceptOptions {
        skip: vec![2],
        sync: every_chunk(),
        ..AcceptOptions::default()
    };
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), opts)
        .await
        .unwrap();
    assert_eq!(rx.files_done(), 1, "the skipped file");
    put_file(&rx, 0, &contents[0]).await.unwrap();
    assert_eq!(rx.files_done(), 2);
    let bundle = bundle_of(&m, &contents).await;
    put_bundle(&rx, &bundle, 0, u64::from(CS), None)
        .await
        .unwrap();
    // The first member ends below 64 KiB; the second does not.
    until(|| rx.files_done() == 3).await;
    let total = bundle.len() as u64;
    put_bundle(&rx, &bundle, u64::from(CS), total, Some(root_of(&bundle)))
        .await
        .unwrap();
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    assert_eq!(rx.files_done(), 6);
}

/// `Directory` mode: the destination folder is deleted while the bundle streams, so the
/// worker's kept folder handle fails at the last members, found only when `finish` drains.
/// `finish` retries once with a fresh worker and the transfer completes. Not on Windows:
/// a folder with an open handle cannot be deleted there (sharing violation), so the
/// situation the test builds cannot happen on that platform.
#[cfg(not(windows))]
#[tokio::test]
async fn a_worker_failure_found_late_in_finish_is_retried_there() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = eight_members();
    let opts = AcceptOptions {
        sync: every_chunk(),
        ..AcceptOptions::default()
    };
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), opts)
        .await
        .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    // Four chunks hold members 0 to 5; the rest arrives in one last short piece, handed
    // over only at the end of that request, right before `finish`.
    let part = 4 * u64::from(CS);
    put_bundle(&rx, &bundle, 0, part, None).await.unwrap();
    until(|| rx.files_done() == 6).await;
    std::fs::remove_dir_all(root.join("T")).unwrap();
    let state = put_bundle(&rx, &bundle, part, total, Some(root_of(&bundle)))
        .await
        .unwrap();
    assert_eq!(state, FileState::Verified);
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    for (i, c) in contents.iter().enumerate().skip(6) {
        assert_eq!(
            &std::fs::read(root.join("T").join(format!("f{i}.dat"))).unwrap(),
            c
        );
    }
    assert_eq!(rx.delivered().len(), 8);
}

/// A sink that takes `delay` per file and reports when it is inside `deliver`.
#[derive(Debug, Default)]
struct SlowSink {
    delay: Duration,
    count: std::sync::atomic::AtomicUsize,
    inside: std::sync::atomic::AtomicBool,
    /// Members after this many wait inside `deliver` until `released` is set, so a test
    /// can hold a member in flight with the rest queued, whatever the runner's speed.
    hold_after: Option<usize>,
    released: std::sync::atomic::AtomicBool,
}

impl ReceiveSink for SlowSink {
    fn deliver(&self, item: DeliverItem, _file: std::fs::File) -> Result<String, SinkError> {
        use std::sync::atomic::Ordering;
        self.inside.store(true, Ordering::SeqCst);
        if self
            .hold_after
            .is_some_and(|n| self.count.load(Ordering::SeqCst) >= n)
        {
            while !self.released.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        std::thread::sleep(self.delay);
        self.count.fetch_add(1, Ordering::SeqCst);
        self.inside.store(false, Ordering::SeqCst);
        Ok(item.path.join("/"))
    }

    fn make_dir(&self, _path: Vec<String>) -> Result<(), SinkError> {
        Ok(())
    }
}

fn slow_staged(sink: Arc<SlowSink>) -> AcceptOptions {
    AcceptOptions {
        sink: Some(SinkBinding {
            sink,
            peer: "cd".repeat(32),
        }),
        sync: every_chunk(),
        ..AcceptOptions::default()
    }
}

/// A cancel with thousands of members queued for a slow sink returns right after the
/// member in flight (no starvation on the worker's lock), leaves nothing in flight, and
/// no member reaches the sink afterwards. The upload still running after the cancel does
/// not create a delivery log again in the cleared staging folder.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_with_thousands_queued_returns_after_the_member_in_flight() {
    use std::sync::atomic::Ordering;
    const N: usize = 3_000;
    let tmp = tempfile::tempdir().unwrap();
    let (m, contents) = manifest(&vec![(200, true); N]);
    // Three members go through, the fourth is held inside the sink: one member in flight
    // and the rest queued however slowly the runner stores the upload (the Windows runner
    // once took long enough that 2,437 of 3,000 had gone out before the cancel).
    let sink = Arc::new(SlowSink {
        delay: Duration::from_millis(5),
        hold_after: Some(3),
        ..SlowSink::default()
    });
    let rx = TransferReceiver::accept(tmp.path(), m.clone(), plenty(), slow_staged(sink.clone()))
        .await
        .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    let token = rx.state().bundle.unwrap().token.unwrap();
    let mut up = rx.begin_bundle(&token, 0, total - 1).await.unwrap();
    up.receive(&body(&bundle, 0, total).await[..])
        .await
        .unwrap();
    drop(up);
    until(|| sink.count.load(Ordering::SeqCst) == 3 && sink.inside.load(Ordering::SeqCst)).await;
    let t0 = Instant::now();
    let rx2 = rx.clone();
    let cancel = tokio::spawn(async move { rx2.cancel().await });
    // The cancel must wait for the member in flight, so let it start waiting first.
    tokio::time::sleep(Duration::from_millis(50)).await;
    sink.released.store(true, Ordering::SeqCst);
    cancel.await.unwrap().unwrap();
    let took = t0.elapsed();
    assert!(
        !sink.inside.load(Ordering::SeqCst),
        "the member in flight finished first"
    );
    let at_cancel = sink.count.load(Ordering::SeqCst);
    assert!(took < Duration::from_secs(1), "cancel waited {took:?}");
    assert_eq!(
        at_cancel, 4,
        "only the member in flight finished after the cancel"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        sink.count.load(Ordering::SeqCst),
        at_cancel,
        "nothing after the cancel"
    );
    assert_eq!(rx.transfer_state(), TransferState::Cancelled);
    assert!(!rx.staging_dir().join("bundle.delivered").exists());
}

/// A restart at 0 while a member is in flight at a slow sink: the restart waits for that
/// member, drops the queue, and in the end every member reached the sink exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_restart_at_zero_with_a_member_in_flight_delivers_each_once() {
    let tmp = tempfile::tempdir().unwrap();
    let (m, contents) = eight_members();
    let sink = Arc::new(CountingSink {
        delay: Duration::from_millis(30),
        ..CountingSink::default()
    });
    let rx = TransferReceiver::accept(
        tmp.path(),
        m.clone(),
        plenty(),
        staged(sink.clone(), every_chunk()),
    )
    .await
    .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    put_bundle(&rx, &bundle, 0, 3 * u64::from(CS), None)
        .await
        .unwrap();
    // Member 0 is at the sink (30 ms), members 1 to 3 wait behind it.
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        put_bundle(&rx, &bundle, 0, total, Some(root_of(&bundle)))
            .await
            .unwrap(),
        FileState::Verified
    );
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    let got = sink.got.lock().unwrap();
    for (i, c) in contents.iter().enumerate() {
        assert_eq!(
            got[&format!("T/f{i}.dat")],
            (1, c.clone()),
            "member {i} once"
        );
    }
}

/// After a restart, progress counts the members placed before it at once.
#[tokio::test]
async fn files_done_counts_members_placed_before_a_load() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Downloads");
    let (m, contents) = eight_members();
    let opts = AcceptOptions {
        sync: every_chunk(),
        ..AcceptOptions::default()
    };
    let rx = TransferReceiver::accept(&root, m.clone(), plenty(), opts)
        .await
        .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    put_bundle(&rx, &bundle, 0, 3 * u64::from(CS), None)
        .await
        .unwrap();
    until(|| rx.files_done() == 4).await;
    drop(rx);
    let rx = TransferReceiver::load(&root, &m.id, plenty(), every_chunk())
        .await
        .unwrap();
    assert_eq!(rx.files_done(), 4);
}

/// Two real hosts: the receiver cannot place a bundle (a plain file stands where its
/// folder goes), so `finish` fails after every byte verified. The receiver answers
/// `409 offset_mismatch` at the full size (and a `500` would now re-plan too), the sender
/// keeps re-planning, and once the obstacle is gone the send completes with every file in
/// place once and without the bundle being received a second time.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_survives_a_placement_failure_on_the_receiver() {
    use playdrop_core::host::{HostEvent, TransferDirection};
    common::init_logs();
    let root = common::temp_root();
    let mut a = common::start_host(root.path(), "alpha", 0).await;
    let mut b = common::start_host(root.path(), "bravo", 0).await;
    common::pair(&mut a, &mut b).await;
    let dir = root.path().join("src").join("Batch");
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..20u64 {
        common::write_random_file(&dir.join(format!("f{i}.txt")), 3000 + i, 70 + i);
    }
    std::fs::create_dir_all(&b.recv_dir).unwrap();
    std::fs::write(b.recv_dir.join("Batch"), b"in the way").unwrap();

    let mut handle = a
        .host
        .send(&b.host.device_id(), vec![dir.clone()])
        .await
        .unwrap();
    // Wait until the receiver reports every byte verified (its progress reading comes
    // once a second), so placement has run into the obstacle, then clear it. A fixed delay
    // or the sender's own byte counter raced slow runners (Defender on Windows): the
    // obstacle went before the receiver tried to place anything. The sender keeps
    // re-planning for at least 10 s without progress, far longer than this wait.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let full = b.host.transfers().iter().any(|t| {
            t.direction == TransferDirection::Receive && t.total > 0 && t.bytes == t.total
        });
        if full {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the receiver never held every byte"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::remove_file(b.recv_dir.join("Batch")).unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(30), handle.wait())
        .await
        .expect("send finished")
        .expect("send completed");
    assert!(
        summary.reconnects >= 1,
        "the failure made the sender re-plan"
    );
    for i in 0..20u64 {
        let name = format!("f{i}.txt");
        assert_eq!(
            std::fs::read(b.recv_dir.join("Batch").join(&name)).unwrap(),
            std::fs::read(dir.join(&name)).unwrap()
        );
    }
    assert_eq!(
        std::fs::read_dir(b.recv_dir.join("Batch")).unwrap().count(),
        20
    );
    // No second download: the receiver's verified count never went back down.
    b.events.drain();
    let received: Vec<u64> = b
        .events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::TransferProgress {
                direction: TransferDirection::Receive,
                bytes,
                ..
            } => Some(*bytes),
            _ => None,
        })
        .collect();
    assert!(
        received.windows(2).all(|w| w[0] <= w[1]),
        "received bytes went back: {received:?}"
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Feeds `data` to the receiver at a steady pace: `piece` bytes every `every`.
fn paced(data: Vec<u8>, piece: usize, every: Duration) -> tokio::io::DuplexStream {
    let (mut w, r) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        use tokio::io::AsyncWriteExt as _;
        for p in data.chunks(piece) {
            w.write_all(p).await.unwrap();
            tokio::time::sleep(every).await;
        }
    });
    r
}

/// 5,000 members into a sink that takes 0.5 ms per file, with the bundle arriving over
/// about 3 s. Handing members over while the bundle streams should hide almost all of
/// the 2.5 s of sink work behind the stream: before Phase 8 it all came after the root.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; prints its numbers with --nocapture"]
async fn bench_slow_sink_overlaps_the_stream() {
    const N: usize = 5_000;
    const SINK_DELAY: Duration = Duration::from_micros(500);
    let tmp = tempfile::tempdir().unwrap();
    let (m, contents) = manifest(&vec![(1_000, true); N]);
    let sink = Arc::new(CountingSink {
        delay: SINK_DELAY,
        ..CountingSink::default()
    });
    let rx = TransferReceiver::accept(
        tmp.path(),
        m.clone(),
        plenty(),
        staged(sink.clone(), SyncPolicy::default()),
    )
    .await
    .unwrap();
    let bundle = bundle_of(&m, &contents).await;
    let total = bundle.len() as u64;
    let records = body(&bundle, 0, total).await;
    // About 3 s of stream, a little more than the sink's serial work.
    let pieces = 300;
    let piece = records.len().div_ceil(pieces);
    let token = rx.state().bundle.unwrap().token.unwrap();

    let t0 = Instant::now();
    let mut up = rx.begin_bundle(&token, 0, total - 1).await.unwrap();
    up.receive(paced(records, piece, Duration::from_millis(10)))
        .await
        .unwrap();
    let stream = t0.elapsed();
    let at_stream_end = sink.count();
    assert_eq!(
        up.finish(Some(root_of(&bundle))).await.unwrap().state,
        FileState::Verified
    );
    let wall = t0.elapsed();
    let tail = wall.saturating_sub(stream);
    let sink_work = SINK_DELAY * u32::try_from(N).unwrap();
    println!(
        "{N} members, bundle {:.1} MiB: stream {:.2} s, wall {:.2} s, after the root {:.2} s; \
         sink work {:.2} s serial; members already delivered when the stream ended: {at_stream_end}",
        f64::from(u32::try_from(total).unwrap()) / (1024.0 * 1024.0),
        stream.as_secs_f64(),
        wall.as_secs_f64(),
        tail.as_secs_f64(),
        sink_work.as_secs_f64(),
    );
    assert_eq!(sink.count(), N);
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    assert!(
        wall < stream + sink_work * 3 / 4,
        "delivery should overlap the stream: wall {wall:?}, stream {stream:?}, sink {sink_work:?}"
    );
}
