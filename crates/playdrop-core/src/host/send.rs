//! Send side of the transfer plane over HTTP/2 (protocol.md 7.2 to 7.7): build the
//! manifest, offer it, stream up to 4 files plus the bundle concurrently on the one
//! connection, send the `pd-root` trailer, retry failed chunks (SR-22), and resume after any
//! disconnect from the receiver's verified offsets (7.6, SR-21).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use serde::Serialize;
use tokio::io::AsyncRead;
use tokio::sync::watch;

use super::events::{FileRoot, HostEvent, TransferDirection};
use super::receive::{CHUNK_SIZE_HEADER, ROOT_TRAILER, TOKEN_HEADER};
use super::{connect, lock, Inner};
use crate::constants::DEFAULT_CHUNK_SIZE;
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::{
    send_all, BodyReader, Connection, ErrorBody, CHUNKS_CONTENT_TYPE, JSON_CONTENT_TYPE,
};
use crate::session::types::{TransferFrame, TransferState as FrameState};
use crate::session::ErrorCode;
use crate::transfer::chunk::HASH_LEN;
use crate::transfer::receiver::MAX_CHUNK_RETRIES;
use crate::transfer::{
    hash_to_hex, ChunkEncoder, FileState, Manifest, ManifestBuilder, PutResponse, ResumeAction,
    SourceInfo, TransferSender, TransferState, TransferStateView,
};
use crate::{Error, Result};

/// Concurrent file `PUT`s besides the bundle (protocol.md section 10).
pub const FILE_CONCURRENCY: usize = 4;

/// How long a sender keeps trying to reach the receiver after a drop. *Choice:* ten
/// minutes covers a Wi-Fi switch or a restarted daemon; the user can cancel sooner.
const RECONNECT_FOR: Duration = Duration::from_secs(600);

/// Progress event period.
const PROGRESS_EVERY: Duration = Duration::from_millis(500);

/// Response bodies other than the session are small JSON.
const MAX_RESPONSE: usize = 16 * 1024 * 1024;

// TODO(SR-21, protocol.md 7.6): persist the sender's transfer ID, manifest, sources and
// chunk hashes so a send survives a restart of the sending host; today only the receiver
// side persists, so a receiver restart resumes but a sender restart starts a new offer.

/// Bookkeeping for a send, so `transfer` frames reach it (protocol.md 5.13).
pub(crate) struct Outgoing {
    peer: DeviceId,
    decision: watch::Sender<Option<FrameState>>,
    cancel: Arc<watch::Sender<bool>>,
}

/// What a finished send reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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

    /// File bytes handed to the connection so far (re-sent bytes count again).
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
    peer: DeviceId,
    manifest: Manifest,
    sources: std::sync::Mutex<Option<Vec<SourceInfo>>>,
    sender: tokio::sync::Mutex<Option<TransferSender>>,
    sent: Arc<AtomicU64>,
    chunk_size: AtomicU32,
    cancel: watch::Receiver<bool>,
    decision: watch::Receiver<Option<FrameState>>,
}

/// Builds the manifest for `paths` and starts sending to `peer`.
pub(crate) async fn start(
    inner: &Arc<Inner>,
    peer: DeviceId,
    paths: Vec<PathBuf>,
) -> Result<TransferHandle> {
    if !inner.pins.contains(&peer) {
        return Err(Error::NotPaired);
    }
    let mut b = ManifestBuilder::new(DEFAULT_CHUNK_SIZE);
    for p in &paths {
        b.add_path(p).await?;
    }
    let (manifest, sources) = b.build()?;
    let id = manifest.id.clone();
    let total = manifest.total_bytes;
    let (decision_tx, decision_rx) = watch::channel(None);
    let cancel = Arc::new(watch::Sender::new(false));
    lock(&inner.outgoing).insert(
        id.clone(),
        Outgoing {
            peer,
            decision: decision_tx,
            cancel: Arc::clone(&cancel),
        },
    );
    let sent = Arc::new(AtomicU64::new(0));
    let job = Arc::new(Job {
        inner: Arc::clone(inner),
        peer,
        manifest,
        sources: std::sync::Mutex::new(Some(sources)),
        sender: tokio::sync::Mutex::new(None),
        sent: Arc::clone(&sent),
        chunk_size: AtomicU32::new(DEFAULT_CHUNK_SIZE),
        cancel: cancel.subscribe(),
        decision: decision_rx,
    });
    let (done_tx, done_rx) = watch::channel(None);
    let i2 = Arc::clone(inner);
    let id2 = id.clone();
    inner.spawn(async move {
        let ticker = progress_ticker(Arc::clone(&job));
        let outcome = tokio::select! {
            o = run(Arc::clone(&job)) => o,
            () = ticker => Err("progress ticker ended".into()),
        };
        lock(&i2.outgoing).remove(&id2);
        match &outcome {
            Ok(s) => i2.emit(HostEvent::TransferComplete {
                id: s.id.clone(),
                peer: s.peer.clone(),
                direction: TransferDirection::Send,
                bytes: s.bytes,
                elapsed_ms: s.elapsed_ms,
                roots: s.roots.clone(),
                bundle_root: s.bundle_root.clone(),
            }),
            Err(reason) => i2.emit(HostEvent::TransferFailed {
                id: id2.clone(),
                peer: peer.to_hex(),
                direction: TransferDirection::Send,
                reason: reason.clone(),
            }),
        }
        done_tx.send_replace(Some(outcome));
    });
    Ok(TransferHandle {
        id,
        total,
        sent,
        done: done_rx,
        cancel,
    })
}

async fn progress_ticker(job: Arc<Job>) {
    let mut last = (Instant::now(), 0u64);
    loop {
        tokio::time::sleep(PROGRESS_EVERY).await;
        let bytes = job.sent.load(Ordering::Relaxed);
        let now = Instant::now();
        let dt = now.duration_since(last.0).as_secs_f64().max(0.001);
        #[allow(
            clippy::cast_precision_loss,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss
        )] // display rate only
        let rate = (bytes.saturating_sub(last.1) as f64 / dt) as u64;
        last = (now, bytes);
        job.inner.emit(HostEvent::TransferProgress {
            id: job.manifest.id.clone(),
            peer: job.peer.to_hex(),
            direction: TransferDirection::Send,
            bytes: bytes.min(job.manifest.total_bytes),
            total: job.manifest.total_bytes,
            bytes_per_sec: rate,
        });
    }
}

/// Routes a `transfer` frame to the send it names, if the frame came from its receiver
/// (protocol.md 5.13, SR-13).
pub(crate) fn on_transfer_frame(inner: &Inner, peer: DeviceId, tf: &TransferFrame) {
    let map = lock(&inner.outgoing);
    if let Some(o) = map.get(&tf.id).filter(|o| o.peer == peer) {
        o.decision.send_replace(Some(tf.state.clone()));
    }
}

/// Cancels every send to an unpaired peer (protocol.md 6.4).
pub(crate) fn drop_peer(inner: &Inner, peer: &DeviceId) {
    for o in lock(&inner.outgoing).values().filter(|o| o.peer == *peer) {
        o.cancel.send_replace(true);
    }
}

/// One step's failure.
enum Step {
    /// The connection is gone or unusable: reconnect and resume.
    Conn(String),
    /// The receiver refused: stop.
    Fatal(String),
}

async fn request_json(
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

fn error_code(body: &[u8]) -> (ErrorCode, Option<u64>, String) {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(b) => (b.error.code, b.error.verified_offset, b.error.message),
        Err(_) => (ErrorCode::Internal, None, String::new()),
    }
}

async fn get_state(conn: &Connection, id: &str) -> std::result::Result<TransferStateView, Step> {
    let (status, body) =
        request_json(conn, Method::GET, &format!("/v1/transfers/{id}"), None).await?;
    if status != StatusCode::OK {
        let (code, _, _) = error_code(&body);
        return Err(Step::Fatal(format!("receiver answered {status} {code}")));
    }
    serde_json::from_slice(&body).map_err(|e| Step::Fatal(format!("bad state body: {e}")))
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
    let body = Bytes::from(format!("{{\"reason\":\"{reason}\"}}"));
    let _ = request_json(
        conn,
        Method::DELETE,
        &format!("/v1/transfers/{id}"),
        Some(body),
    )
    .await;
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

async fn run(job: Arc<Job>) -> Outcome {
    let started = Instant::now();
    let id = job.manifest.id.clone();
    let mut offered = false;
    let mut reconnects = 0u32;
    let mut last_ok = Instant::now();
    let mut backoff = Duration::from_millis(100);
    loop {
        if *job.cancel.borrow() {
            if let Some(c) = job.inner.registry.outbound(&job.peer) {
                cancel_remote(&c, &id, "user").await;
            }
            return Err("cancelled".into());
        }
        let conn = match connect::outbound(&job.inner, job.peer).await {
            Ok(c) => c,
            Err(e) => {
                if last_ok.elapsed() > RECONNECT_FOR {
                    return Err(format!("receiver unreachable: {e}"));
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
                continue;
            }
        };
        last_ok = Instant::now();
        backoff = Duration::from_millis(100);
        let step = attempt(&job, &conn, &mut offered).await;
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
                reconnects += 1;
                tracing::info!(peer = %short(&job.peer), error = %e, "transfer connection lost; resuming");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(Step::Fatal(e)) => return Err(e),
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
) -> std::result::Result<Option<Option<String>>, Step> {
    let id = job.manifest.id.clone();
    if !*offered {
        let accepted = post_offer(conn, &job.manifest).await?;
        *offered = true;
        if !accepted {
            wait_decision(job).await.map_err(Step::Fatal)?;
        }
    }
    let view = get_state(conn, &id).await?;
    match view.state {
        TransferState::Complete => return Ok(Some(view.bundle.and_then(|b| b.root))),
        TransferState::Pending => {
            wait_decision(job).await.map_err(Step::Fatal)?;
            return Ok(None);
        }
        TransferState::Declined
        | TransferState::Cancelled
        | TransferState::Failed
        | TransferState::Expired => {
            return Err(Step::Fatal(format!(
                "receiver reports the transfer {}",
                format!("{:?}", view.state).to_lowercase()
            )))
        }
        TransferState::Accepted | TransferState::Active => {}
    }
    {
        let mut guard = job.sender.lock().await;
        if guard.is_none() {
            let sources = lock(&job.sources).take().unwrap_or_default();
            let s = TransferSender::new(job.manifest.clone(), sources, view.chunk_size)
                .map_err(|e| Step::Fatal(e.to_string()))?;
            *guard = Some(s);
            job.chunk_size.store(view.chunk_size, Ordering::Release);
        }
    }
    let plan = {
        let guard = job.sender.lock().await;
        let s = guard
            .as_ref()
            .ok_or_else(|| Step::Fatal("sender missing".into()))?;
        s.plan_resume(&view)
            .await
            .map_err(|e| Step::Fatal(e.to_string()))?
    };
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
    let view = get_state(conn, &id).await?;
    match view.state {
        TransferState::Complete => Ok(Some(view.bundle.and_then(|b| b.root))),
        TransferState::Failed => Err(Step::Fatal("receiver reports the transfer failed".into())),
        _ => Ok(None),
    }
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
            PutResult::Ok(r) if r.state == FileState::Verified => return TargetOutcome::Done,
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
            PutResult::Refused(s, code, msg)
                if matches!(
                    code,
                    ErrorCode::RootMismatch | ErrorCode::BundleMismatch | ErrorCode::BadRequest
                ) =>
            {
                return TargetOutcome::Failed(format!("{target:?}: {s} {code} {msg}"));
            }
            PutResult::Refused(s, code, msg) => {
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
            let r = put_body(conn, req, &mut enc, size, &job.sent).await;
            if let Some(s) = job.sender.lock().await.as_mut() {
                s.record_hashes(fid, enc.into_hashes());
            }
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
            let r = put_body(conn, req, &mut enc, size, &job.sent).await;
            if let Some(s) = job.sender.lock().await.as_mut() {
                s.record_bundle_hashes(enc.into_hashes());
            }
            r
        }
    }
}

/// Sends the chunk records and the `pd-root` trailer while reading the response, so a
/// `422` that arrives mid-body stops the upload (protocol.md 7.4).
async fn put_body<R: AsyncRead + Unpin + Send>(
    conn: &Connection,
    req: Request<()>,
    enc: &mut ChunkEncoder<R>,
    size: u64,
    sent: &AtomicU64,
) -> PutResult {
    let (resp, mut upload) = match conn.request(req, false).await {
        Ok(v) => v,
        Err(e) => return PutResult::Conn(e.to_string()),
    };
    let body = async {
        while let Some(rec) = enc.next().await {
            let rec = rec.map_err(|e| e.to_string())?;
            let n = rec.len().saturating_sub(HASH_LEN) as u64;
            send_all(&mut upload, rec, false)
                .await
                .map_err(|e| e.to_string())?;
            sent.fetch_add(n, Ordering::Relaxed);
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
