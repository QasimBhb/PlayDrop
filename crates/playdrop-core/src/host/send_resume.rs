//! Which sends are running and which recorded sends wait to resume, and who may start or
//! cancel them (protocol.md 7.6 "sender persistence", `docs/core-api.md` section 12).
//!
//! One lock ([`Sends`]) holds both the running sends and an in-memory index of the records
//! on disk, so a resume, an adoption by a new send and a cancel can never both win for the
//! same transfer ID. Files are read and deleted outside the lock; only the index is
//! consulted under it.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::events::TransferDirection;
use super::send::{self, Outgoing};
use super::send_store::{self, Index, SendRecord, Summary, Tomb};
use super::transfers::{TransferSnapshot, TransferStatus};
use super::{connect, lock, Inner};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::session::types::TransferState as FrameState;
use crate::transfer::{Manifest, SourceInfo};

/// Running sends, the index of recorded ones and the cancels not yet delivered, under one
/// lock.
#[derive(Default)]
pub(crate) struct Sends {
    /// Running sends by transfer ID, so `transfer` frames and cancels reach them.
    pub(crate) live: HashMap<String, Outgoing>,
    /// Every record on disk, kept in step by the sends that write them.
    pub(crate) records: Index,
    /// Cancelled waiting sends whose receiver has not had the `DELETE` yet.
    tombs: HashMap<String, Tomb>,
}

/// The channels of a send whose ID is registered in [`Sends::live`].
pub(crate) struct Claim {
    pub(super) peer: DeviceId,
    pub(super) id: String,
    pub(super) decision_rx: watch::Receiver<Option<FrameState>>,
    pub(super) cancel: Arc<watch::Sender<bool>>,
}

impl Claim {
    /// Registers `id` as running and returns its channels; `wire_id` is the transfer ID
    /// the receiver knows.
    fn register(sends: &mut Sends, peer: DeviceId, id: &str, wire_id: &str) -> Self {
        let (decision_tx, decision_rx) = watch::channel(None);
        let cancel = Arc::new(watch::Sender::new(false));
        sends.live.insert(
            id.to_owned(),
            Outgoing {
                peer,
                wire_id: wire_id.to_owned(),
                decision: decision_tx,
                cancel: Arc::clone(&cancel),
            },
        );
        Self {
            peer,
            id: id.to_owned(),
            decision_rx,
            cancel,
        }
    }

    /// Gives the ID back without starting a send. A cancel that reached the claim in the
    /// meantime was answered as for a running send, so it is carried out on the record
    /// now instead of being lost.
    async fn release(self, inner: &Arc<Inner>) {
        lock(&inner.sends).live.remove(&self.id);
        if *self.cancel.borrow() {
            cancel(inner, &self.id).await;
        }
    }
}

/// Registers a brand-new send (fresh transfer ID).
pub(super) fn claim_new(inner: &Inner, peer: DeviceId, id: &str) -> Claim {
    Claim::register(&mut lock(&inner.sends), peer, id, id)
}

/// Points a running send at a new transfer ID on the wire, keeping its local ID and its
/// cancel channel (a send that continues as a new offer). `None` when it is not running.
pub(super) fn reclaim(inner: &Inner, id: &str, wire_id: &str) -> Option<Claim> {
    let mut sends = lock(&inner.sends);
    let o = sends.live.get_mut(id)?;
    let (decision_tx, decision_rx) = watch::channel(None);
    o.decision = decision_tx;
    wire_id.clone_into(&mut o.wire_id);
    Some(Claim {
        peer: o.peer,
        id: id.to_owned(),
        decision_rx,
        cancel: Arc::clone(&o.cancel),
    })
}

pub(super) fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

async fn read_record(inner: &Inner, id: &str) -> Option<SendRecord> {
    let (dir, id) = (inner.data_dir.clone(), id.to_owned());
    tokio::task::spawn_blocking(move || send_store::load(&dir, &id))
        .await
        .ok()
        .flatten()
}

async fn delete_records(inner: &Inner, ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    let dir = inner.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for id in ids {
            send_store::remove(&dir, &id);
        }
    })
    .await;
}

/// Takes a recorded send for starting: under the lock, only when no send runs it, it is
/// indexed and its summary passes `pred`; the record itself is read afterwards, outside
/// the lock. A cancel that arrives in between reaches the claim's channel, so the send
/// starts cancelled and deletes its record (or [`Claim::release`] carries it out).
async fn claim_record(
    inner: &Arc<Inner>,
    id: &str,
    pred: impl FnOnce(&Summary) -> bool,
) -> Option<(SendRecord, Claim)> {
    let now = unix_ms();
    let claim = {
        let mut sends = lock(&inner.sends);
        if sends.live.contains_key(id) {
            return None;
        }
        let summary = sends.records.get(id)?;
        if summary.is_expired(now) || !pred(summary) {
            return None;
        }
        let (peer, wire) = (summary.peer, summary.wire_id.clone());
        Claim::register(&mut sends, peer, id, &wire)
    };
    if let Some(rec) = read_record(inner, id).await {
        return Some((rec, claim));
    }
    tracing::debug!("indexed send record unreadable; forgotten");
    lock(&inner.sends).records.remove(id);
    lock(&inner.transfers).forget_finished(id, TransferDirection::Send);
    claim.release(inner).await;
    None
}

/// Finds the record a new send of `manifest` and `sources` to `peer` continues, and
/// claims it (protocol.md 7.6; the match rule is [`SendRecord::matches`]). Expired
/// records met on the way are deleted.
pub(super) async fn adopt(
    inner: &Arc<Inner>,
    peer: &DeviceId,
    manifest: &Manifest,
    sources: &[SourceInfo],
) -> Option<(SendRecord, Claim)> {
    let now = unix_ms();
    let (candidates, expired) = {
        let mut sends = lock(&inner.sends);
        let live: Vec<String> = sends.live.keys().cloned().collect();
        let expired = sends
            .records
            .ids_where(|s| s.is_expired(now))
            .into_iter()
            .filter(|id| !live.contains(id))
            .collect::<Vec<_>>();
        for id in &expired {
            sends.records.remove(id);
        }
        let candidates = sends.records.ids_where(|s| s.may_match(peer, manifest));
        (candidates, expired)
    };
    {
        let mut book = lock(&inner.transfers);
        for id in &expired {
            book.forget_finished(id, TransferDirection::Send);
        }
    }
    delete_records(inner, expired).await;
    for id in candidates {
        let Some((rec, claim)) = claim_record(inner, &id, |s| s.may_match(peer, manifest)).await
        else {
            continue;
        };
        if rec.matches(peer, manifest, sources) {
            return Some((rec, claim));
        }
        claim.release(inner).await;
    }
    None
}

/// Cancels a send by ID, running or waiting to resume, in one step under the lock, so a
/// resume that fires at the same moment either never claims the ID or claims it and is
/// signalled. A waiting send's record is deleted, its snapshot shows `cancelled`, and the
/// receiver gets the `DELETE` so it frees its slot and `.part` files (protocol.md 7.5,
/// SR-20): now when the peer is online, else at its next session, for 24 h at most. False
/// when there is no such send.
pub(crate) async fn cancel(inner: &Arc<Inner>, id: &str) -> bool {
    let summary = {
        let mut sends = lock(&inner.sends);
        if let Some(o) = sends.live.get(id) {
            o.cancel.send_replace(true);
            return true;
        }
        let Some(summary) = sends.records.remove(id) else {
            return false;
        };
        summary
    };
    lock(&inner.transfers).amend_finished(
        id,
        TransferDirection::Send,
        TransferStatus::Cancelled,
        "cancelled by you".into(),
    );
    delete_records(inner, vec![id.to_owned()]).await;
    leave_tomb(inner, id, &summary.wire_id, summary.peer).await;
    true
}

/// Keeps a cancel the receiver has not heard of (on disk, 24 h at most) and delivers it
/// now when the peer is online, else at its next session ([`resume_for_peer`]). Used for
/// a waiting send cancelled here and for a running send cancelled while its receiver is
/// away (protocol.md 7.5, SR-20).
pub(super) async fn leave_tomb(inner: &Arc<Inner>, id: &str, wire_id: &str, peer: DeviceId) {
    let tomb = Tomb {
        peer,
        at_ms: unix_ms(),
        wire_id: (wire_id != id).then(|| wire_id.to_owned()),
    };
    lock(&inner.sends).tombs.insert(id.to_owned(), tomb.clone());
    let (dir, id2) = (inner.data_dir.clone(), id.to_owned());
    let _ = tokio::task::spawn_blocking(move || {
        if let Err(e) = send_store::write_tomb(&dir, &id2, &tomb) {
            tracing::debug!(error = %e, "cancel not recorded for the receiver");
        }
    })
    .await;
    if inner.registry.session(&peer).is_some() {
        tell_cancelled(inner, id.to_owned(), wire_id.to_owned(), peer);
    }
}

/// Sends the `DELETE` for a kept cancel in the background and forgets it once the
/// receiver answered (any answer: `204`, or `404` for a transfer it already dropped).
fn tell_cancelled(inner: &Arc<Inner>, id: String, wire_id: String, peer: DeviceId) {
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        let tell = async {
            match connect::outbound(&i2, peer).await {
                Ok(conn) => send::delete_remote(&conn, &wire_id, "user").await,
                Err(_) => false,
            }
        };
        let told = tokio::time::timeout(Duration::from_secs(10), tell)
            .await
            .unwrap_or(false);
        if !told {
            tracing::debug!(peer = %short(&peer), "receiver not told about the cancelled send yet");
            return;
        }
        lock(&i2.sends).tombs.remove(&id);
        let dir = i2.data_dir.clone();
        let _ = tokio::task::spawn_blocking(move || send_store::remove_tomb(&dir, &id)).await;
    });
}

/// Cancels every send to an unpaired peer and deletes the records of those that wait to
/// resume, and any cancel it was still owed (protocol.md 6.4): nothing is resumed toward
/// a device that is no longer paired. A running send deletes its own record as it ends.
pub(crate) async fn drop_peer(inner: &Inner, peer: &DeviceId) {
    let (waiting, tombs) = {
        let mut sends = lock(&inner.sends);
        for o in sends.live.values().filter(|o| o.peer == *peer) {
            o.cancel.send_replace(true);
        }
        let ids = sends.records.ids_where(|s| s.peer == *peer);
        let ids: Vec<String> = ids
            .into_iter()
            .filter(|id| !sends.live.contains_key(id))
            .collect();
        for id in &ids {
            sends.records.remove(id);
        }
        let tombs: Vec<String> = sends
            .tombs
            .iter()
            .filter(|(_, t)| t.peer == *peer)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &tombs {
            sends.tombs.remove(id);
        }
        (ids, tombs)
    };
    {
        let mut book = lock(&inner.transfers);
        for id in &waiting {
            book.amend_finished(
                id,
                TransferDirection::Send,
                TransferStatus::Cancelled,
                "device unpaired".into(),
            );
        }
    }
    delete_records(inner, waiting).await;
    let dir = inner.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for id in tombs {
            send_store::remove_tomb(&dir, &id);
        }
    })
    .await;
}

/// At a session start with `peer` (protocol.md 7.6): starts again every send to it that
/// gave up because it was unreachable (path sources only, at most
/// [`send_store::MAX_AUTO_RESUMES`] times per record; a running send is left alone), and
/// delivers the cancels it is owed.
pub(crate) fn resume_for_peer(inner: &Arc<Inner>, peer: DeviceId) {
    let now = unix_ms();
    let (ids, tombs) = {
        let sends = lock(&inner.sends);
        let ids = sends
            .records
            .ids_where(|s| s.peer == peer && !s.is_expired(now) && s.resumes_on_return());
        let tombs: Vec<(String, String)> = sends
            .tombs
            .iter()
            .filter(|(_, t)| t.peer == peer && !t.is_expired(now))
            .map(|(id, t)| (id.clone(), t.wire_id.clone().unwrap_or_else(|| id.clone())))
            .collect();
        (ids, tombs)
    };
    for (id, wire) in tombs {
        tell_cancelled(inner, id, wire, peer);
    }
    if ids.is_empty() {
        return;
    }
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        for id in ids {
            restart(&i2, &id, |s| s.peer == peer && s.resumes_on_return()).await;
        }
    });
}

/// Loads the records at host start, before the server accepts a connection (protocol.md
/// 7.6): deletes those of unpaired peers and those older than 24 h, indexes the rest, and
/// starts again the sends that were running when the host stopped (path sources only).
/// The others are listed as failed and resumable, as before the restart, and wait for
/// their peer ([`resume_for_peer`]) or a new send of the same files. Undelivered cancels
/// are reloaded too.
pub(crate) async fn load_persisted(inner: &Arc<Inner>) {
    let dir = inner.data_dir.clone();
    let (records, tombs) = tokio::task::spawn_blocking(move || {
        let now = unix_ms();
        (
            send_store::load_all(&dir, now),
            send_store::load_tombs(&dir, now),
        )
    })
    .await
    .unwrap_or_default();
    let (keep, gone): (Vec<SendRecord>, Vec<SendRecord>) = records
        .into_iter()
        .partition(|r| inner.pins.contains(&r.peer));
    delete_records(inner, gone.iter().map(|r| r.id().to_owned()).collect()).await;
    {
        let mut book = lock(&inner.transfers);
        for rec in keep.iter().filter(|r| !r.summary().resumes_at_start()) {
            book.add_waiting(waiting_snapshot(inner, rec));
        }
    }
    let (tombs, stale): (HashMap<String, Tomb>, HashMap<String, Tomb>) = tombs
        .into_iter()
        .partition(|(_, t)| inner.pins.contains(&t.peer));
    let dir = inner.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for id in stale.keys() {
            send_store::remove_tomb(&dir, id);
        }
    })
    .await;
    let ids = {
        let mut sends = lock(&inner.sends);
        sends.records = Index::from_records(&keep);
        sends.tombs = tombs;
        sends.records.ids_where(Summary::resumes_at_start)
    };
    for id in ids {
        restart(inner, &id, Summary::resumes_at_start).await;
    }
}

/// How a send that waits to resume is listed after a restart of this host: failed, with
/// the note it had (or, for a send that was running when the host stopped, why it waits).
fn waiting_snapshot(inner: &Inner, rec: &SendRecord) -> TransferSnapshot {
    let mut snap = TransferSnapshot::new(
        rec.id(),
        &rec.peer,
        TransferDirection::Send,
        TransferStatus::Failed,
        rec.manifest.total_bytes,
        rec.manifest.files.len(),
        None,
    );
    snap.started_at = rec.created_ms / 1000;
    let reason = rec
        .failure
        .clone()
        .unwrap_or_else(|| "this device stopped while sending".into());
    snap.note = Some(send::resume_note(
        &reason,
        &inner.label(&rec.peer),
        rec.summary().resumes_on_return(),
    ));
    snap
}

/// Starts a recorded path send again under its own ID, from what its record holds, and
/// counts it against [`send_store::MAX_AUTO_RESUMES`].
async fn restart(inner: &Arc<Inner>, id: &str, pred: impl FnOnce(&Summary) -> bool) {
    let Some((mut rec, claim)) = claim_record(inner, id, pred).await else {
        return;
    };
    let Some(sources) = rec.path_sources() else {
        claim.release(inner).await;
        return;
    };
    rec.auto_resumes += 1;
    tracing::info!(peer = %short(&rec.peer), id = %id, n = rec.auto_resumes, "resuming a recorded send");
    let manifest = rec.manifest.clone();
    send::launch(
        inner,
        claim,
        manifest,
        sources,
        Some(rec),
        send::Origin::AutoResume,
    )
    .await;
}

/// How often records, waiting snapshots and kept cancels are checked for their 24 h end.
const SWEEP_EVERY: Duration = Duration::from_secs(3600);

/// Hourly: forgets records older than 24 h that no send runs (files, index and waiting
/// snapshot) and kept cancels older than 24 h, so a host that runs for days does not
/// keep or resume what the receiver has long dropped (protocol.md 7.6).
pub(crate) async fn maintenance(inner: Arc<Inner>) {
    loop {
        tokio::time::sleep(SWEEP_EVERY).await;
        sweep(&inner, unix_ms()).await;
    }
}

async fn sweep(inner: &Inner, now: u64) {
    let (records, tombs) = {
        let mut sends = lock(&inner.sends);
        let live: Vec<String> = sends.live.keys().cloned().collect();
        let records: Vec<String> = sends
            .records
            .ids_where(|s| s.is_expired(now))
            .into_iter()
            .filter(|id| !live.contains(id))
            .collect();
        for id in &records {
            sends.records.remove(id);
        }
        let tombs: Vec<String> = sends
            .tombs
            .iter()
            .filter(|(_, t)| t.is_expired(now))
            .map(|(id, _)| id.clone())
            .collect();
        for id in &tombs {
            sends.tombs.remove(id);
        }
        (records, tombs)
    };
    {
        let mut book = lock(&inner.transfers);
        for id in &records {
            book.forget_finished(id, TransferDirection::Send);
        }
    }
    delete_records(inner, records).await;
    let dir = inner.data_dir.clone();
    let _ = tokio::task::spawn_blocking(move || {
        for id in tombs {
            send_store::remove_tomb(&dir, &id);
        }
    })
    .await;
}
