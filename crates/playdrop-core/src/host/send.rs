//! Send side of the transfer plane over HTTP/2 (protocol.md 7.2 to 7.7): build the
//! manifest, offer it, stream up to 4 files plus the bundle concurrently on the one
//! connection, send the `pd-root` trailer, retry failed chunks (SR-22), and resume after any
//! disconnect from the receiver's verified offsets (7.6, SR-21).
//!
//! Every send keeps an on-disk record ([`send_store`]) so it resumes after a restart of
//! this host, when an unreachable receiver comes back, or when the same files are sent to
//! the same device again (protocol.md 7.6 "sender persistence").

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use serde::Serialize;
use tokio::io::AsyncRead;
use tokio::sync::watch;

use super::events::{FileRoot, TransferDirection};
use super::receive::{CHUNK_SIZE_HEADER, ROOT_TRAILER, TOKEN_HEADER};
use super::send_resume::{self, Claim};
use super::send_store::{self, FailureKind, RecordState, SendRecord};
use super::transfers::{ProgressReading, TransferSnapshot, TransferStatus};
use super::{connect, lock, Inner, TransferEnd};
use crate::constants::DEFAULT_CHUNK_SIZE;
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::{
    send_all, BodyReader, Connection, ErrorBody, CHUNKS_CONTENT_TYPE, JSON_CONTENT_TYPE,
};
use crate::session::types::{TransferFrame, TransferState as FrameState};
use crate::session::ErrorCode;
use crate::transfer::chunk::{ChunkHash, HASH_LEN};
use crate::transfer::receiver::MAX_CHUNK_RETRIES;
use crate::transfer::{
    hash_to_hex, ChunkEncoder, FileState, Manifest, ManifestBuilder, PutResponse, ResumeAction,
    SourceInfo, StreamRef, StreamSource, TransferSender, TransferState, TransferStateView,
    UnsavedHashes,
};
use crate::{Error, Result};

/// Concurrent file `PUT`s besides the bundle (protocol.md section 10).
pub const FILE_CONCURRENCY: usize = 4;

/// How long a sender keeps trying to reach the receiver after a drop, unless
/// `HostConfig::reconnect_for` says otherwise. *Choice:* ten minutes covers a Wi-Fi switch
/// or a restarted daemon; the user can cancel sooner. A send that gives up keeps its
/// record and resumes later ([`resume_for_peer`]).
pub(crate) const RECONNECT_FOR: Duration = Duration::from_secs(600);

/// Times a send offers its manifest again after the receiver answered `404` for it (it
/// swept the transfer after 24 h, or its folder was wiped) before it fails. *Choice:* one
/// re-offer is the normal case; a receiver that keeps forgetting has a problem a re-offer
/// cannot fix.
const MAX_REOFFERS: u32 = 3;

/// Least time between two record writes triggered by `PUT`s that finished (protocol.md
/// 7.6). *Choice:* a send of many medium files finishes a `PUT` every few milliseconds,
/// and a durable write each time would cost more than the files; hashes lost to a crash
/// inside this window are rebuilt from the source at resume
/// ([`TransferSender::plan_resume`]).
const PERSIST_EVERY: Duration = Duration::from_secs(1);

/// During a long `PUT`, the encoder's hashes are copied into the sender (and so into the
/// next record write) every this many bytes or [`PERSIST_EVERY`], whichever comes first,
/// so a crash mid-file loses at most that much hashing work. The same 16 MiB as the
/// receiver's durability batch (protocol.md 7.6).
const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;

/// Hash log entries allowed beyond twice the valid ones before the log is rewritten.
const LOG_SLACK: usize = 4096;

/// Progress event period.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// Response bodies other than the session are small JSON.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

/// Rounds in a row that may end on a lost stream while the receiver verifies nothing new
/// before the send fails. *Choice:* a receiver that keeps resetting the upload without
/// progress has a problem resuming cannot fix (for example its disk refuses writes), and
/// retrying it forever only hides that from the user.
const MAX_STALLED_ROUNDS: u32 = 8;

/// The least time a send keeps re-planning without progress before it fails, however many
/// rounds that takes. *Choice:* rounds come about 100 ms apart, so the round count alone
/// gave up after about a second; a receiver whose placement fails for a few seconds (a
/// full disk being emptied, a folder being moved back, an antivirus holding a file) must
/// get the chance to finish.
const MIN_STALL_TIME: Duration = Duration::from_secs(10);

/// Counts connected rounds that ended without the receiver verifying anything new, and
/// since when.
#[derive(Debug)]
struct Stall {
    best: u64,
    rounds: u32,
    since: Instant,
}

impl Default for Stall {
    fn default() -> Self {
        Self {
            best: 0,
            rounds: 0,
            since: Instant::now(),
        }
    }
}

impl Stall {
    /// Notes the receiver's verified byte count at the start of a round; any growth
    /// clears the count and the clock.
    fn observe(&mut self, verified: u64) {
        if verified > self.best {
            self.best = verified;
            self.rounds = 0;
            self.since = Instant::now();
        }
    }

    /// Counts one round lost to a dropped stream; true once too many came in a row and
    /// [`MIN_STALL_TIME`] has passed without progress.
    fn lost_round(&mut self) -> bool {
        self.lost_round_at(Instant::now())
    }

    fn lost_round_at(&mut self, now: Instant) -> bool {
        self.rounds += 1;
        self.rounds > MAX_STALLED_ROUNDS
            && now.saturating_duration_since(self.since) >= MIN_STALL_TIME
    }
}

/// One thing to send (`docs/core-api.md` section 3).
#[derive(Debug, Clone)]
pub enum SendSource {
    /// A file or folder, walked as before (desktop).
    Path(PathBuf),
    /// One file behind a reopenable handle (mobile share targets).
    Stream(StreamSource),
}

/// Builds the manifest and per-fid sources for `sources` (protocol.md 7.2).
pub(crate) async fn build_manifest(
    sources: Vec<SendSource>,
    note: Option<String>,
) -> Result<(Manifest, Vec<SourceInfo>)> {
    if sources.is_empty() {
        return Err(Error::BadRequest("nothing to send".into()));
    }
    let mut b = ManifestBuilder::new(DEFAULT_CHUNK_SIZE);
    if let Some(n) = note {
        b = b.note(n);
    }
    for src in sources {
        match src {
            SendSource::Path(p) => b.add_path(&p).await?,
            SendSource::Stream(s) => b.add_stream(s).await?,
        }
    }
    b.build()
}

/// Bookkeeping for a send, so `transfer` frames reach it (protocol.md 5.13).
pub(crate) struct Outgoing {
    pub(super) peer: DeviceId,
    /// The transfer ID on the wire, which `transfer` frames name; it differs from the
    /// running-sends key once a send continued under a new ID.
    pub(super) wire_id: String,
    pub(super) decision: watch::Sender<Option<FrameState>>,
    pub(super) cancel: Arc<watch::Sender<bool>>,
}

/// What a finished send reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct TransferSummary {
    /// Transfer ID.
    pub id: String,
    /// Receiver's Device ID.
    pub peer: String,
    /// Total file bytes.
    pub bytes: u64,
    /// Wall time from offer to completion, in milliseconds.
    pub elapsed_ms: u64,
    /// Root of every streamed file (protocol.md 7.4).
    pub roots: Vec<FileRoot>,
    /// Root of the bundle stream, if any (7.7).
    pub bundle_root: Option<String>,
    /// Reconnections needed.
    pub reconnects: u32,
}

type Outcome = std::result::Result<TransferSummary, String>;

/// Why a run ended without completing.
#[derive(Debug)]
struct RunError {
    /// Shown to the user.
    reason: String,
    /// Set when the record is kept and the send can resume later: the receiver could not
    /// be reached, or kept dropping the upload. Every other end deletes the record.
    resumable: Option<FailureKind>,
}

impl RunError {
    fn fatal(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            resumable: None,
        }
    }

    fn resumable(kind: FailureKind, reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            resumable: Some(kind),
        }
    }
}

/// A running send. Progress also arrives as [`HostEvent::TransferProgress`].
#[derive(Debug)]
pub struct TransferHandle {
    id: String,
    total: u64,
    sent: Arc<AtomicU64>,
    done: watch::Receiver<Option<Outcome>>,
    cancel: Arc<watch::Sender<bool>>,
}

impl TransferHandle {
    /// Transfer ID.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Total file bytes.
    pub fn total_bytes(&self) -> u64 {
        self.total
    }

    /// File bytes the receiver holds and keeps, as of the last connected round, plus
    /// those handed to the connection since (`docs/core-api.md` section 12).
    pub fn bytes_sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    /// Cancels the send; the receiver deletes its `.part` files (protocol.md 7.5).
    pub fn cancel(&self) {
        self.cancel.send_replace(true);
    }

    /// Waits for the end of the send.
    pub async fn wait(&mut self) -> Result<TransferSummary> {
        let r = self
            .done
            .wait_for(Option::is_some)
            .await
            .map_err(|_| Error::Protocol("send task ended".into()))?;
        match r.as_ref() {
            Some(Ok(s)) => Ok(s.clone()),
            Some(Err(e)) => Err(Error::Protocol(e.clone())),
            None => Err(Error::Protocol("send task ended".into())),
        }
    }
}

struct Job {
    inner: Arc<Inner>,
    /// The ID this device knows the send by: the handle's, the events', `transfers()`',
    /// the running-sends key, `cancel_transfer`'s and the record's. It stays the same
    /// when the send continues under a new transfer ID on the wire ([`continue_fresh`]);
    /// only the offer and the HTTP paths use `manifest.id`.
    id: String,
    peer: DeviceId,
    manifest: Manifest,
    sources: std::sync::Mutex<Option<Vec<SourceInfo>>>,
    sender: tokio::sync::Mutex<Option<TransferSender>>,
    sent: Arc<AtomicU64>,
    chunk_size: AtomicU32,
    cancel: watch::Receiver<bool>,
    decision: watch::Receiver<Option<FrameState>>,
    /// Set when the receiver reported the transfer cancelled (a `410 cancelled`, a
    /// `cancelled` state or a `transfer` frame), so the end is reported as a cancel.
    remote_cancelled: AtomicBool,
    /// Set when the first file or bundle `PUT` starts moving bytes. Until then the send is
    /// `offered` and no progress is reported, so a UI never shows a running transfer while
    /// the receiver is still deciding. It stays set across reconnects.
    streaming: AtomicBool,
    /// Which files the receiver has finished, for `files_done`.
    done: std::sync::Mutex<DoneFiles>,
    /// The rate of the last progress reading, reused for the final one.
    rate: AtomicU64,
    /// What is kept on disk for this send (protocol.md 7.6): verified streams, re-offer
    /// and auto-resume counts; hashes are filled in from `sender` at each write. `None`
    /// when no record could be made (a source path that cannot be made absolute): the send
    /// then runs as before, in memory only.
    record: std::sync::Mutex<Option<SendRecord>>,
    /// Throttles record writes ([`PERSIST_EVERY`]).
    persist: tokio::sync::Mutex<Option<Instant>>,
    /// Held by every file operation on the record, on a blocking thread; true once the
    /// send ended. A write abandoned by its caller (the progress ticker dropped when the
    /// run ends) still runs to its end on its thread, so it checks this flag under the
    /// lock and never lands after the record was deleted or marked failed.
    gate: Arc<std::sync::Mutex<bool>>,
    /// Something worth writing changed since the last record write.
    dirty: AtomicBool,
    /// Committed length of the hash log in bytes (the state vouches for it); read and
    /// moved only under the gate, so a write appends exactly where the last one ended.
    log_len: Arc<AtomicU64>,
    /// The next write replaces the hash log instead of appending to it.
    rewrite_log: AtomicBool,
    /// The record's fixed part is on disk.
    whole_written: AtomicBool,
    /// `404` answers to `GET` over the life of the record (the receiver forgot the
    /// transfer); starts from the record's count.
    reoffers: AtomicU32,
    /// The receiver answered this offer before (the record has its chunk size), so the
    /// first round asks for its state instead of offering again: a receiver that forgot a
    /// transfer it had verified in full then answers `404`, which completes the send,
    /// instead of taking a fresh offer and receiving everything again.
    known_offer: bool,
    /// Where this send came from.
    origin: Origin,
    /// True until the first `GET` answer of the send.
    first_view: AtomicBool,
    /// The receiver had ended the transfer an adopted send took over: it continues as a
    /// new transfer ([`continue_fresh`]).
    ended_before: AtomicBool,
}

/// Files the receiver has finished, by fid: a streamed file once its `PUT` came back
/// `verified`, every bundle member once the bundle did, a skipped file always, and a
/// zero-length file only when the whole transfer is complete (it is made at completion,
/// protocol.md 7.2).
#[derive(Debug, Default)]
struct DoneFiles {
    done: Vec<bool>,
    count: usize,
}

impl DoneFiles {
    fn new(files: usize) -> Self {
        Self {
            done: vec![false; files],
            count: 0,
        }
    }

    fn mark(&mut self, fid: u32) {
        if let Some(d) = self.done.get_mut(fid as usize) {
            if !*d {
                *d = true;
                self.count += 1;
            }
        }
    }

    fn mark_all(&mut self) {
        self.done.fill(true);
        self.count = self.done.len();
    }

    /// Rebuilds the set from the receiver's `GET` view (protocol.md 7.5), so a resumed
    /// send counts what the receiver really holds, not what an earlier round saw.
    fn from_view(manifest: &Manifest, view: &TransferStateView) -> Self {
        let mut out = Self::new(manifest.files.len());
        if view.state == TransferState::Complete {
            out.mark_all();
            return out;
        }
        let mut states = vec![None; manifest.files.len()];
        for f in &view.files {
            if let Some(slot) = states.get_mut(f.fid as usize) {
                *slot = Some(f.state);
            }
        }
        let bundle_verified = view
            .bundle
            .as_ref()
            .is_some_and(|b| b.state == FileState::Verified);
        for f in &manifest.files {
            let state = states.get(f.fid as usize).copied().flatten();
            let done = if state == Some(FileState::Skipped) {
                true
            } else if f.in_bundle {
                bundle_verified
            } else if f.size == 0 {
                false
            } else {
                state == Some(FileState::Verified)
            };
            if done {
                out.mark(f.fid);
            }
        }
        out
    }
}

impl Job {
    /// How a failed run is reported: a cancel by either side, or a failure.
    fn end_for(&self, reason: String) -> TransferEnd {
        if *self.cancel.borrow() {
            TransferEnd::Cancelled("cancelled by you".into())
        } else if matches!(*self.decision.borrow(), Some(FrameState::Declined)) {
            TransferEnd::Cancelled("declined by the receiver".into())
        } else if self.remote_cancelled.load(Ordering::Acquire)
            || matches!(*self.decision.borrow(), Some(FrameState::Cancelled))
        {
            TransferEnd::Cancelled("cancelled by the receiver".into())
        } else {
            TransferEnd::Failed(reason)
        }
    }

    fn mark_remote_cancel(&self) {
        self.remote_cancelled.store(true, Ordering::Release);
    }

    /// Notes that bytes are moving; the first call turns the snapshot `active`.
    fn mark_streaming(&self) {
        if !self.streaming.load(Ordering::Acquire) && !self.streaming.swap(true, Ordering::AcqRel) {
            lock(&self.inner.transfers).set_state(
                &self.id,
                TransferDirection::Send,
                TransferStatus::Active,
            );
        }
    }

    fn files_done(&self) -> usize {
        lock(&self.done).count
    }

    /// Counts the files a `PUT` that came back `verified` finished.
    fn target_verified(&self, target: Target) {
        {
            let mut done = lock(&self.done);
            match target {
                Target::File(fid) => done.mark(fid),
                Target::Bundle => {
                    for f in self.manifest.files.iter().filter(|f| f.in_bundle) {
                        done.mark(f.fid);
                    }
                }
            }
        }
        match target {
            Target::File(fid) => self.note_verified([fid], false),
            Target::Bundle => self.note_verified([], true),
        }
    }

    /// Copies a running encoder's hashes into the sender, so the next record write holds
    /// them and a long `PUT` that dies keeps most of its hashing work (protocol.md 7.6).
    /// The write itself is left to the progress ticker, off the data path.
    async fn checkpoint(&self, stream: StreamRef, hashes: &[ChunkHash]) {
        if let Some(s) = self.sender.lock().await.as_mut() {
            s.sync_hashes(stream, hashes);
        }
        self.dirty.store(true, Ordering::Release);
    }

    /// Resets the count from the receiver's view at the start of a connected round.
    fn sync_done(&self, view: &TransferStateView) {
        *lock(&self.done) = DoneFiles::from_view(&self.manifest, view);
    }

    /// The reading the progress ticker and the final report send.
    fn reading(&self, bytes: u64, bytes_per_sec: u64) -> ProgressReading {
        ProgressReading {
            bytes: bytes.min(self.manifest.total_bytes),
            total: self.manifest.total_bytes,
            bytes_per_sec,
            files_done: self.files_done(),
        }
    }

    /// Notes that the receiver verified streams, in the record (for a later `404`).
    fn note_verified(&self, files: impl IntoIterator<Item = u32>, bundle: bool) {
        let mut rec = lock(&self.record);
        let Some(rec) = rec.as_mut() else { return };
        let mut changed = false;
        for fid in files {
            if !rec.verified.contains(&fid) {
                rec.verified.push(fid);
                changed = true;
            }
        }
        if bundle && !rec.bundle_verified {
            rec.bundle_verified = true;
            changed = true;
        }
        if changed {
            self.dirty.store(true, Ordering::Release);
        }
    }

    /// How the send gave up, once its record was kept.
    fn failure_kind(&self) -> Option<FailureKind> {
        lock(&self.record).as_ref().and_then(|r| r.failure_kind)
    }

    /// Notes what the receiver's `GET` view reports verified.
    fn note_view(&self, view: &TransferStateView) {
        let files = view
            .files
            .iter()
            .filter(|f| f.state == FileState::Verified)
            .map(|f| f.fid);
        let bundle = view
            .bundle
            .as_ref()
            .is_some_and(|b| b.state == FileState::Verified);
        self.note_verified(files, bundle);
    }

    /// True when the record shows every stream verified ([`SendRecord::all_streams_verified`]).
    fn all_streams_verified(&self) -> bool {
        lock(&self.record)
            .as_ref()
            .is_some_and(SendRecord::all_streams_verified)
    }

    /// The record write due now: the base record plus the chunk size and the hash counts
    /// the sender holds, and the hashes added or changed since the last write. Before the
    /// sender exists (offer time, or a resumed send before its first `GET`), the counts and
    /// hashes loaded with the record are kept as they are.
    async fn snapshot(&self) -> Option<RecordWrite> {
        let mut rewrite = self.rewrite_log.swap(false, Ordering::AcqRel);
        let mut unsaved = Vec::new();
        let mut counts = None;
        if let Some(s) = self.sender.lock().await.as_mut() {
            let files: Vec<(u32, usize)> = self
                .manifest
                .files
                .iter()
                .filter(|f| !f.in_bundle && f.size > 0)
                .map(|f| (f.fid, s.hashes(f.fid).len()))
                .collect();
            let bundle = s.bundle_hashes().len();
            let valid = files.iter().map(|(_, n)| *n).sum::<usize>() + bundle;
            // The log only grows; once it holds far more than the valid hashes (streams
            // restarted at 0, many small appends), it is written again from scratch.
            let logged = usize::try_from(self.log_len.load(Ordering::Acquire))
                .unwrap_or(usize::MAX)
                / send_store::ENTRY;
            rewrite |= logged > 2 * valid + LOG_SLACK;
            if rewrite {
                s.mark_all_unsaved();
            }
            unsaved = s.take_unsaved();
            counts = Some((files, bundle, s.chunk_size()));
        }
        let mut guard = lock(&self.record);
        let rec = guard.as_mut()?;
        match counts {
            Some((files, bundle, chunk_size)) => {
                rec.chunk_size = Some(chunk_size);
                rec.set_hash_counts(&files, bundle);
            }
            None if rewrite => {
                // Before the sender exists the record's own hashes are the valid ones.
                unsaved = rec
                    .hashes
                    .iter()
                    .map(|(fid, h)| UnsavedHashes {
                        stream: StreamRef::File(*fid),
                        start: 0,
                        hashes: h.clone(),
                    })
                    .chain((!rec.bundle_hashes.is_empty()).then(|| UnsavedHashes {
                        stream: StreamRef::Bundle,
                        start: 0,
                        hashes: rec.bundle_hashes.clone(),
                    }))
                    .collect();
            }
            None => {}
        }
        // The fixed part (manifest, sources) is written once; later writes carry only
        // what changes.
        let whole = (!self.whole_written.load(Ordering::Acquire)).then(|| rec.clone());
        Some(RecordWrite {
            whole,
            state: rec.state(),
            summary: rec.summary(),
            unsaved,
            rewrite,
        })
    }

    /// Notes that the record changed and writes it now (offer time, the chunk size fixed,
    /// a re-offer). Changes made while bytes stream only set `dirty`; the progress ticker
    /// writes them ([`save`](Self::save)), so a record write never stalls a `PUT`.
    async fn persist(&self) {
        self.dirty.store(true, Ordering::Release);
        self.save(true).await;
    }

    /// Writes the record if it changed: always with `force`, otherwise only when
    /// [`PERSIST_EVERY`] passed since the last write and no write is running.
    async fn save(&self, force: bool) {
        if !self.dirty.load(Ordering::Acquire) {
            return;
        }
        let mut last = if force {
            self.persist.lock().await
        } else {
            match self.persist.try_lock() {
                Ok(p) => p,
                Err(_) => return,
            }
        };
        if !self.dirty.load(Ordering::Acquire)
            || (!force && last.is_some_and(|t| t.elapsed() < PERSIST_EVERY))
        {
            return;
        }
        self.dirty.store(false, Ordering::Release);
        if let Some(w) = self.snapshot().await {
            if let Err(e) = self.write_gated(Some(w), false).await {
                self.dirty.store(true, Ordering::Release);
                tracing::debug!(error = %e, "send record not written");
            }
        }
        *last = Some(Instant::now());
    }

    /// Writes a record (or, with `None`, deletes it) on a blocking thread under the gate,
    /// keeping the host's index of records in step. `last` closes the gate: no write
    /// happens after it. Returns whether the record is on disk afterwards. A write that
    /// fails makes the next one rewrite the hash log, since the hashes it carried are no
    /// longer pending in the sender.
    async fn write_gated(&self, write: Option<RecordWrite>, last: bool) -> Result<bool> {
        let (inner, gate) = (Arc::clone(&self.inner), Arc::clone(&self.gate));
        let log_len = Arc::clone(&self.log_len);
        let id = self.id.clone();
        let whole = write.as_ref().is_some_and(|w| w.whole.is_some());
        let result = tokio::task::spawn_blocking(move || {
            let mut closed = lock(&gate);
            if *closed {
                return Ok(false);
            }
            if let Some(w) = write {
                // A last write that fails leaves the gate open, so the delete that
                // follows it in `close` still runs.
                let len = send_store::write(
                    &inner.data_dir,
                    &id,
                    w.whole.as_ref(),
                    w.state,
                    log_len.load(Ordering::Acquire),
                    &w.unsaved,
                    w.rewrite,
                )?;
                log_len.store(len, Ordering::Release);
                lock(&inner.sends).records.upsert(&id, w.summary);
                *closed = last;
                Ok(true)
            } else {
                lock(&inner.sends).records.remove(&id);
                send_store::remove(&inner.data_dir, &id);
                *closed = last;
                Ok(false)
            }
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        if whole && matches!(result, Ok(true)) {
            self.whole_written.store(true, Ordering::Release);
        }
        if result.is_err() {
            // The hashes it carried are no longer pending in the sender.
            self.rewrite_log.store(true, Ordering::Release);
        }
        result
    }

    /// Ends the record when the send ends: with `failure` it is kept, marked failed with
    /// that kind, and waits for the peer or a new send of the same files (returns true when
    /// it is on disk); without, it is deleted. No write happens afterwards.
    async fn close(&self, failure: Option<(&str, FailureKind)>) -> bool {
        let _last = self.persist.lock().await;
        if let Some((why, kind)) = failure {
            if let Some(rec) = lock(&self.record).as_mut() {
                rec.failed_at_ms = Some(send_resume::unix_ms());
                rec.failure = Some(why.to_owned());
                rec.failure_kind = Some(kind);
            }
            // The last write carries every hash: a ticker write dropped with the run
            // may have taken some out of the sender without landing them.
            self.rewrite_log.store(true, Ordering::Release);
            if let Some(w) = self.snapshot().await {
                match self.write_gated(Some(w), true).await {
                    Ok(kept) => return kept,
                    Err(e) => tracing::debug!(error = %e, "failed send record not written"),
                }
            }
        }
        if let Err(e) = self.write_gated(None, true).await {
            tracing::debug!(error = %e, "send record not deleted");
        }
        false
    }
}

/// One record write: the JSON part and the hashes to add to the log.
struct RecordWrite {
    /// The whole record, for the first write; `None` afterwards.
    whole: Option<SendRecord>,
    state: RecordState,
    summary: send_store::Summary,
    unsaved: Vec<UnsavedHashes>,
    /// Replace the whole hash log with `unsaved` instead of appending.
    rewrite: bool,
}

/// The note a send that gave up but will resume carries (`TransferSnapshot::note`,
/// `HostEvent::TransferFailed`). Only a send the host restarts by itself (path sources,
/// receiver unreachable, auto-resumes left) says it resumes when the peer is back; the
/// rest resume when the same items are sent again (a share target's openers died with the
/// send).
pub(super) fn resume_note(reason: &str, who: &str, auto: bool) -> String {
    if auto {
        format!("{reason}; resumes when {who} is back or when these files are sent to it again")
    } else {
        format!("{reason}; resumes when these files are sent to {who} again")
    }
}

/// Builds the manifest for `sources` and starts sending to `peer`. When an earlier send of
/// the same items to the same peer left a record (it failed, or this host stopped while it
/// ran), the new send takes over that record's transfer ID and hashes, so the receiver's
/// verified bytes are not sent again (protocol.md 7.3: the same manifest under a known ID
/// gets the receiver's view back).
pub(crate) async fn start(
    inner: &Arc<Inner>,
    peer: DeviceId,
    sources: Vec<SendSource>,
    note: Option<String>,
) -> Result<TransferHandle> {
    if !inner.pins.contains(&peer) {
        return Err(Error::NotPaired);
    }
    let (manifest, sources) = build_manifest(sources, note).await?;
    if let Some((mut rec, claim)) = send_resume::adopt(inner, &peer, &manifest, &sources).await {
        tracing::info!(peer = %short(&peer), id = %rec.id(), "same files as an earlier send; resuming it");
        // Sent again by the user: the automatic resumes start counting afresh.
        rec.auto_resumes = 0;
        rec.forget_unverifiable_hashes();
        let manifest = rec.manifest.clone();
        return Ok(launch(inner, claim, manifest, sources, Some(rec), Origin::Adopted).await);
    }
    let record = SendRecord::new(peer, send_resume::unix_ms(), &manifest, &sources);
    if record.is_none() {
        tracing::debug!("a source path cannot be made absolute; this send keeps no record");
    }
    let claim = send_resume::claim_new(inner, peer, &manifest.id);
    Ok(launch(inner, claim, manifest, sources, record, Origin::New).await)
}

/// Where a send came from, which decides what an ended transfer on the receiver means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Origin {
    /// A new send under a fresh ID.
    New,
    /// The user sent the same files again and the send took over a record.
    Adopted,
    /// The host started a recorded send again by itself.
    AutoResume,
}

/// Starts the send task for a claimed ID and returns its handle.
pub(super) async fn launch(
    inner: &Arc<Inner>,
    claim: Claim,
    manifest: Manifest,
    sources: Vec<SourceInfo>,
    record: Option<SendRecord>,
    origin: Origin,
) -> TransferHandle {
    let sent = Arc::new(AtomicU64::new(0));
    let (done_tx, done_rx) = watch::channel(None);
    let (id, total, cancel) = (
        claim.id.clone(),
        manifest.total_bytes,
        Arc::clone(&claim.cancel),
    );
    let job = Job::new(
        inner,
        claim,
        manifest,
        sources,
        record,
        origin,
        Arc::clone(&sent),
    );
    begin(job, done_tx).await;
    TransferHandle {
        id,
        total,
        sent,
        done: done_rx,
        cancel,
    }
}

/// Lists the send, writes its record (offer time: on disk before the first `POST`,
/// protocol.md 7.2 "id") and spawns its task. A resumed send re-uses its ID, so a
/// finished snapshot of the earlier attempt is dropped first: `transfers()` lists each ID
/// once.
fn begin(
    job: Arc<Job>,
    done_tx: watch::Sender<Option<Outcome>>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    // Boxed with an explicit `Send`: a send that continues under a new ID starts the next
    // task from inside this one ([`continue_fresh`]), and the compiler cannot infer `Send`
    // through that cycle.
    Box::pin(async move {
        {
            let mut book = lock(&job.inner.transfers);
            book.forget_finished(&job.id, TransferDirection::Send);
            book.begin(TransferSnapshot::new(
                &job.id,
                &job.peer,
                TransferDirection::Send,
                TransferStatus::Offered,
                job.manifest.total_bytes,
                job.manifest.files.len(),
                job.manifest.note.clone(),
            ));
        }
        job.persist().await;
        let inner = Arc::clone(&job.inner);
        inner.spawn(drive(job, done_tx));
    })
}

impl Job {
    #[allow(clippy::too_many_arguments, reason = "every field a send starts from")]
    fn new(
        inner: &Arc<Inner>,
        claim: Claim,
        manifest: Manifest,
        sources: Vec<SourceInfo>,
        record: Option<SendRecord>,
        origin: Origin,
        sent: Arc<AtomicU64>,
    ) -> Arc<Self> {
        // Running again: the record is no longer one that waits for the peer.
        let record = record.map(|mut r| {
            r.failed_at_ms = None;
            r.failure = None;
            r.failure_kind = None;
            r
        });
        let reoffers = record.as_ref().map_or(0, |r| r.reoffers);
        let log_len = record.as_ref().map_or(0, |r| r.log_len);
        let known_offer = record.as_ref().is_some_and(|r| r.chunk_size.is_some());
        // A record that came from disk has its fixed part there already.
        let whole_written = origin != Origin::New;
        let file_count = manifest.files.len();
        Arc::new(Self {
            inner: Arc::clone(inner),
            id: claim.id,
            peer: claim.peer,
            manifest,
            sources: std::sync::Mutex::new(Some(sources)),
            sender: tokio::sync::Mutex::new(None),
            sent,
            chunk_size: AtomicU32::new(DEFAULT_CHUNK_SIZE),
            cancel: claim.cancel.subscribe(),
            decision: claim.decision_rx,
            remote_cancelled: AtomicBool::new(false),
            streaming: AtomicBool::new(false),
            done: std::sync::Mutex::new(DoneFiles::new(file_count)),
            rate: AtomicU64::new(0),
            record: std::sync::Mutex::new(record),
            persist: tokio::sync::Mutex::new(None),
            gate: Arc::new(std::sync::Mutex::new(false)),
            dirty: AtomicBool::new(false),
            log_len: Arc::new(AtomicU64::new(log_len)),
            rewrite_log: AtomicBool::new(false),
            whole_written: AtomicBool::new(whole_written),
            reoffers: AtomicU32::new(reoffers),
            known_offer,
            origin,
            first_view: AtomicBool::new(true),
            ended_before: AtomicBool::new(false),
        })
    }
}

/// The send task: runs the send, settles its record, reports the end.
async fn drive(job: Arc<Job>, done_tx: watch::Sender<Option<Outcome>>) {
    let (i2, id2, peer) = (Arc::clone(&job.inner), job.id.clone(), job.peer);
    let ticker = progress_ticker(Arc::clone(&job));
    let outcome = tokio::select! {
        o = run(Arc::clone(&job)) => o,
        () = ticker => Err(RunError::fatal("progress ticker ended")),
    };
    if outcome.is_err() && job.ended_before.load(Ordering::Acquire) && !*job.cancel.borrow() {
        continue_fresh(&job, done_tx).await;
        return;
    }
    // The record is settled before the ID leaves the running sends, so a new send of the
    // same files (or a peer coming back) never takes over a record this task still writes.
    let end = match outcome {
        Ok(s) => {
            job.close(None).await;
            Ok(s)
        }
        Err(e) => Err(finish_failed(&job, e).await),
    };
    let kept = matches!(end, Err(TransferEnd::FailedResumable(_)));
    if end.is_ok() {
        // One last reading at 100 %, so the event stream and the snapshot both end
        // with every file counted before `TransferComplete` (the ticker may not have
        // run since the last file finished). A send with nothing to stream (only empty
        // files, or everything skipped) passes through `active` here, so the order is
        // always offered, active, complete.
        job.mark_streaming();
        lock(&job.done).mark_all();
        let rate = job.rate.load(Ordering::Relaxed);
        i2.transfer_progress(
            &id2,
            &peer,
            TransferDirection::Send,
            job.reading(job.manifest.total_bytes, rate),
        );
    }
    let (transfer_end, outcome) = match end {
        Ok(s) => (
            TransferEnd::Complete {
                bytes: s.bytes,
                elapsed_ms: s.elapsed_ms,
                roots: s.roots.clone(),
                bundle_root: s.bundle_root.clone(),
                saved: Vec::new(),
                folder: None,
                delivered: Vec::new(),
            },
            Ok(s),
        ),
        Err(end) => {
            let reason = match &end {
                TransferEnd::Failed(r)
                | TransferEnd::FailedResumable(r)
                | TransferEnd::Cancelled(r) => r.clone(),
                TransferEnd::Complete { .. } => String::new(),
            };
            (end, Err(reason))
        }
    };
    // The list is updated while the ID is still running, so a resume that claims it
    // right after never has its own snapshot moved to the finished list.
    i2.transfer_ended(&id2, &peer, TransferDirection::Send, transfer_end);
    lock(&i2.sends).live.remove(&id2);
    done_tx.send_replace(Some(outcome));
    if kept {
        if *job.cancel.borrow() {
            // A cancel that came after the run ended found the send still listed as
            // running; it is carried out on the kept record now.
            send_resume::cancel(&i2, &id2).await;
        } else if job.failure_kind() == Some(FailureKind::Unreachable)
            && i2.registry.session(&peer).is_some()
        {
            // The peer came back while this send was giving up, after its session start
            // had passed this send by.
            send_resume::resume_for_peer(&i2, peer);
        }
    }
}

/// The receiver had ended the transfer a send of the same files took over (cancelled,
/// declined, expired or failed while this device was away): the record is dropped and
/// the files are offered as a new transfer under a new ID, with the same handle.
async fn continue_fresh(job: &Job, done_tx: watch::Sender<Option<Outcome>>) {
    let inner = Arc::clone(&job.inner);
    // The old record goes; the ID stays registered as running throughout, so a cancel in
    // between reaches the shared cancel channel and the new offer starts cancelled. Both
    // records share the local ID, so the old one must be gone before the new one is
    // written (offer time, in `begin`); a crash in that gap leaves no record, and the
    // next send of the same files is simply a fresh offer, as before Phase 9.
    job.close(None).await;
    let pending = lock(&job.sources).take();
    let sources = match pending {
        Some(s) => s,
        None => job
            .sender
            .lock()
            .await
            .as_ref()
            .map(|s| s.sources().to_vec())
            .unwrap_or_default(),
    };
    let mut manifest = job.manifest.clone();
    manifest.id = crate::transfer::manifest::new_transfer_id();
    tracing::info!(peer = %short(&job.peer), id = %job.id, "the receiver had ended the earlier transfer; offering the files anew");
    let mut record = SendRecord::new(job.peer, send_resume::unix_ms(), &manifest, &sources);
    if let Some(r) = record.as_mut() {
        // The record stays under the ID this device knows the send by.
        r.public_id = Some(job.id.clone());
    }
    let Some(claim) = send_resume::reclaim(&inner, &job.id, &manifest.id) else {
        // Not running any more (nothing should take a running ID away): end it plainly.
        let why = "the receiver had ended the earlier transfer".to_owned();
        inner.transfer_ended(
            &job.id,
            &job.peer,
            TransferDirection::Send,
            TransferEnd::Failed(why.clone()),
        );
        done_tx.send_replace(Some(Err(why)));
        return;
    };
    let next = Job::new(
        &inner,
        claim,
        manifest,
        sources,
        record,
        Origin::New,
        Arc::clone(&job.sent),
    );
    begin(next, done_tx).await;
}

/// Settles the record of a send that did not complete and says how it ended. A send that
/// lost its receiver keeps its record, marked failed, and says it will resume; a cancel by
/// either side, a decline or any other failure deletes it.
async fn finish_failed(job: &Job, e: RunError) -> TransferEnd {
    let end = job.end_for(e.reason.clone());
    if let (TransferEnd::Failed(reason), Some(kind)) = (&end, e.resumable) {
        if job.close(Some((reason, kind))).await {
            let auto = lock(&job.record)
                .as_ref()
                .is_some_and(|r| r.summary().resumes_on_return());
            let who = job.inner.label(&job.peer);
            tracing::info!(peer = %short(&job.peer), id = %job.id, "send gave up; its record waits");
            return TransferEnd::FailedResumable(resume_note(reason, &who, auto));
        }
    }
    job.close(None).await;
    end
}

/// Reports progress every [`PROGRESS_EVERY`] once bytes are moving. Nothing is reported
/// while the offer waits for the receiver, so a reading always means a running transfer.
async fn progress_ticker(job: Arc<Job>) {
    let mut last = (Instant::now(), 0u64);
    loop {
        tokio::time::sleep(PROGRESS_EVERY).await;
        job.save(false).await;
        let bytes = job.sent.load(Ordering::Relaxed);
        let now = Instant::now();
        if !job.streaming.load(Ordering::Acquire) {
            // The first rate is measured from when streaming began, not from the offer.
            last = (now, bytes);
            continue;
        }
        let dt = now.duration_since(last.0).as_secs_f64().max(0.001);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )] // display rate only
        let rate = (bytes.saturating_sub(last.1) as f64 / dt) as u64;
        last = (now, bytes);
        job.rate.store(rate, Ordering::Relaxed);
        job.inner.transfer_progress(
            &job.id,
            &job.peer,
            TransferDirection::Send,
            job.reading(bytes, rate),
        );
    }
}

/// Routes a `transfer` frame to the send it names, if the frame came from its receiver
/// (protocol.md 5.13, SR-13).
pub(crate) fn on_transfer_frame(inner: &Inner, peer: DeviceId, tf: &TransferFrame) {
    let sends = lock(&inner.sends);
    if let Some(o) = sends
        .live
        .values()
        .find(|o| o.wire_id == tf.id && o.peer == peer)
    {
        o.decision.send_replace(Some(tf.state.clone()));
    }
}

/// One step's failure.
pub(crate) enum Step {
    /// The connection is gone or unusable: reconnect and resume.
    Conn(String),
    /// The receiver refused: stop.
    Fatal(String),
}

pub(crate) async fn request_json(
    conn: &Connection,
    method: Method,
    path: &str,
    body: Option<Bytes>,
) -> std::result::Result<(StatusCode, Bytes), Step> {
    let mut b = Request::builder().method(method).uri(path);
    if body.is_some() {
        b = b.header(http::header::CONTENT_TYPE, JSON_CONTENT_TYPE);
    }
    let req = b.body(()).map_err(|e| Step::Fatal(e.to_string()))?;
    let (resp, mut send) = conn
        .request(req, body.is_none())
        .await
        .map_err(|e| Step::Conn(e.to_string()))?;
    if let Some(body) = body {
        send_all(&mut send, body, true)
            .await
            .map_err(|e| Step::Conn(e.to_string()))?;
    }
    let resp = resp.await.map_err(|e| Step::Conn(e.to_string()))?;
    let status = resp.status();
    let bytes = BodyReader::new(resp.into_body())
        .read_to_limit(MAX_RESPONSE)
        .await
        .map_err(|e| Step::Conn(e.to_string()))?;
    Ok((status, bytes))
}

pub(crate) fn error_code(body: &[u8]) -> (ErrorCode, Option<u64>, String) {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(b) => (b.error.code, b.error.verified_offset, b.error.message),
        Err(_) => (ErrorCode::Internal, None, String::new()),
    }
}

/// `GET /v1/transfers/{id}`; `None` when the receiver answers `404 not_found` (it no
/// longer knows the transfer).
async fn get_state(
    conn: &Connection,
    id: &str,
) -> std::result::Result<Option<TransferStateView>, Step> {
    let (status, body) =
        request_json(conn, Method::GET, &format!("/v1/transfers/{id}"), None).await?;
    if status != StatusCode::OK {
        let (code, _, _) = error_code(&body);
        if status == StatusCode::NOT_FOUND && code == ErrorCode::NotFound {
            return Ok(None);
        }
        return Err(Step::Fatal(format!("receiver answered {status} {code}")));
    }
    serde_json::from_slice(&body)
        .map(Some)
        .map_err(|e| Step::Fatal(format!("bad state body: {e}")))
}

/// The receiver answered `404` for a transfer it had (it forgot it: a wiped folder, its
/// 24 h sweep, or a finished transfer it no longer reports). True when the send is
/// complete: the record shows every stream verified, so the receiver finished it and
/// re-offering would only make it receive everything again as "name (1)". Otherwise the
/// same manifest is offered again under the same ID on the next round, at most
/// [`MAX_REOFFERS`] times over the life of the record, and the plan starts from what the
/// receiver reports, which is 0 for a fresh offer.
async fn forgotten(job: &Job, offered: &mut bool) -> std::result::Result<bool, Step> {
    if job.all_streams_verified() {
        tracing::info!(peer = %short(&job.peer), "the receiver forgot a transfer it had verified in full; complete");
        return Ok(true);
    }
    let n = job.reoffers.fetch_add(1, Ordering::AcqRel) + 1;
    if let Some(rec) = lock(&job.record).as_mut() {
        rec.reoffers = n;
        // What the receiver verified before it forgot is gone with it: only what it
        // verifies after this offer may complete the send on a later `404`.
        rec.verified.clear();
        rec.bundle_verified = false;
    }
    job.persist().await;
    if n > MAX_REOFFERS {
        return Err(Step::Fatal(format!(
            "the receiver lost this transfer {n} times; giving up"
        )));
    }
    tracing::info!(peer = %short(&job.peer), "the receiver no longer knows this transfer; offering it again");
    // A decision from the first offer must not answer the new one.
    if let Some(o) = lock(&job.inner.sends).live.get(&job.id) {
        o.decision.send_replace(None);
    }
    *offered = false;
    Ok(false)
}

/// `POST /v1/transfers`: true once accepted (201 or 200), false while pending (202).
async fn post_offer(conn: &Connection, manifest: &Manifest) -> std::result::Result<bool, Step> {
    let body = Bytes::from(serde_json::to_vec(manifest).map_err(|e| Step::Fatal(e.to_string()))?);
    let (status, resp) = request_json(conn, Method::POST, "/v1/transfers", Some(body)).await?;
    match status {
        StatusCode::CREATED | StatusCode::OK => Ok(true),
        StatusCode::ACCEPTED => Ok(false),
        s => {
            let (code, _, msg) = error_code(&resp);
            Err(Step::Fatal(
                format!("offer refused: {s} {code} {msg}")
                    .trim_end()
                    .to_owned(),
            ))
        }
    }
}

async fn cancel_remote(conn: &Connection, id: &str, reason: &str) {
    delete_remote(conn, id, reason).await;
}

/// `DELETE /v1/transfers/{id}` (protocol.md 7.5); true once the receiver answered at all
/// (`204`, or `404` for a transfer it no longer has).
pub(super) async fn delete_remote(conn: &Connection, id: &str, reason: &str) -> bool {
    let body = Bytes::from(format!("{{\"reason\":\"{reason}\"}}"));
    request_json(
        conn,
        Method::DELETE,
        &format!("/v1/transfers/{id}"),
        Some(body),
    )
    .await
    .is_ok()
}

/// Waits for the receiver's decision on a pending offer (protocol.md 7.3, 5.13).
async fn wait_decision(job: &Job) -> std::result::Result<(), String> {
    let mut decision = job.decision.clone();
    let mut cancel = job.cancel.clone();
    let wait = async {
        decision
            .wait_for(Option::is_some)
            .await
            .map(|d| d.clone())
            .ok()
            .flatten()
    };
    tokio::select! {
        d = tokio::time::timeout(super::receive::OFFER_TTL, wait) => match d {
            Ok(Some(FrameState::Accepted)) => Ok(()),
            Ok(Some(other)) => Err(format!("offer {other}")),
            Ok(None) => Err("decision channel closed".into()),
            Err(_) => Err("offer expired".into()),
        },
        _ = cancel.wait_for(|c| *c) => Err("cancelled".into()),
    }
}

async fn run(job: Arc<Job>) -> std::result::Result<TransferSummary, RunError> {
    let started = Instant::now();
    let (id, wire) = (job.id.clone(), job.manifest.id.clone());
    let mut offered = job.known_offer;
    let mut reconnects = 0u32;
    let mut last_ok = Instant::now();
    let mut backoff = Duration::from_millis(100);
    let mut stall = Stall::default();
    loop {
        if *job.cancel.borrow() {
            let told = match job.inner.registry.outbound(&job.peer) {
                Some(c) => delete_remote(&c, &wire, "user").await,
                None => false,
            };
            if !told {
                // The receiver is away: it gets the cancel at its next session.
                send_resume::leave_tomb(&job.inner, &id, &wire, job.peer).await;
            }
            return Err(RunError::fatal("cancelled"));
        }
        let conn = match connect::outbound(&job.inner, job.peer).await {
            Ok(c) => c,
            Err(e) => {
                if last_ok.elapsed() > job.inner.reconnect_for {
                    tracing::info!(peer = %short(&job.peer), error = %e, "receiver unreachable; send gives up for now");
                    return Err(RunError::resumable(
                        FailureKind::Unreachable,
                        "receiver unreachable",
                    ));
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
                continue;
            }
        };
        last_ok = Instant::now();
        backoff = Duration::from_millis(100);
        let step = attempt(&job, &conn, &mut offered, &mut stall).await;
        if matches!(step, Ok(None) | Err(Step::Conn(_))) {
            // End of a connected round: what it changed is on disk before the next one.
            job.save(true).await;
        }
        match step {
            Ok(Some(done)) => {
                let roots = roots(&job).await;
                return Ok(TransferSummary {
                    id,
                    peer: job.peer.to_hex(),
                    bytes: job.manifest.total_bytes,
                    elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    roots: roots.0,
                    bundle_root: roots.1.or(done),
                    reconnects,
                });
            }
            Ok(None) => {}
            Err(Step::Conn(e)) => {
                if stall.lost_round() {
                    // The last error stays in the reason: it is the receiver's answer
                    // (a `500` from its disk, say), which the user needs to act on.
                    return Err(RunError::resumable(
                        FailureKind::Stalled,
                        format!("the receiver keeps dropping the upload without progress: {e}"),
                    ));
                }
                reconnects += 1;
                tracing::info!(peer = %short(&job.peer), error = %e, "transfer connection lost; resuming");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(Step::Fatal(e)) => {
                // A local cancel that landed while waiting for the decision or mid-stream
                // ends the round as a fatal step; tell the receiver on the connection we
                // still hold, or it keeps the offer pending (or the transfer active,
                // waiting for a resume) and counts it against the 4 per peer (SR-20)
                // until its 24 h sweep.
                if *job.cancel.borrow()
                    && !job.remote_cancelled.load(Ordering::Acquire)
                    && !delete_remote(&conn, &wire, "user").await
                {
                    send_resume::leave_tomb(&job.inner, &id, &wire, job.peer).await;
                }
                return Err(RunError::fatal(e));
            }
        }
    }
}

/// One connected round: offer or fetch state, plan, stream. `Ok(Some(_))` is complete;
/// `Ok(None)` means plan again on the same or a new connection.
#[allow(
    clippy::too_many_lines,
    reason = "one connected round, read top to bottom"
)]
async fn attempt(
    job: &Arc<Job>,
    conn: &Arc<Connection>,
    offered: &mut bool,
    stall: &mut Stall,
) -> std::result::Result<Option<Option<String>>, Step> {
    let id = job.manifest.id.clone();
    if !*offered {
        let accepted = post_offer(conn, &job.manifest).await?;
        *offered = true;
        if !accepted {
            wait_decision(job).await.map_err(Step::Fatal)?;
        }
        // The snapshot stays `offered` until the first `PUT` moves bytes (`mark_streaming`).
    }
    let Some(view) = get_state(conn, &id).await? else {
        return Ok(forgotten(job, offered).await?.then_some(None));
    };
    stall.observe(view.bytes_verified);
    let first = job.first_view.swap(false, Ordering::AcqRel);
    if first
        && job.origin == Origin::Adopted
        && matches!(
            view.state,
            TransferState::Cancelled
                | TransferState::Declined
                | TransferState::Failed
                | TransferState::Expired
        )
    {
        // The user sent these files again, but the receiver had ended the transfer they
        // continue: they go as a new one ([`continue_fresh`]).
        job.ended_before.store(true, Ordering::Release);
        return Err(Step::Fatal(
            "the receiver had ended the earlier transfer".into(),
        ));
    }
    job.note_view(&view);
    match view.state {
        TransferState::Complete => return Ok(Some(view.bundle.and_then(|b| b.root))),
        TransferState::Pending => {
            wait_decision(job).await.map_err(Step::Fatal)?;
            return Ok(None);
        }
        TransferState::Cancelled => {
            job.mark_remote_cancel();
            return Err(Step::Fatal(
                "receiver reports the transfer cancelled".into(),
            ));
        }
        TransferState::Declined | TransferState::Failed | TransferState::Expired => {
            return Err(Step::Fatal(format!(
                "receiver reports the transfer {}",
                format!("{:?}", view.state).to_lowercase()
            )))
        }
        TransferState::Accepted | TransferState::Active => {}
    }
    job.sync_done(&view);
    if prepare_sender(job, view.chunk_size).await? {
        // The receiver's answer fixed the chunk size: on disk now, so a resume after a
        // crash asks for the receiver's state first.
        job.persist().await;
    }
    let plan = plan_round(job, &view).await?;
    // The count restarts each round at what the receiver holds and keeps, so it never
    // passes the total after a stream restarted at 0 and never counts a re-sent byte
    // twice.
    job.sent.store(kept_bytes(&view, &plan), Ordering::Relaxed);
    if plan.must_cancel() {
        cancel_remote(conn, &id, "source_changed").await;
        return Err(Step::Fatal(
            "a source file changed size; cancelled (source_changed)".into(),
        ));
    }
    let token_of = |t: Target| -> Option<String> {
        match t {
            Target::File(fid) => view
                .files
                .iter()
                .find(|f| f.fid == fid)
                .and_then(|f| f.token.clone()),
            Target::Bundle => view.bundle.as_ref().and_then(|b| b.token.clone()),
        }
    };
    let mut work: Vec<(Target, u64, String)> = Vec::new();
    for (fid, action) in &plan.files {
        if let Some(from) = start_of(*action) {
            let token = token_of(Target::File(*fid))
                .ok_or_else(|| Step::Fatal(format!("no token for file {fid}")))?;
            work.push((Target::File(*fid), from, token));
        }
    }
    let bundle_work = match plan.bundle.and_then(start_of) {
        Some(from) => Some((
            Target::Bundle,
            from,
            token_of(Target::Bundle).ok_or_else(|| Step::Fatal("no bundle token".into()))?,
        )),
        None => None,
    };
    let files = stream::iter(work)
        .map(|(t, from, token)| stream_target(job, conn, t, token, from))
        .buffer_unordered(FILE_CONCURRENCY)
        .collect::<Vec<_>>();
    let bundle = async {
        match bundle_work {
            Some((t, from, token)) => vec![stream_target(job, conn, t, token, from).await],
            None => Vec::new(),
        }
    };
    let (mut results, bundle_results) = tokio::join!(files, bundle);
    results.extend(bundle_results);
    let mut failures = Vec::new();
    for r in results {
        match r {
            TargetOutcome::Done => {}
            TargetOutcome::Failed(e) => failures.push(e),
            TargetOutcome::Conn(e) => return Err(Step::Conn(e)),
            TargetOutcome::Fatal(e) => return Err(Step::Fatal(e)),
        }
    }
    if !failures.is_empty() {
        return Err(Step::Fatal(failures.join("; ")));
    }
    let Some(view) = get_state(conn, &id).await? else {
        return Ok(forgotten(job, offered).await?.then_some(None));
    };
    job.note_view(&view);
    match view.state {
        TransferState::Complete => Ok(Some(view.bundle.and_then(|b| b.root))),
        TransferState::Failed => Err(Step::Fatal("receiver reports the transfer failed".into())),
        _ => Ok(None),
    }
}

/// Makes the sender state on the first connected round, with the receiver's binding chunk
/// size, and fills in the hashes a record kept for that chunk size; true when it made one. A receiver that
/// answers another chunk size later (it forgot the transfer and took the re-offer with a
/// different size) gets a fresh sender with no hashes.
async fn prepare_sender(job: &Job, chunk_size: u32) -> std::result::Result<bool, Step> {
    let mut guard = job.sender.lock().await;
    if guard.as_ref().is_some_and(|s| s.chunk_size() == chunk_size) {
        return Ok(false);
    }
    let sources = match guard.take() {
        Some(old) => old.sources().to_vec(),
        None => lock(&job.sources).take().unwrap_or_default(),
    };
    let mut s = TransferSender::new(job.manifest.clone(), sources, chunk_size)
        .map_err(|e| Step::Fatal(e.to_string()))?;
    {
        let mut rec = lock(&job.record);
        if let Some(rec) = rec.as_mut() {
            // The loaded hashes move into the sender, so later record writes do not copy
            // them; the sender now says which hashes are valid.
            let hashes = std::mem::take(&mut rec.hashes);
            let bundle = std::mem::take(&mut rec.bundle_hashes);
            if rec.chunk_size == Some(chunk_size) {
                for (fid, h) in hashes {
                    s.record_hashes(fid, h);
                }
                s.record_bundle_hashes(bundle);
                // They are in the log already.
                s.mark_all_saved();
            } else {
                // Hashes for another chunk size are worthless: start the log afresh.
                job.rewrite_log.store(true, Ordering::Release);
            }
        }
    }
    *guard = Some(s);
    job.chunk_size.store(chunk_size, Ordering::Release);
    Ok(true)
}

/// Plans a connected round (protocol.md 7.6). Hashes the sender lost below the receiver's
/// offset are rebuilt from the source first, without holding the sender, so progress
/// readings and record writes go on; the send shows as running at what the receiver
/// holds while the source is read, and a cancel stops the read at once.
async fn plan_round(
    job: &Job,
    view: &TransferStateView,
) -> std::result::Result<crate::transfer::ResumePlan, Step> {
    let (plan, rebuilder) = {
        let guard = job.sender.lock().await;
        let s = guard
            .as_ref()
            .ok_or_else(|| Step::Fatal("sender missing".into()))?;
        let plan = s
            .plan_resume(view)
            .await
            .map_err(|e| Step::Fatal(e.to_string()))?;
        let rebuilder = (!plan.rebuild.is_empty()).then(|| s.rebuilder());
        (plan, rebuilder)
    };
    let Some(rebuilder) = rebuilder else {
        return Ok(plan);
    };
    job.sent.store(view.bytes_verified, Ordering::Relaxed);
    job.mark_streaming();
    let mut cancel = job.cancel.clone();
    for lost in &plan.rebuild {
        let rebuilt = tokio::select! {
            r = rebuilder.hashes(lost.stream, lost.offset) => r,
            _ = cancel.wait_for(|c| *c) => return Err(Step::Fatal("cancelled".into())),
        };
        match rebuilt {
            Ok(h) => {
                if let Some(s) = job.sender.lock().await.as_mut() {
                    s.install_hashes(lost.stream, h);
                }
                job.dirty.store(true, Ordering::Release);
            }
            Err(e) => {
                tracing::debug!(stream = ?lost.stream, error = %e, "lost chunk hashes could not be rebuilt");
            }
        }
    }
    let guard = job.sender.lock().await;
    let s = guard
        .as_ref()
        .ok_or_else(|| Step::Fatal("sender missing".into()))?;
    let mut plan = s
        .plan_resume(view)
        .await
        .map_err(|e| Step::Fatal(e.to_string()))?;
    plan.rebuild.clear();
    Ok(plan)
}

/// File bytes the receiver holds and keeps under `plan`: its verified count less what a
/// restart at 0 discards. The bundle's offset counts tar bytes, close enough for a
/// progress figure.
fn kept_bytes(view: &TransferStateView, plan: &crate::transfer::ResumePlan) -> u64 {
    let restarted = |a: ResumeAction| matches!(a, ResumeAction::Restart);
    let mut discarded: u64 = plan
        .files
        .iter()
        .filter(|(_, a)| restarted(*a))
        .filter_map(|(fid, _)| view.files.iter().find(|f| f.fid == *fid))
        .map(|f| f.verified_offset)
        .sum();
    if plan.bundle.is_some_and(restarted) {
        discarded += view.bundle.as_ref().map_or(0, |b| b.verified_offset);
    }
    view.bytes_verified.saturating_sub(discarded)
}

fn start_of(a: ResumeAction) -> Option<u64> {
    match a {
        ResumeAction::Done | ResumeAction::Cancel => None,
        ResumeAction::Resume { from } => Some(from),
        ResumeAction::Restart => Some(0),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    File(u32),
    Bundle,
}

impl Target {
    fn stream(self) -> StreamRef {
        match self {
            Self::File(fid) => StreamRef::File(fid),
            Self::Bundle => StreamRef::Bundle,
        }
    }
}

enum TargetOutcome {
    Done,
    Failed(String),
    Conn(String),
    Fatal(String),
}

enum PutResult {
    Ok(PutResponse),
    HashMismatch(u64),
    Refused(StatusCode, ErrorCode, String),
    Conn(String),
}

/// Streams one file or the bundle, re-sending from the receiver's verified offset after a
/// chunk hash mismatch, at most [`MAX_CHUNK_RETRIES`] times (SR-22).
async fn stream_target(
    job: &Arc<Job>,
    conn: &Arc<Connection>,
    target: Target,
    token: String,
    mut from: u64,
) -> TargetOutcome {
    let mut retries = 0u32;
    loop {
        if *job.cancel.borrow() {
            return TargetOutcome::Fatal("cancelled".into());
        }
        match put_once(job, conn, target, &token, from).await {
            PutResult::Ok(r) if r.state == FileState::Verified => {
                job.target_verified(target);
                return TargetOutcome::Done;
            }
            PutResult::Ok(r) => {
                return TargetOutcome::Failed(format!("{target:?} ended {:?}", r.state));
            }
            PutResult::HashMismatch(offset) => {
                retries += 1;
                if retries > MAX_CHUNK_RETRIES {
                    return TargetOutcome::Failed(format!("{target:?} failed integrity check"));
                }
                from = offset;
            }
            PutResult::Refused(_, ErrorCode::OffsetMismatch, _) => {
                // The receiver's offset moved (another stream or a restart): re-plan.
                return TargetOutcome::Conn("offset mismatch; re-planning".into());
            }
            PutResult::Refused(s, ErrorCode::Internal, msg) => {
                // A local failure on the receiver (its disk, while placing verified data).
                // The receiver keeps what it verified, so re-plan: the next round asks
                // for its state and resumes or retries the placement. The stall counter
                // ends the send if the failure does not clear.
                return TargetOutcome::Conn(format!(
                    "{target:?}: {s} {} {msg}; re-planning",
                    ErrorCode::Internal
                ));
            }
            PutResult::Refused(s, code, msg)
                if matches!(
                    code,
                    ErrorCode::RootMismatch | ErrorCode::BundleMismatch | ErrorCode::BadRequest
                ) =>
            {
                return TargetOutcome::Failed(format!("{target:?}: {s} {code} {msg}"));
            }
            PutResult::Refused(s, code, msg) => {
                if code == ErrorCode::Cancelled {
                    job.mark_remote_cancel();
                }
                return TargetOutcome::Fatal(format!("{target:?}: {s} {code} {msg}"));
            }
            PutResult::Conn(e) => return TargetOutcome::Conn(e),
        }
    }
}

async fn put_once(
    job: &Arc<Job>,
    conn: &Connection,
    target: Target,
    token: &str,
    from: u64,
) -> PutResult {
    let id = &job.manifest.id;
    let (path, size) = match target {
        Target::File(fid) => (
            format!("/v1/transfers/{id}/files/{fid}"),
            job.manifest.files.get(fid as usize).map_or(0, |f| f.size),
        ),
        Target::Bundle => (
            format!("/v1/transfers/{id}/bundle"),
            job.manifest.bundle.map_or(0, |b| b.size),
        ),
    };
    if size == 0 || from >= size {
        return PutResult::Refused(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            "empty range".into(),
        );
    }
    let chunk_size = job.chunk_size.load(Ordering::Acquire);
    let req = Request::builder()
        .method(Method::PUT)
        .uri(path)
        .header(http::header::CONTENT_TYPE, CHUNKS_CONTENT_TYPE)
        .header(
            http::header::CONTENT_RANGE,
            format!("bytes {from}-{}/{size}", size - 1),
        )
        .header(TOKEN_HEADER, token)
        .header(CHUNK_SIZE_HEADER, chunk_size.to_string())
        .body(());
    let Ok(req) = req else {
        return PutResult::Refused(
            StatusCode::BAD_REQUEST,
            ErrorCode::BadRequest,
            "bad request head".into(),
        );
    };
    match target {
        Target::File(fid) => {
            let enc = {
                let mut g = job.sender.lock().await;
                match g.as_mut() {
                    Some(s) => s.file_encoder(fid, from).await,
                    None => Err(Error::Protocol("sender missing".into())),
                }
            };
            let mut enc = match enc {
                Ok(e) => e,
                Err(e) => {
                    return PutResult::Refused(
                        StatusCode::BAD_REQUEST,
                        ErrorCode::BadRequest,
                        e.to_string(),
                    )
                }
            };
            let r = put_body(conn, req, &mut enc, size, job, target.stream()).await;
            if let Some(s) = job.sender.lock().await.as_mut() {
                s.record_hashes(fid, enc.into_hashes());
            }
            job.dirty.store(true, Ordering::Release);
            r
        }
        Target::Bundle => {
            let enc = {
                let mut g = job.sender.lock().await;
                match g.as_mut() {
                    Some(s) => s.bundle_encoder(from),
                    None => Err(Error::Protocol("sender missing".into())),
                }
            };
            let mut enc = match enc {
                Ok(e) => e,
                Err(e) => {
                    return PutResult::Refused(
                        StatusCode::BAD_REQUEST,
                        ErrorCode::BadRequest,
                        e.to_string(),
                    )
                }
            };
            let r = put_body(conn, req, &mut enc, size, job, target.stream()).await;
            if let Some(s) = job.sender.lock().await.as_mut() {
                s.record_bundle_hashes(enc.into_hashes());
            }
            job.dirty.store(true, Ordering::Release);
            r
        }
    }
}

/// Sends the chunk records and the `pd-root` trailer while reading the response, so a
/// `422` that arrives mid-body stops the upload (protocol.md 7.4). A local cancel resets
/// the stream at the next chunk, so a large file stops at once instead of at its end.
/// The first record handed to the connection turns the send `active`.
async fn put_body<R: AsyncRead + Unpin + Send>(
    conn: &Connection,
    req: Request<()>,
    enc: &mut ChunkEncoder<R>,
    size: u64,
    job: &Job,
    stream: StreamRef,
) -> PutResult {
    let (sent, cancel) = (&job.sent, &job.cancel);
    let (resp, mut upload) = match conn.request(req, false).await {
        Ok(v) => v,
        Err(e) => return PutResult::Conn(e.to_string()),
    };
    let body = async {
        let mut checkpoint = (0u64, Instant::now());
        while let Some(rec) = enc.next().await {
            if *cancel.borrow() {
                upload.send_reset(h2::Reason::CANCEL);
                return Err("cancelled".to_owned());
            }
            let rec = rec.map_err(|e| e.to_string())?;
            let n = rec.len().saturating_sub(HASH_LEN) as u64;
            send_all(&mut upload, rec, false)
                .await
                .map_err(|e| e.to_string())?;
            sent.fetch_add(n, Ordering::Relaxed);
            job.mark_streaming();
            checkpoint.0 += n;
            if checkpoint.0 >= CHECKPOINT_BYTES || checkpoint.1.elapsed() >= PERSIST_EVERY {
                job.checkpoint(stream, enc.hashes()).await;
                checkpoint = (0, Instant::now());
            }
        }
        let root = enc.root(size).ok_or("root unknown after the last chunk")?;
        let mut trailers = HeaderMap::new();
        let value = HeaderValue::from_str(&hash_to_hex(&root)).map_err(|e| e.to_string())?;
        trailers.insert(ROOT_TRAILER, value);
        upload.send_trailers(trailers).map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    };
    let (body_res, resp_res) = tokio::join!(body, resp);
    let resp = match resp_res {
        Ok(r) => r,
        Err(e) => {
            let why = body_res.err().unwrap_or_else(|| e.to_string());
            return PutResult::Conn(why);
        }
    };
    let status = resp.status();
    let bytes = match BodyReader::new(resp.into_body())
        .read_to_limit(MAX_RESPONSE)
        .await
    {
        Ok(b) => b,
        Err(e) => return PutResult::Conn(e.to_string()),
    };
    if status == StatusCode::OK {
        return match serde_json::from_slice::<PutResponse>(&bytes) {
            Ok(r) => PutResult::Ok(r),
            Err(e) => PutResult::Refused(status, ErrorCode::Internal, e.to_string()),
        };
    }
    let (code, offset, msg) = error_code(&bytes);
    if code == ErrorCode::ChunkHashMismatch {
        if let Some(o) = offset {
            return PutResult::HashMismatch(o);
        }
    }
    PutResult::Refused(status, code, msg)
}

/// Per-file roots and the bundle root, for the final report.
async fn roots(job: &Job) -> (Vec<FileRoot>, Option<String>) {
    let guard = job.sender.lock().await;
    let Some(s) = guard.as_ref() else {
        return (Vec::new(), None);
    };
    let files = job
        .manifest
        .files
        .iter()
        .map(|f| FileRoot {
            path: f.path.join("/"),
            size: f.size,
            root: (!f.in_bundle && f.size > 0)
                .then(|| s.root(f.fid).map(|r| hash_to_hex(&r)))
                .flatten(),
        })
        .collect();
    (files, s.bundle_root().map(|r| hash_to_hex(&r)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lost rounds without new verified bytes end the send; any progress starts the count
    /// again.
    #[test]
    fn stall_counts_only_rounds_without_progress() {
        let mut s = Stall::default();
        s.observe(0);
        for _ in 0..MAX_STALLED_ROUNDS {
            assert!(!s.lost_round());
        }
        s.observe(10);
        for _ in 0..MAX_STALLED_ROUNDS {
            s.observe(10);
            assert!(!s.lost_round());
        }
        s.observe(10);
        assert!(
            !s.lost_round(),
            "enough rounds, but not yet the minimum time without progress"
        );
        let later = Instant::now() + MIN_STALL_TIME;
        assert!(
            s.lost_round_at(later),
            "no progress for too many rounds and long enough"
        );
    }
}
