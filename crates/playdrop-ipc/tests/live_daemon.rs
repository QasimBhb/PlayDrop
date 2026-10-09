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
#![cfg(target_os = "linux")]

use std::io::Write as _;
use std::time::Duration;

use futures_util::StreamExt;
use playdrop_core::host::{HostEvent, TransferDirection};
use playdrop_ipc::client::Client;
use playdrop_ipc::{Event, Ok};

/// Size of the file sent: big enough to take more than one chunk.
const FILE_BYTES: usize = 3 * 1024 * 1024 + 17;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs a running playdropd paired with the device named by PD_PEER"]
async fn send_files_to_a_live_peer() {
    let peer = std::env::var("PD_PEER").expect("set PD_PEER to the receiving device");
    let client = Client::connect()
        .await
        .expect("daemon reachable and verified");
    let mut events = client.events();

    // Under the system temp dir, never on the project mount.
    let dir = tempfile::Builder::new()
        .prefix("playdrop-live-")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let path = dir.path().join("live-send.bin");
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
