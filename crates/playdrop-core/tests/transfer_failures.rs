//! Why a transfer failed, on both sides (`docs/core-api.md` section 16): a send refused for
//! lack of space ends with `TransferFailure::NoSpace` and a plain sentence on the sender,
//! in the event and the snapshot, and the receiver reports the refusal once a minute per
//! peer with the bytes offered, free and kept in reserve; a declined offer ends with
//! `Declined`; an offer accepted by hand that then finds no space ends with `NoSpace` on
//! the sender too (the `no_space` reason of the decision frame, protocol.md 5.13).

mod common;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use common::{pair, start_host, start_host_with, temp_root, write_random_file, TestHost};
use playdrop_core::host::{HostEvent, TransferDirection, TransferFailure, TransferStatus};
use playdrop_core::transfer::{FreeSpace, SpaceInfo};

const T: Duration = Duration::from_secs(20);
const GIB: u64 = 1024 * 1024 * 1024;

/// A volume of 100 GiB with 3 GiB free: the reserve is 5 GiB, so nothing fits.
#[derive(Debug)]
struct NearlyFull;

impl FreeSpace for NearlyFull {
    fn space(&self, _path: &Path) -> std::io::Result<SpaceInfo> {
        Ok(SpaceInfo {
            available: 3 * GIB,
            total: 100 * GIB,
        })
    }
}

/// Sends one small file from `a` to `b` and returns the sender's failure event.
async fn send_and_fail(
    a: &mut TestHost,
    b: &TestHost,
    root: &Path,
    n: u64,
) -> (String, TransferFailure) {
    let src = root.join(format!("f{n}.bin"));
    write_random_file(&src, 4096, n);
    let handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let id = handle.id().to_owned();
    let ev = a
        .events
        .wait_for(T, |e| {
            matches!(e, HostEvent::TransferFailed { id: i, direction: TransferDirection::Send, .. } if *i == id)
        })
        .await;
    let HostEvent::TransferFailed { reason, code, .. } = ev else {
        unreachable!()
    };
    let snap = a
        .host
        .transfers()
        .into_iter()
        .find(|t| t.id == id)
        .expect("listed");
    assert_eq!(
        snap.failure,
        Some(code),
        "the snapshot carries the same code"
    );
    assert_eq!(snap.note.as_deref(), Some(reason.as_str()));
    (reason, code)
}

fn refusals(h: &TestHost) -> Vec<(u64, u64, u64)> {
    h.events
        .seen
        .iter()
        .filter_map(|e| match e {
            HostEvent::ReceiveRefusedNoSpace {
                offered_bytes,
                free_bytes,
                reserve_bytes,
                ..
            } => Some((*offered_bytes, *free_bytes, *reserve_bytes)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_space_is_plain_on_the_sender_and_reported_once_a_minute_on_the_receiver() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "a", 0).await;
    let mut b = start_host_with(root.path(), "b", 0, |c| c.free_space = Arc::new(NearlyFull)).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();

    let (reason, code) = send_and_fail(&mut a, &b, root.path(), 1).await;
    assert_eq!(code, TransferFailure::NoSpace);
    assert_eq!(
        reason,
        "the receiving device does not have enough free space"
    );
    assert!(
        !reason.contains("507") && !reason.contains("no_space"),
        "{reason}"
    );
    let snap = a.host.transfers().into_iter().next().unwrap();
    assert_eq!(snap.state, TransferStatus::Failed);

    b.events
        .wait_for(T, |e| matches!(e, HostEvent::ReceiveRefusedNoSpace { peer, .. } if *peer == a_id.to_hex()))
        .await;
    assert_eq!(refusals(&b), vec![(4096, 3 * GIB, 5 * GIB)]);

    // The sender tries again within the minute: refused again, reported no second time.
    let (_, code) = send_and_fail(&mut a, &b, root.path(), 2).await;
    assert_eq!(code, TransferFailure::NoSpace);
    a.host.ping(&b.host.device_id(), Some("after")).unwrap();
    b.events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::Ping { message: Some(m), .. } if m == "after"),
        )
        .await;
    assert_eq!(refusals(&b).len(), 1);

    a.host.shutdown().await;
    b.host.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_declined_offer_says_declined() {
    common::init_logs();
    let root = temp_root();
    let mut a = start_host(root.path(), "a", 0).await;
    let mut b = start_host(root.path(), "b", 0).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();

    let src = root.path().join("doc.bin");
    write_random_file(&src, 4096, 7);
    let handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let id = handle.id().to_owned();
    let offer = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::TransferOffer { .. }))
        .await;
    let HostEvent::TransferOffer { id: offer_id, .. } = offer else {
        unreachable!()
    };
    b.host.respond_offer(&offer_id, false).await.unwrap();
    let ev = a
        .events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::TransferFailed { id: i, .. } if *i == id),
        )
        .await;
    let HostEvent::TransferFailed { reason, code, .. } = ev else {
        unreachable!()
    };
    assert_eq!(code, TransferFailure::Declined);
    assert_eq!(reason, "the receiving device declined the files");
    // The receiving side's own book says declined too.
    let snap = b
        .host
        .transfers()
        .into_iter()
        .find(|t| t.id == offer_id)
        .unwrap();
    assert_eq!(snap.failure, Some(TransferFailure::Declined));

    a.host.shutdown().await;
    b.host.shutdown().await;
}

/// Room while the offer waits, none when the user accepts it.
#[derive(Debug, Default)]
struct FillsUp(AtomicBool);

impl FreeSpace for FillsUp {
    fn space(&self, path: &Path) -> std::io::Result<SpaceInfo> {
        if self.0.load(Ordering::Acquire) {
            NearlyFull.space(path)
        } else {
            Ok(SpaceInfo {
                available: 90 * GIB,
                total: 100 * GIB,
            })
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_offer_accepted_by_hand_without_space_says_no_space() {
    common::init_logs();
    let root = temp_root();
    let space = Arc::new(FillsUp::default());
    let s2 = Arc::clone(&space);
    let mut a = start_host(root.path(), "a", 0).await;
    let mut b = start_host_with(root.path(), "b", 0, move |c| c.free_space = s2).await;
    pair(&mut a, &mut b).await;
    let a_id = a.host.device_id();
    let mut g = b.host.grants(&a_id).unwrap();
    g.files = false;
    b.host.set_grants(&a_id, g).await.unwrap();

    let src = root.path().join("late.bin");
    write_random_file(&src, 4096, 9);
    let handle = a.host.send(&b.host.device_id(), vec![src]).await.unwrap();
    let id = handle.id().to_owned();
    let offer = b
        .events
        .wait_for(T, |e| matches!(e, HostEvent::TransferOffer { .. }))
        .await;
    let HostEvent::TransferOffer { id: offer_id, .. } = offer else {
        unreachable!()
    };
    space.0.store(true, Ordering::Release);
    assert!(b.host.respond_offer(&offer_id, true).await.is_err());
    let ev = a
        .events
        .wait_for(
            T,
            |e| matches!(e, HostEvent::TransferFailed { id: i, .. } if *i == id),
        )
        .await;
    let HostEvent::TransferFailed { reason, code, .. } = ev else {
        unreachable!()
    };
    assert_eq!(code, TransferFailure::NoSpace);
    assert_eq!(
        reason,
        "the receiving device does not have enough free space"
    );

    a.host.shutdown().await;
    b.host.shutdown().await;
}
