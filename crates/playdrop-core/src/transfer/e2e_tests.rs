//! End-to-end transfer over an in-memory duplex pipe: a 64 MiB file in 1 MiB chunks, the
//! stream killed at a random point, resumed from the receiver's verified offset, ending
//! with a matching root (protocol.md 7.4, 7.6; SR-21, SR-22).

use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::chunk::tests::TestRng;
use super::chunk::{expected_body_len, ChunkHash};
use super::receiver::tests::FakeSpace;
use super::receiver::{
    AcceptOptions, FileState, TransferReceiver, TransferState, TransferStateView,
};
use super::sender::{ResumeAction, TransferSender};
use super::ManifestBuilder;

const SIZE: usize = 64 * 1024 * 1024;
const CS: u32 = 1024 * 1024;
const PIPE: usize = 4 * 1024 * 1024;

/// Streams `fid` from `from` to end of file over a fresh duplex pipe, cutting the body after
/// `kill_after` bytes when given. Returns body bytes moved and the receiver's outcome.
async fn stream_once(
    sender: &mut TransferSender,
    rx: &TransferReceiver,
    token: &str,
    from: u64,
    kill_after: Option<u64>,
) -> (u64, bool) {
    let size = sender.manifest().files[0].size;
    let mut enc = sender.file_encoder(0, from).await.unwrap();
    let mut upload = rx.begin_file(0, token, from, size - 1).await.unwrap();
    let (mut w, r) = tokio::io::duplex(PIPE);
    let send = async {
        let moved = match kill_after {
            Some(limit) => tokio::io::copy(&mut (&mut enc).take(limit), &mut w).await,
            None => tokio::io::copy(&mut enc, &mut w).await,
        }
        .unwrap();
        w.shutdown().await.unwrap();
        drop(w);
        moved
    };
    let recv = async { upload.receive(r).await };
    let (moved, received) = tokio::join!(send, recv);
    let root: Option<ChunkHash> = enc.root(size);
    sender.record_hashes(0, enc.into_hashes());
    let complete = received.is_ok();
    let resp = upload.finish(root).await.unwrap();
    assert_eq!(complete, resp.state == FileState::Verified);
    (moved, complete)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn duplex_kill_and_resume_64mib() {
    let tmp = tempfile::tempdir().unwrap();
    let src_dir = tmp.path().join("send");
    std::fs::create_dir(&src_dir).unwrap();
    let mut rng = TestRng(0x5eed);
    let data = rng.bytes(SIZE);
    let src = src_dir.join("movie.bin");
    std::fs::write(&src, &data).unwrap();

    let mut b = ManifestBuilder::new(CS);
    b.add_path(&src).await.unwrap();
    let (manifest, sources) = b.build().unwrap();
    let mut sender = TransferSender::new(manifest.clone(), sources, CS).unwrap();

    let recv_root = tmp.path().join("recv");
    let rx = TransferReceiver::accept(
        &recv_root,
        manifest.clone(),
        FakeSpace::plenty(),
        AcceptOptions::default(),
    )
    .await
    .unwrap();
    let token = rx.state().files[0].token.clone().unwrap();

    let body_len = expected_body_len(0, SIZE as u64 - 1, CS);
    let kill = 1 + rng.below(body_len - 1);
    let started = Instant::now();
    let (moved1, done1) = stream_once(&mut sender, &rx, &token, 0, Some(kill)).await;
    assert!(!done1);
    assert_eq!(moved1, kill);

    // The sender asks for state over the wire (JSON round trip) and plans the resume.
    let wire = serde_json::to_vec(&rx.state()).unwrap();
    let remote: TransferStateView = serde_json::from_slice(&wire).unwrap();
    let rec = u64::from(CS) + 32;
    let expect_offset = kill / rec * u64::from(CS);
    assert_eq!(remote.files[0].verified_offset, expect_offset);
    let plan = sender.plan_resume(&remote).await.unwrap();
    let from = match plan.files[0].1 {
        ResumeAction::Resume { from } => from,
        other => panic!("expected resume, got {other:?}"),
    };
    assert_eq!(from, expect_offset);

    let (moved2, done2) = stream_once(&mut sender, &rx, &token, from, None).await;
    let elapsed = started.elapsed();
    assert!(done2);
    assert_eq!(moved2, expected_body_len(from, SIZE as u64 - 1, CS));
    assert_eq!(rx.transfer_state(), TransferState::Complete);

    let received = recv_root.join("movie.bin");
    let mut got = Vec::with_capacity(SIZE);
    tokio::fs::File::open(&received)
        .await
        .unwrap()
        .read_to_end(&mut got)
        .await
        .unwrap();
    assert!(got == data, "received bytes differ");
    let st = rx.state();
    assert_eq!(
        st.files[0].root.as_deref(),
        Some(super::hash_to_hex(&sender.root(0).unwrap()).as_str())
    );

    let moved = moved1 + moved2;
    #[allow(clippy::cast_precision_loss)] // display only
    let rate = moved as f64 / elapsed.as_secs_f64();
    eprintln!(
        "e2e: kill at {kill} of {body_len} body bytes, resumed from {from}; moved {moved} bytes in {:.3} s = {:.1} MiB/s",
        elapsed.as_secs_f64(),
        rate / (1024.0 * 1024.0)
    );
}
