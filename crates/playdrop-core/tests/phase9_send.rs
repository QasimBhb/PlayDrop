//! Phase 9 sender persistence over loopback (protocol.md 7.6 "sender persistence",
//! `docs/core-api.md` section 12): a send survives a restart of the sending host, a send
//! that lost its receiver resumes when the receiver is back or when the same files are
//! sent again, a cancelled waiting send never resumes, and a receiver that forgot the
//! transfer gets it offered again under the same ID.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{file_root, init_logs, pair, start_host, start_host_with, temp_root, TestHost};
use playdrop_core::host::{
    HostEvent, SendSource, SourceOpener, StreamSource, TransferDirection, TransferSnapshot,
    TransferStatus,
};

const MIB: u64 = 1024 * 1024;
const SIZE: u64 = 64 * MIB;
/// Where the tests interrupt a send: far enough in that the receiver holds verified
/// chunks, far enough from the end that the send is still running.
const CUT_AT: u64 = 8 * MIB;
const CHUNK: u32 = 1024 * 1024;
const T: Duration = Duration::from_secs(120);
/// The sender's reconnect window in the tests that let a send give up.
const SHORT: Duration = Duration::from_secs(2);

/// The part of a record that changes as the send goes (counts, verified streams).
fn state_path(host: &TestHost, id: &str) -> PathBuf {
    host.data_dir.join("outgoing").join(format!("{id}.state"))
}

/// Every file of a record, read now (as a sender killed at this moment leaves them).
fn copy_record(data_dir: &Path, id: &str) -> Vec<(PathBuf, Vec<u8>)> {
    ["json", "state", "hashes"]
        .iter()
        .map(|ext| data_dir.join("outgoing").join(format!("{id}.{ext}")))
        .filter_map(|p| std::fs::read(&p).ok().map(|b| (p, b)))
        .collect()
}

fn restore_record(copy: &[(PathBuf, Vec<u8>)]) {
    for (p, b) in copy {
        std::fs::write(p, b).unwrap();
    }
}

fn record_path(host: &TestHost, id: &str) -> PathBuf {
    host.data_dir.join("outgoing").join(format!("{id}.json"))
}

fn snapshot(host: &TestHost, id: &str, dir: TransferDirection) -> Option<TransferSnapshot> {
    host.host
        .transfers()
        .into_iter()
        .find(|t| t.id == id && t.direction == dir)
}

fn big_file(root: &Path, name: &str, seed: u64) -> PathBuf {
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join(name);
    common::write_random_file(&file, SIZE, seed);
    file
}

/// Waits until a send handle has handed `bytes` to the connection.
async fn wait_sent(handle: &playdrop_core::host::TransferHandle, bytes: u64) {
    let started = Instant::now();
    while handle.bytes_sent() < bytes {
        assert!(started.elapsed() < T, "send stalled");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// Stops a host as a crash or power-off would leave it, keeping its folders and port.
async fn stop(host: TestHost) -> u16 {
    let port = host.port;
    host.host.shutdown().await;
    drop(host);
    port
}

async fn wait_complete(host: &mut TestHost, id: &str, dir: TransferDirection) {
    host.events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferComplete { id: i, direction, .. }
                if i == id && *direction == dir)
        })
        .await;
}

/// Waits for the send to give up and returns its reason.
async fn wait_failed(host: &mut TestHost, id: &str) -> String {
    let ev = host
        .events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferFailed { id: i, direction: TransferDirection::Send, .. }
                if i == id)
        })
        .await;
    match ev {
        HostEvent::TransferFailed { reason, .. } => reason,
        _ => unreachable!(),
    }
}

async fn wait_online(host: &mut TestHost) {
    host.events
        .wait_for(T, |e| matches!(e, HostEvent::PeerOnline { .. }))
        .await;
}

/// Samples the size of a staged file every millisecond until stopped and keeps the
/// smallest size seen while it existed.
struct PartWatch {
    stop: Arc<std::sync::atomic::AtomicBool>,
    task: tokio::task::JoinHandle<u64>,
}

impl PartWatch {
    async fn stop(self) -> u64 {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.task.await.unwrap()
    }
}

fn watch_part(part: PathBuf) -> PartWatch {
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s2 = Arc::clone(&stop);
    let task = tokio::spawn(async move {
        let mut smallest = u64::MAX;
        while !s2.load(std::sync::atomic::Ordering::SeqCst) {
            if let Ok(m) = std::fs::metadata(&part) {
                smallest = smallest.min(m.len());
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        smallest
    });
    PartWatch { stop, task }
}

/// Reads a sender record as JSON, changes it, and writes it back (the host must be
/// stopped).
fn edit_record(path: &Path, change: impl FnOnce(&mut serde_json::Value)) {
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    change(&mut v);
    std::fs::write(path, serde_json::to_vec(&v).unwrap()).unwrap();
}

/// Files in a receive folder, staging excluded.
fn received(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

/// Two paired hosts; the sender `alpha` gets the short reconnect window when `short`.
async fn two_hosts(root: &Path, short: bool) -> (TestHost, TestHost) {
    let mut a = start_host_with(root, "alpha", 0, |c| {
        if short {
            c.reconnect_for = SHORT;
        }
    })
    .await;
    let mut b = start_host(root, "bravo", 0).await;
    pair(&mut a, &mut b).await;
    (a, b)
}

/// Starts a 64 MiB send, stops the receiver mid-stream and waits for the send to give
/// up. Returns the receiver's port and the transfer ID.
async fn send_then_lose_receiver(a: &mut TestHost, b: TestHost, file: &Path) -> (u16, String) {
    let handle = a
        .host
        .send(&b.host.device_id(), vec![file.to_path_buf()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    assert!(
        record_path(a, &id).exists(),
        "the record is on disk at offer time"
    );
    wait_sent(&handle, CUT_AT).await;
    let port = stop(b).await;
    let reason = wait_failed(a, &id).await;
    assert!(
        reason.contains("resumes"),
        "the failure says it resumes: {reason}"
    );
    (port, id)
}

/// The sending host is stopped mid-stream and started again: the send resumes by itself
/// under the same ID, from the receiver's verified offset (hashes the killed `PUT` never
/// recorded are rebuilt from the file), and the received file matches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sender_restart_resumes_the_send() {
    init_logs();
    let root = temp_root();
    let (a, mut b) = two_hosts(root.path(), false).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0001);
    let expected = file_root(&file, CHUNK);

    let handle = a
        .host
        .send(&b.host.device_id(), vec![file.clone()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let a_port = stop(a).await;
    b.events.drain();
    assert!(
        !b.events
            .seen
            .iter()
            .any(|e| matches!(e, HostEvent::TransferComplete { .. })),
        "the stop landed mid-stream"
    );
    // What the receiver holds now that the sender is gone, once its once-a-second
    // reading caught up: at least about half of what was handed to the connection.
    let started = Instant::now();
    let held = loop {
        let held = snapshot(&b, &id, TransferDirection::Receive)
            .expect("the receiver lists the transfer")
            .bytes;
        if held >= CUT_AT / 2 {
            break held;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the receiver holds only {held} bytes"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(held > 0);
    b.events.clear();
    // Watch the receiver's staged file while the send resumes: a restart at 0 would
    // discard it and start it again from nothing.
    let smallest = watch_part(
        b.recv_dir
            .join(".playdrop-partial")
            .join(&id)
            .join("0.part"),
    );
    let mut a = start_host(root.path(), "alpha", a_port).await;

    wait_complete(&mut a, &id, TransferDirection::Send).await;
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("movie.bin"), CHUNK), expected);

    // The restarted sender reported the same ID active, then complete, listed once.
    let sends: Vec<(u64, bool)> = a
        .events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::TransferProgress {
                id: i,
                direction: TransferDirection::Send,
                bytes,
                ..
            } if *i == id => Some((*bytes, false)),
            HostEvent::TransferComplete { id: i, .. } if *i == id => Some((0, true)),
            _ => None,
        })
        .collect();
    assert!(
        matches!(sends.first(), Some((_, false))),
        "progress (active) before completion: {sends:?}"
    );
    // The count starts each round at what the receiver keeps (`bytes_sent`), so a
    // restart at 0 would show readings below `held`.
    assert!(
        sends.iter().all(|(b, done)| *done || *b >= held),
        "the resumed send never counted from below the receiver's {held} bytes: {sends:?}"
    );
    let smallest = smallest.stop().await;
    assert!(
        smallest >= held,
        "the staged file never shrank below {held} bytes (smallest seen {smallest})"
    );
    let listed: Vec<_> = a
        .host
        .transfers()
        .into_iter()
        .filter(|t| t.id == id)
        .collect();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].state, TransferStatus::Complete);

    // Nothing the receiver had verified was discarded: its count never fell below what
    // it held when the sender stopped.
    for e in &b.events.seen {
        if let HostEvent::TransferProgress { id: i, bytes, .. } = e {
            if *i == id {
                assert!(*bytes >= held, "receiver went back to {bytes} from {held}");
            }
        }
    }
    assert!(
        !record_path(&a, &id).exists(),
        "a complete send leaves no record"
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A send whose receiver stays away past the reconnect window fails with a note that it
/// resumes, keeps its record, and completes under the same ID once the receiver is back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn failed_send_resumes_when_the_receiver_is_back() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0002);
    let expected = file_root(&file, CHUNK);
    let (port, id) = send_then_lose_receiver(&mut a, b, &file).await;

    let snap = snapshot(&a, &id, TransferDirection::Send).unwrap();
    assert_eq!(snap.state, TransferStatus::Failed);
    assert!(snap.note.as_deref().unwrap_or_default().contains("resumes"));
    assert!(
        record_path(&a, &id).exists(),
        "the failed send keeps its record"
    );

    a.events.clear();
    let mut b = start_host(root.path(), "bravo", port).await;
    wait_online(&mut a).await;
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("movie.bin"), CHUNK), expected);
    assert!(
        !record_path(&a, &id).exists(),
        "the record is gone once complete"
    );
    let listed: Vec<_> = a
        .host
        .transfers()
        .into_iter()
        .filter(|t| t.id == id)
        .collect();
    assert_eq!(listed.len(), 1, "the resumed ID is listed once");
    assert_eq!(listed[0].state, TransferStatus::Complete);
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Cancelling a send that waits to resume deletes its record: the receiver coming back
/// does not start it again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiting_send_does_not_resume() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0003);
    let (port, id) = send_then_lose_receiver(&mut a, b, &file).await;

    a.host
        .cancel_transfer(&id)
        .await
        .expect("a waiting send can be cancelled");
    assert!(!record_path(&a, &id).exists(), "cancel deletes the record");
    let snap = snapshot(&a, &id, TransferDirection::Send).unwrap();
    assert_eq!(snap.state, TransferStatus::Cancelled);

    a.events.clear();
    let mut b = start_host(root.path(), "bravo", port).await;
    wait_online(&mut a).await;
    tokio::time::sleep(Duration::from_secs(2)).await;
    a.events.drain();
    assert!(
        !a.events.seen.iter().any(|e| matches!(e,
            HostEvent::TransferProgress { id: i, .. } | HostEvent::TransferComplete { id: i, .. }
                if *i == id)),
        "the cancelled send did not resume"
    );
    assert_eq!(
        snapshot(&a, &id, TransferDirection::Send).unwrap().state,
        TransferStatus::Cancelled
    );
    b.events.drain();
    assert!(!b.events.seen.iter().any(|e| matches!(e,
        HostEvent::TransferComplete { id: i, .. } if *i == id)));
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Sending the same files to the same device again after a failure takes over the
/// earlier send: same ID, and the receiver ends with one transfer and one file.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_files_again_adopt_the_failed_send() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0004);
    let expected = file_root(&file, CHUNK);
    let peer = b.host.device_id();
    let (port, id) = send_then_lose_receiver(&mut a, b, &file).await;

    // Sent again while the receiver is still away, so the peer-online resume cannot be
    // what picks it up.
    let again = a.host.send(&peer, vec![file.clone()]).await.unwrap();
    assert_eq!(again.id(), id, "the new send adopts the recorded ID");
    let mut b = start_host(root.path(), "bravo", port).await;
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("movie.bin"), CHUNK), expected);
    let received: Vec<_> = std::fs::read_dir(&b.recv_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().into_string().unwrap())
        .filter(|n| !n.starts_with('.'))
        .collect();
    assert_eq!(
        received,
        vec!["movie.bin".to_owned()],
        "one file, no duplicate"
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Opens a temp file, as a share target's `content://` opener would.
#[derive(Debug)]
struct FileOpener(PathBuf);

impl SourceOpener for FileOpener {
    fn open(&self) -> std::io::Result<std::fs::File> {
        std::fs::File::open(&self.0)
    }
}

fn stream_of(path: &Path) -> SendSource {
    let meta = std::fs::metadata(path).unwrap();
    let mtime_ms = meta
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis();
    SendSource::Stream(StreamSource {
        name: "clip.bin".into(),
        size: meta.len(),
        mtime_ms: i64::try_from(mtime_ms).unwrap(),
        mime: None,
        // A fresh opener each time, as a new share intent gives.
        opener: Arc::new(FileOpener(path.to_path_buf())),
    })
}

/// Stream sources are never restarted by the host (their openers died with the send),
/// but sharing the same items again adopts the record and completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shared_again_streams_adopt_the_failed_send() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "shared.bin", 0x9_0005);
    let expected = file_root(&file, CHUNK);
    let peer = b.host.device_id();
    let handle = a
        .host
        .send_sources(&peer, vec![stream_of(&file)], None)
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let port = stop(b).await;
    let reason = wait_failed(&mut a, &id).await;
    assert!(
        reason.contains("resumes when these files are sent"),
        "{reason}"
    );
    assert!(record_path(&a, &id).exists());

    a.events.clear();
    let mut b = start_host(root.path(), "bravo", port).await;
    wait_online(&mut a).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        snapshot(&a, &id, TransferDirection::Send).unwrap().state == TransferStatus::Failed,
        "a stream send is not restarted by the host"
    );

    let mut again = a
        .host
        .send_sources(&peer, vec![stream_of(&file)], None)
        .await
        .unwrap();
    assert_eq!(
        again.id(),
        id,
        "sharing the same item again adopts the record"
    );
    tokio::time::timeout(T, again.wait())
        .await
        .expect("finished in time")
        .expect("resumed send succeeded");
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("clip.bin"), CHUNK), expected);
    assert!(!record_path(&a, &id).exists());
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A receiver that lost the transfer (its folders wiped while it was down) answers `404`;
/// the sender offers the same manifest again under the same ID and completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn receiver_that_forgot_gets_the_offer_again() {
    init_logs();
    let root = temp_root();
    let (a, b) = two_hosts(root.path(), false).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0006);
    let expected = file_root(&file, CHUNK);
    let mut handle = a
        .host
        .send(&b.host.device_id(), vec![file.clone()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let (data_dir, recv_dir) = (b.data_dir.clone(), b.recv_dir.clone());
    let port = stop(b).await;
    std::fs::remove_file(data_dir.join("incoming").join(format!("{id}.json"))).unwrap();
    std::fs::remove_dir_all(recv_dir.join(".playdrop-partial").join(&id)).unwrap();

    let mut b = start_host(root.path(), "bravo", port).await;
    let summary = tokio::time::timeout(T, handle.wait())
        .await
        .expect("finished in time")
        .expect("the re-offered send succeeded");
    assert_eq!(summary.id, id);
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(file_root(&b.recv_dir.join("movie.bin"), CHUNK), expected);
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Sends a 64 MiB file and keeps a copy of the sender's record from the middle of the
/// send (as a sender killed before its final `GET` would leave it); waits for the send to
/// complete. Returns the transfer ID and the copy.
async fn send_keeping_a_mid_record(
    root: &Path,
    a: &mut TestHost,
    b: &mut TestHost,
    file: &Path,
) -> (String, Vec<(PathBuf, Vec<u8>)>) {
    let mut handle = a
        .host
        .send(&b.host.device_id(), vec![file.to_path_buf()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    // Past the first round, so the record holds the receiver's chunk size.
    let copy = copy_record(&a.data_dir, &id);
    tokio::time::timeout(T, handle.wait())
        .await
        .expect("finished in time")
        .expect("send succeeded");
    wait_complete(b, &id, TransferDirection::Receive).await;
    let _ = root;
    (id, copy)
}

/// The receiver finished a transfer and both hosts restarted before the sender saw the
/// end: the receiver still answers for the transfer (protocol.md 7.6), so the resumed
/// send ends `complete` and nothing is received twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finished_transfer_is_not_received_twice() {
    init_logs();
    let root = temp_root();
    let (mut a, mut b) = two_hosts(root.path(), false).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0007);
    let (id, copy) = send_keeping_a_mid_record(root.path(), &mut a, &mut b, &file).await;
    let (a_port, b_port) = (stop(a).await, stop(b).await);
    restore_record(&copy);

    let mut b = start_host(root.path(), "bravo", b_port).await;
    let mut a = start_host(root.path(), "alpha", a_port).await;
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    assert_eq!(received(&b.recv_dir), vec!["movie.bin".to_owned()]);
    assert!(!record_path(&a, &id).exists());
    b.events.drain();
    assert!(!b.events.seen.iter().any(|e| matches!(
        e,
        HostEvent::TransferOffer { .. } | HostEvent::TransferProgress { .. }
    )));
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// The receiver lost every trace of a transfer the sender's record shows verified in
/// full: its `404` completes the send instead of offering everything again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn forgotten_after_full_verification_counts_as_complete() {
    init_logs();
    let root = temp_root();
    let (mut a, mut b) = two_hosts(root.path(), false).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0008);
    let (id, copy) = send_keeping_a_mid_record(root.path(), &mut a, &mut b, &file).await;
    let b_data = b.data_dir.clone();
    let (a_port, b_port) = (stop(a).await, stop(b).await);
    std::fs::remove_file(b_data.join("incoming").join(format!("{id}.json"))).unwrap();
    restore_record(&copy);
    let state = root
        .path()
        .join("alpha")
        .join("data")
        .join("outgoing")
        .join(format!("{id}.state"));
    edit_record(&state, |v| v["verified"] = serde_json::json!([0]));

    let mut b = start_host(root.path(), "bravo", b_port).await;
    let mut a = start_host(root.path(), "alpha", a_port).await;
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    assert_eq!(received(&b.recv_dir), vec!["movie.bin".to_owned()]);
    b.events.drain();
    assert!(!b
        .events
        .seen
        .iter()
        .any(|e| matches!(e, HostEvent::TransferOffer { .. })));
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A receiver that keeps forgetting the transfer gets it offered again at most three
/// times over the record's life; the count survives a restart of the sender.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reoffers_are_capped() {
    init_logs();
    let root = temp_root();
    let (a, b) = two_hosts(root.path(), false).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0009);
    let handle = a
        .host
        .send(&b.host.device_id(), vec![file.clone()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let rec = record_path(&a, &id);
    let state = state_path(&a, &id);
    let (b_data, b_recv) = (b.data_dir.clone(), b.recv_dir.clone());
    let a_port = stop(a).await;
    let b_port = stop(b).await;
    edit_record(&state, |v| v["reoffers"] = serde_json::json!(3));
    std::fs::remove_file(b_data.join("incoming").join(format!("{id}.json"))).unwrap();
    std::fs::remove_dir_all(b_recv.join(".playdrop-partial").join(&id)).unwrap();

    let b = start_host(root.path(), "bravo", b_port).await;
    let mut a = start_host(root.path(), "alpha", a_port).await;
    let reason = wait_failed(&mut a, &id).await;
    assert!(reason.contains("lost this transfer"), "{reason}");
    assert!(
        !rec.exists(),
        "a send that gave up for good leaves no record"
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Unpairing a device deletes the records of sends waiting for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unpair_deletes_waiting_records() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let peer = b.host.device_id();
    let file = big_file(root.path(), "movie.bin", 0x9_000a);
    let (_, id) = send_then_lose_receiver(&mut a, b, &file).await;
    assert!(record_path(&a, &id).exists());
    a.host.unpair(&peer).await.unwrap();
    assert!(!record_path(&a, &id).exists(), "unpair deletes the record");
    assert!(
        a.host.cancel_transfer(&id).await.is_err(),
        "nothing left to cancel"
    );
    a.host.shutdown().await;
}

/// A cancel issued while the peer's return starts the waiting send again ends the send
/// `cancelled`, whichever of the two ran first.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_during_resume_ends_cancelled() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_000b);
    let (port, id) = send_then_lose_receiver(&mut a, b, &file).await;
    assert!(
        snapshot(&a, &id, TransferDirection::Send)
            .unwrap()
            .resumable
    );
    a.events.clear();
    let mut b = start_host(root.path(), "bravo", port).await;
    wait_online(&mut a).await;
    a.host.cancel_transfer(&id).await.expect("cancel accepted");
    let started = Instant::now();
    loop {
        let snap = snapshot(&a, &id, TransferDirection::Send).unwrap();
        if snap.state == TransferStatus::Cancelled {
            assert!(!snap.resumable);
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "still {:?}",
            snap.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!record_path(&a, &id).exists());
    a.events.drain();
    b.events.drain();
    assert!(!a.events.seen.iter().any(|e| matches!(e,
        HostEvent::TransferComplete { id: i, .. } if *i == id)));
    assert!(!b.events.seen.iter().any(|e| matches!(e,
        HostEvent::TransferComplete { id: i, .. } if *i == id)));
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A re-offer forgets which streams the receiver verified before it lost the transfer:
/// only what it verifies again may complete the send on a later `404`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reoffer_forgets_verified_streams() {
    init_logs();
    let root = temp_root();
    let (a, b) = two_hosts(root.path(), false).await;
    let dir = root.path().join("src").join("Pair");
    std::fs::create_dir_all(&dir).unwrap();
    common::write_random_file(&dir.join("a.bin"), SIZE, 0x9_0101);
    common::write_random_file(&dir.join("b.bin"), SIZE, 0x9_0102);
    let handle = a
        .host
        .send(&b.host.device_id(), vec![dir.clone()])
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let rec = state_path(&a, &id);
    let (b_data, b_recv) = (b.data_dir.clone(), b.recv_dir.clone());
    let a_port = stop(a).await;
    let b_port = stop(b).await;
    // As if the receiver had verified the first file before it lost everything.
    edit_record(&rec, |v| v["verified"] = serde_json::json!([0]));
    std::fs::remove_file(b_data.join("incoming").join(format!("{id}.json"))).unwrap();
    std::fs::remove_dir_all(b_recv.join(".playdrop-partial").join(&id)).unwrap();

    let mut b = start_host(root.path(), "bravo", b_port).await;
    let mut a = start_host(root.path(), "alpha", a_port).await;
    // The first write after the re-offer carries the count and no verified stream.
    let started = Instant::now();
    loop {
        if let Ok(bytes) = std::fs::read(&rec) {
            let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if v["reoffers"].as_u64() == Some(1) {
                assert!(
                    v.get("verified").is_none(),
                    "verified streams survived the re-offer: {}",
                    v["verified"]
                );
                break;
            }
        }
        assert!(started.elapsed() < T, "no re-offer");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    wait_complete(&mut b, &id, TransferDirection::Receive).await;
    assert_eq!(received(&b.recv_dir), vec!["Pair".to_owned()]);
    assert_eq!(
        file_root(&b.recv_dir.join("Pair").join("a.bin"), CHUNK),
        file_root(&dir.join("a.bin"), CHUNK)
    );
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Fails a stream send while its receiver is down, brings the receiver back, has it
/// cancel the transfer, and shares the same item again. Returns the hosts, the first
/// transfer ID and the new handle.
async fn reshare_after_receiver_cancelled(
    root: &Path,
    seed: u64,
    size: u64,
) -> (
    TestHost,
    TestHost,
    String,
    PathBuf,
    playdrop_core::host::TransferHandle,
) {
    let (mut a, b) = two_hosts(root, true).await;
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    let file = src.join("shared.bin");
    common::write_random_file(&file, size, seed);
    let peer = b.host.device_id();
    let handle = a
        .host
        .send_sources(&peer, vec![stream_of(&file)], None)
        .await
        .unwrap();
    let id = handle.id().to_owned();
    wait_sent(&handle, CUT_AT).await;
    let port = stop(b).await;
    wait_failed(&mut a, &id).await;
    a.events.clear();
    let b = start_host(root, "bravo", port).await;
    wait_online(&mut a).await;
    b.host
        .cancel_transfer(&id)
        .await
        .expect("the receiver cancels");
    let again = a
        .host
        .send_sources(&peer, vec![stream_of(&file)], None)
        .await
        .unwrap();
    assert_eq!(again.id(), id, "the record is taken over");
    (a, b, id, file, again)
}

/// The receiver cancelled the transfer while the sender was away; sharing the same item
/// again sends it as a new transfer on the wire, while this device keeps following it
/// under the ID it already knows: events, `transfers()` and the summary all carry it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_files_after_receiver_cancelled_go_as_new_transfer() {
    init_logs();
    let root = temp_root();
    let (mut a, mut b, id, file, mut again) =
        reshare_after_receiver_cancelled(root.path(), 0x9_0103, SIZE).await;
    let summary = tokio::time::timeout(T, again.wait())
        .await
        .expect("finished in time")
        .expect("sent as a new transfer");
    assert_eq!(summary.id, id, "the local ID stays");
    wait_complete(&mut a, &id, TransferDirection::Send).await;
    assert!(
        !a.events.seen.iter().any(|e| matches!(e,
            HostEvent::TransferFailed { id: i, .. } if *i == id)),
        "no end is reported for the first attempt"
    );
    let wire = b
        .events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferComplete { id: i, direction: TransferDirection::Receive, .. }
                if *i != id)
        })
        .await;
    let HostEvent::TransferComplete { id: wire, .. } = wire else {
        unreachable!()
    };
    assert_ne!(wire, id, "a new transfer on the wire");
    assert_eq!(received(&b.recv_dir), vec!["clip.bin".to_owned()]);
    assert_eq!(
        file_root(&b.recv_dir.join("clip.bin"), CHUNK),
        file_root(&file, CHUNK)
    );
    assert!(!record_path(&a, &id).exists());
    let snap = snapshot(&a, &id, TransferDirection::Send).unwrap();
    assert_eq!(snap.state, TransferStatus::Complete);
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// `cancel_transfer` with the ID this device knows cancels a send that continued under a
/// new ID on the wire, and the receiver drops the new transfer.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_by_local_id_reaches_the_continued_send() {
    init_logs();
    let root = temp_root();
    // Large enough that the new transfer is still running when the cancel comes.
    let (a, b, id, _, mut again) =
        reshare_after_receiver_cancelled(root.path(), 0x9_0106, 4 * SIZE).await;
    // The new transfer exists on the receiver: the send continued under a new ID.
    let started = Instant::now();
    while !b
        .host
        .transfers()
        .iter()
        .any(|t| t.direction == TransferDirection::Receive && t.id != id)
    {
        assert!(started.elapsed() < T, "no new transfer on the receiver");
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    a.host
        .cancel_transfer(&id)
        .await
        .expect("the local ID cancels it");
    let end = tokio::time::timeout(T, again.wait())
        .await
        .expect("ended in time");
    assert!(end.is_err(), "the send was cancelled");
    assert_eq!(
        snapshot(&a, &id, TransferDirection::Send).unwrap().state,
        TransferStatus::Cancelled
    );
    let started = Instant::now();
    loop {
        let cancelled = b.host.transfers().into_iter().any(|t| {
            t.direction == TransferDirection::Receive
                && t.id != id
                && t.state == TransferStatus::Cancelled
        });
        if cancelled {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the receiver kept the new transfer"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// A send that waits to resume is listed after a restart of the sending host, and can
/// be cancelled from that listing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_send_is_listed_after_restart() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0104);
    let (_, id) = send_then_lose_receiver(&mut a, b, &file).await;
    let a = a.restart(root.path()).await;
    let snap = snapshot(&a, &id, TransferDirection::Send).expect("listed after the restart");
    assert_eq!(snap.state, TransferStatus::Failed);
    assert!(snap.resumable);
    assert!(
        snap.note.as_deref().unwrap_or_default().contains("resumes"),
        "{:?}",
        snap.note
    );
    a.host.cancel_transfer(&id).await.expect("cancellable");
    let snap = snapshot(&a, &id, TransferDirection::Send).unwrap();
    assert_eq!(snap.state, TransferStatus::Cancelled);
    assert!(!snap.resumable);
    assert!(!record_path(&a, &id).exists());
    a.host.shutdown().await;
}

/// A waiting send cancelled while its receiver is away reaches the receiver at its next
/// session, so the receiver frees the transfer instead of keeping it for 24 h.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_reaches_the_receiver_at_its_next_session() {
    init_logs();
    let root = temp_root();
    let (mut a, b) = two_hosts(root.path(), true).await;
    let file = big_file(root.path(), "movie.bin", 0x9_0105);
    let (port, id) = send_then_lose_receiver(&mut a, b, &file).await;
    a.host.cancel_transfer(&id).await.expect("cancellable");
    let tomb = a.data_dir.join("outgoing").join(format!("{id}.tomb"));
    assert!(tomb.exists(), "the cancel waits for the receiver");

    a.events.clear();
    let b = start_host(root.path(), "bravo", port).await;
    wait_online(&mut a).await;
    let started = Instant::now();
    loop {
        let state = snapshot(&b, &id, TransferDirection::Receive).map(|s| s.state);
        if state == Some(TransferStatus::Cancelled) && !tomb.exists() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "receiver still {state:?}, tomb {}",
            tomb.exists()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    a.host.shutdown().await;
    b.host.shutdown().await;
}
