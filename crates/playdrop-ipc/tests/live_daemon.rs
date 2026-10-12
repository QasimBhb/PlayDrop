//! Manual check against a running `playdropd` on the real session bus (Linux only).
//!
//! Ignored by default. Start a daemon, pair it with another host, then run:
//!
//! ```text
//! PD_PEER=<label, Short ID or Device ID> cargo test -p playdrop-ipc --test live_daemon -- --ignored
//! ```
//!
//! It connects with the normal client checks (same UID, daemon binary named `playdropd`),
//! sends a fresh random file through `SendFiles` (as a file descriptor, SR-26), and waits
//! for the daemon to report the send complete.
//!
//! The second test queues a file in the daemon's outbox for the peer instead (protocol.md
//! 7.8), for the "pulls queued files on open" check of a phone: run it while the phone's
//! app is closed, then open the app and watch the file arrive.
//!
//! ```text
//! PD_PEER=<peer> cargo test -p playdrop-ipc --test live_daemon -- --ignored queue_outbox
//! ```
#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::time::Duration;

use futures_util::StreamExt;
use playdrop_core::host::{HostEvent, TransferDirection};
use playdrop_ipc::client::Client;
use playdrop_ipc::{Event, Ok};

/// Size of the file sent: big enough to take more than one chunk.
const FILE_BYTES: usize = 3 * 1024 * 1024 + 17;

/// A fresh random file of `FILE_BYTES` under the system temp dir, never on the project
/// mount. The directory lives as long as the returned guard.
fn random_file(name: &str) -> (tempfile::TempDir, std::path::PathBuf, Vec<u8>) {
    let dir = tempfile::Builder::new()
        .prefix("playdrop-live-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let path = dir.path().join(name);
    let mut data = vec![0u8; FILE_BYTES];
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15
        ^ u64::from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos(),
        );
    for b in &mut data {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x.to_le_bytes()[0];
    }
    std::fs::File::create(&path)
        .unwrap()
        .write_all(&data)
        .unwrap();
    (dir, path, data)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running playdropd paired with the device named by PD_PEER"]
async fn send_files_to_a_live_peer() {
    let peer = std::env::var("PD_PEER").expect("set PD_PEER to the receiving device");
    let client = Client::connect()
        .await
        .expect("daemon reachable and verified");
    let mut events = client.events();

    let (_dir, path, data) = random_file("live-send.bin");

    let started = client
        .send_files(&peer, vec![path.clone()])
        .await
        .expect("daemon accepted the send");
    let Ok::TransferStarted { id } = started else {
        panic!("expected TransferStarted, got {started:?}");
    };
    println!("transfer {id} started: {} bytes", data.len());

    let done = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(item) = events.next().await {
            match item {
                Result::Ok(Event::Host(HostEvent::TransferComplete {
                    id: done_id,
                    direction: TransferDirection::Send,
                    bytes,
                    elapsed_ms,
                    ..
                })) if done_id == id => return Some((bytes, elapsed_ms)),
                Result::Ok(Event::Host(HostEvent::TransferFailed {
                    id: failed_id,
                    reason,
                    ..
                })) if failed_id == id => panic!("transfer failed: {reason}"),
                _ => {}
            }
        }
        None
    })
    .await
    .expect("transfer finished within 60 s")
    .expect("event stream stayed open");
    assert_eq!(done.0, FILE_BYTES as u64);
    println!("transfer {id} complete in {} ms", done.1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running playdropd paired with the device named by PD_PEER"]
async fn queue_outbox_for_a_live_peer() {
    let peer = std::env::var("PD_PEER").expect("set PD_PEER to the receiving device");
    let client = Client::connect()
        .await
        .expect("daemon reachable and verified");

    // The outbox keeps the path, not a copy (protocol.md 7.8), so the file must outlive
    // the test: it goes to <temp>/playdrop-live-outbox and is left there.
    let (dir, tmp_path, data) = random_file("live-outbox.bin");
    let keep = std::env::temp_dir().join("playdrop-live-outbox");
    std::fs::create_dir_all(&keep).unwrap();
    let path = keep.join(format!(
        "live-outbox-{}.bin",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    ));
    std::fs::copy(&tmp_path, &path).unwrap();
    drop(dir);
    let queued = client
        .queue_outbox(&peer, vec![path.clone()])
        .await
        .expect("daemon queued the file");
    let Ok::OutboxQueued { id } = queued else {
        panic!("expected OutboxQueued, got {queued:?}");
    };
    println!(
        "outbox item {id} queued for {peer}: {} bytes from {}; open PlayDrop on that device to pull it",
        data.len(),
        path.display()
    );
}
