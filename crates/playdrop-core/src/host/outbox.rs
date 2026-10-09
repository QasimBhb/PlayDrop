//! The outbox (protocol.md 7.8): files queued for a peer that pulls them, such as a
//! backgrounded iPhone that cannot listen.
//!
//! Holder side: items live in memory and, when every source is a local path, in
//! `<data_dir>/outbox/<id>.json` so they survive a restart. Items with stream sources stay
//! in memory only: a `SourceOpener` is a foreign handle (an Android URI grant) that cannot
//! be re-opened after the process dies, so persisting it would promise files the host can
//! no longer read. `GET` serves only the items queued for the caller's Device ID (SR-13);
//! file and bundle bodies are the 7.4 chunk records with the `pd-root` trailer, from a
//! chunk-aligned `range` when `pd-prefix` matches the holder's recorded hashes (`206`),
//! else from 0 (`200`). Unpulled items expire after 7 days.
//!
//! Puller side: each item becomes an incoming transfer through the same path as an
//! auto-accepted offer (the puller's `files` grant decides), its streams are fetched over
//! the puller's own connection (11.1: only the dialler sends requests) into the receiver
//! with the verified offset and prefix as `range` and `pd-prefix`, and the item is deleted
//! on the holder once everything verified (SR-22).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{stream, StreamExt};
use h2::server::SendResponse;
use h2::RecvStream;
use http::{HeaderMap, HeaderValue, Method, Request, Response, StatusCode};
use serde::{Deserialize, Serialize};

use super::receive::{self, Target, CHUNK_SIZE_HEADER, ROOT_TRAILER};
use super::send::{self, request_json, SendSource, Step, FILE_CONCURRENCY, RECONNECT_FOR};
use super::{connect, lock, Inner};
use crate::identity::{secure_fs, DeviceId};
use crate::net::connection::short;
use crate::net::{
    respond_error, respond_json, send_all, BodyReader, Connection, HttpError, CHUNKS_CONTENT_TYPE,
};
use crate::session::types::{Frame, OutboxReady};
use crate::session::ErrorCode;
use crate::transfer::chunk::{prefix_hash, ChunkEncoder, ChunkHash};
use crate::transfer::manifest::{is_valid_transfer_id, MAX_MANIFEST_BODY};
use crate::transfer::sender::SourceStatus;
use crate::transfer::{
    hash_from_hex, hash_to_hex, FileId, FileState, Manifest, PutError, SourceInfo, SourceRef,
    TransferReceiver, TransferSender, TransferState,
};
use crate::{Error, Result};

/// Unpulled items expire after 7 days (protocol.md 7.8).
pub(crate) const OUTBOX_TTL: Duration = Duration::from_hours(7 * 24);

/// Header carrying the puller's prefix hash (protocol.md 7.8).
pub(crate) const PREFIX_HEADER: &str = "pd-prefix";

/// How often expired items are pruned.
const PRUNE_EVERY: Duration = Duration::from_hours(1);

/// How often a pull waiting for the user's decision looks again.
const DECISION_POLL: Duration = Duration::from_millis(250);

/// An item this host holds for a peer (`docs/core-api.md` section 4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxItem {
    /// Item ID (the manifest's transfer ID).
    pub id: String,
    /// The peer it is queued for, hex Device ID.
    pub peer: String,
    /// Number of files.
    pub file_count: usize,
    /// Total bytes.
    pub total_bytes: u64,
    /// Note shown to the puller.
    pub note: Option<String>,
    /// When it was queued, Unix ms.
    pub created_at_ms: u64,
}

/// One held item.
pub(crate) struct Held {
    peer: DeviceId,
    manifest: Arc<Manifest>,
    created_at_ms: u64,
    sender: tokio::sync::Mutex<TransferSender>,
}

impl Held {
    fn item(&self) -> OutboxItem {
        OutboxItem {
            id: self.manifest.id.clone(),
            peer: self.peer.to_hex(),
            file_count: self.manifest.files.len(),
            total_bytes: self.manifest.total_bytes,
            note: self.manifest.note.clone(),
            created_at_ms: self.created_at_ms,
        }
    }

    fn expired(&self, now_ms: u64) -> bool {
        now_ms.saturating_sub(self.created_at_ms) > ttl_ms()
    }
}

fn ttl_ms() -> u64 {
    u64::try_from(OUTBOX_TTL.as_millis()).unwrap_or(u64::MAX)
}

/// `GET /v1/outbox` entry (protocol.md 7.8).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListEntry {
    id: String,
    files: usize,
    total_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ListBody {
    items: Vec<ListEntry>,
}

/// `<data_dir>/outbox/<id>.json`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    v: u32,
    peer: DeviceId,
    created_at_ms: u64,
    manifest: Manifest,
    sources: Vec<PathRecord>,
}

/// A path source with the facts recorded at queue time (SR-21).
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PathRecord {
    path: PathBuf,
    size: u64,
    mtime_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ino: Option<u64>,
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn dir_of(inner: &Inner) -> PathBuf {
    inner.data_dir.join("outbox")
}

fn record_path(inner: &Inner, id: &str) -> PathBuf {
    dir_of(inner).join(format!("{id}.json"))
}

/// The persistable form of `sources`, or `None` when any is a stream.
fn path_records(sources: &[SourceInfo]) -> Option<Vec<PathRecord>> {
    sources
        .iter()
        .map(|s| {
            s.local_path().map(|p| PathRecord {
                path: p.to_path_buf(),
                size: s.size,
                mtime_ms: s.mtime_ms,
                dev: s.file_id.map(|f| f.dev),
                ino: s.file_id.map(|f| f.ino),
            })
        })
        .collect()
}

fn source_of(r: PathRecord) -> SourceInfo {
    SourceInfo {
        source: SourceRef::Path(r.path),
        size: r.size,
        mtime_ms: r.mtime_ms,
        file_id: match (r.dev, r.ino) {
            (Some(dev), Some(ino)) => Some(FileId { dev, ino }),
            _ => None,
        },
    }
}

/// Queues `sources` for `peer` and tells it if it is online (protocol.md 7.8, 5.11).
pub(crate) async fn queue(
    inner: &Arc<Inner>,
    peer: DeviceId,
    sources: Vec<SendSource>,
    note: Option<String>,
) -> Result<String> {
    if !inner.pins.contains(&peer) {
        return Err(Error::NotPaired);
    }
    let (manifest, infos) = send::build_manifest(sources, note).await?;
    let id = manifest.id.clone();
    let created_at_ms = unix_ms();
    if let Some(records) = path_records(&infos) {
        let rec = Record {
            v: 1,
            peer,
            created_at_ms,
            manifest: manifest.clone(),
            sources: records,
        };
        let dir = dir_of(inner);
        let path = record_path(inner, &id);
        tokio::task::spawn_blocking(move || {
            secure_fs::ensure_private_dir(&dir)?;
            secure_fs::write_private_atomic(&path, &serde_json::to_vec(&rec)?)
        })
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e)))??;
    }
    let sender = TransferSender::new(manifest.clone(), infos, manifest.chunk_size)?;
    lock(&inner.outbox).insert(
        id.clone(),
        Arc::new(Held {
            peer,
            manifest: Arc::new(manifest),
            created_at_ms,
            sender: tokio::sync::Mutex::new(sender),
        }),
    );
    tracing::debug!(peer = %short(&peer), "outbox item queued");
    notify(inner, peer);
    Ok(id)
}

/// Sends `outbox_ready` to `peer` if items wait for it and a session is up (5.11).
pub(crate) fn notify(inner: &Inner, peer: DeviceId) {
    let Some(s) = inner.registry.session(&peer) else {
        return;
    };
    let now = unix_ms();
    let (count, bytes, ids) = {
        let map = lock(&inner.outbox);
        let mine: Vec<&Arc<Held>> = map
            .values()
            .filter(|h| h.peer == peer && !h.expired(now))
            .collect();
        (
            mine.len() as u64,
            mine.iter().map(|h| h.manifest.total_bytes).sum::<u64>(),
            mine.iter()
                .map(|h| h.manifest.id.clone())
                .collect::<Vec<_>>(),
        )
    };
    if count > 0 {
        s.send(Frame::OutboxReady(OutboxReady { count, bytes, ids }));
    }
}

/// Items this host holds, oldest first.
pub(crate) fn list(inner: &Inner) -> Vec<OutboxItem> {
    let mut v: Vec<OutboxItem> = lock(&inner.outbox).values().map(|h| h.item()).collect();
    v.sort_by(|a, b| a.created_at_ms.cmp(&b.created_at_ms).then(a.id.cmp(&b.id)));
    v
}

/// Removes an item and its record. [`Error::NotFound`] if unknown.
pub(crate) async fn remove(inner: &Arc<Inner>, id: &str) -> Result<()> {
    if lock(&inner.outbox).remove(id).is_none() {
        return Err(Error::NotFound);
    }
    forget_record(inner, id).await;
    Ok(())
}

async fn forget_record(inner: &Inner, id: &str) {
    let path = record_path(inner, id);
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_file(path)).await;
}

/// Drops every item queued for an unpaired peer (protocol.md 6.4).
pub(crate) async fn drop_peer(inner: &Arc<Inner>, peer: &DeviceId) {
    let ids: Vec<String> = {
        let mut map = lock(&inner.outbox);
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, h)| h.peer == *peer)
            .map(|(k, _)| k.clone())
            .collect();
        for id in &ids {
            map.remove(id);
        }
        ids
    };
    for id in ids {
        forget_record(inner, &id).await;
    }
}

/// Reloads persisted items at start and drops expired or unreadable ones.
pub(crate) async fn load_persisted(inner: &Arc<Inner>) {
    let dir = dir_of(inner);
    let found = tokio::task::spawn_blocking(move || {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dir) else {
            return out;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            let rec = std::fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Record>(&b).ok());
            out.push((path, rec));
        }
        out
    })
    .await
    .unwrap_or_default();
    let now = unix_ms();
    for (path, rec) in found {
        let usable = rec.filter(|r| {
            r.v == 1
                && now.saturating_sub(r.created_at_ms) <= ttl_ms()
                && r.manifest.validate().is_ok()
                && r.sources.len() == r.manifest.files.len()
                && path.file_stem().and_then(|s| s.to_str()) == Some(r.manifest.id.as_str())
        });
        let Some(rec) = usable else {
            let _ = std::fs::remove_file(&path);
            continue;
        };
        let sources = rec.sources.into_iter().map(source_of).collect();
        let cs = rec.manifest.chunk_size;
        match TransferSender::new(rec.manifest.clone(), sources, cs) {
            Ok(sender) => {
                lock(&inner.outbox).insert(
                    rec.manifest.id.clone(),
                    Arc::new(Held {
                        peer: rec.peer,
                        manifest: Arc::new(rec.manifest),
                        created_at_ms: rec.created_at_ms,
                        sender: tokio::sync::Mutex::new(sender),
                    }),
                );
            }
            Err(e) => {
                tracing::debug!(error = %e, "outbox record dropped");
                let _ = std::fs::remove_file(&path);
            }
        }
    }
}

/// Prunes expired items every hour (protocol.md 7.8).
pub(crate) async fn maintenance(inner: Arc<Inner>) {
    loop {
        tokio::time::sleep(PRUNE_EVERY).await;
        let now = unix_ms();
        let gone: Vec<String> = {
            let mut map = lock(&inner.outbox);
            let ids: Vec<String> = map
                .iter()
                .filter(|(_, h)| h.expired(now))
                .map(|(k, _)| k.clone())
                .collect();
            for id in &ids {
                map.remove(id);
            }
            ids
        };
        for id in gone {
            forget_record(&inner, &id).await;
        }
    }
}

// ---------------------------------------------------------------------------------------
// Holder: the router's endpoints
// ---------------------------------------------------------------------------------------

/// The item `id` if it is queued for `peer` (SR-13: another device gets `404`).
fn lookup(inner: &Inner, id: &str, peer: &DeviceId) -> std::result::Result<Arc<Held>, HttpError> {
    let held = lock(&inner.outbox)
        .get(id)
        .filter(|h| h.peer == *peer)
        .cloned()
        .ok_or_else(|| HttpError::from_code(ErrorCode::NotFound, ""))?;
    if held.expired(unix_ms()) {
        return Err(HttpError::from_code(ErrorCode::Expired, ""));
    }
    Ok(held)
}

/// `GET /v1/outbox`: the items queued for the caller.
pub(crate) async fn serve_list(
    inner: &Inner,
    conn: &Connection,
    respond: &mut SendResponse<Bytes>,
) {
    let peer = conn.device_id();
    let now = unix_ms();
    let mut items: Vec<ListEntry> = lock(&inner.outbox)
        .values()
        .filter(|h| h.peer == peer && !h.expired(now))
        .map(|h| ListEntry {
            id: h.manifest.id.clone(),
            files: h.manifest.files.len(),
            total_bytes: h.manifest.total_bytes,
            note: h.manifest.note.clone(),
            created_at: h.created_at_ms,
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let _ = respond_json(respond, StatusCode::OK, &ListBody { items }).await;
}

/// `GET /v1/outbox/{id}`: the 7.2 manifest with the holder's chunk size.
pub(crate) async fn serve_manifest(
    inner: &Inner,
    conn: &Connection,
    respond: &mut SendResponse<Bytes>,
    id: &str,
) {
    match lookup(inner, id, &conn.device_id()) {
        Ok(h) => {
            let _ = respond_json(respond, StatusCode::OK, &*h.manifest).await;
        }
        Err(e) => {
            let _ = respond_error(respond, &e).await;
        }
    }
}

/// `DELETE /v1/outbox/{id}`: the puller confirms receipt (204).
pub(crate) async fn serve_delete(
    inner: &Arc<Inner>,
    conn: &Connection,
    respond: &mut SendResponse<Bytes>,
    id: &str,
) {
    let peer = conn.device_id();
    let removed = {
        let mut map = lock(&inner.outbox);
        match map.get(id) {
            Some(h) if h.peer == peer => map.remove(id).is_some(),
            _ => false,
        }
    };
    if !removed {
        let _ = respond_error(respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
        return;
    }
    forget_record(inner, id).await;
    let _ = crate::net::respond(respond, StatusCode::NO_CONTENT, None, Bytes::new()).await;
}

/// The parsed `range: bytes=<start>-[<end>]` header (protocol.md 7.8); `end` must be the
/// last byte when present.
pub(crate) fn parse_range(v: &str) -> Option<(u64, Option<u64>)> {
    let rest = v.trim().strip_prefix("bytes=")?;
    let (start, end) = rest.split_once('-')?;
    let start = start.trim().parse().ok()?;
    let end = match end.trim() {
        "" => None,
        e => Some(e.parse().ok()?),
    };
    Some((start, end))
}

/// Where a pull may start (protocol.md 7.8): `Ok(from)` with `from` above 0 only when the
/// range is chunk-aligned and `pd-prefix` equals the prefix of the holder's recorded
/// hashes; `Ok(0)` when it does not match or the hashes are lost; `Err` (`416
/// range_invalid`) for a misaligned range or one past the end.
pub(crate) fn pull_start(
    range: Option<(u64, Option<u64>)>,
    prefix: Option<&str>,
    size: u64,
    chunk_size: u32,
    recorded: &[ChunkHash],
) -> std::result::Result<u64, ()> {
    let Some((start, end)) = range else {
        return Ok(0);
    };
    if start >= size || !start.is_multiple_of(u64::from(chunk_size)) {
        return Err(());
    }
    if end.is_some_and(|e| e != size - 1) {
        return Err(());
    }
    if start == 0 {
        return Ok(0);
    }
    let Ok(k) = usize::try_from(start / u64::from(chunk_size)) else {
        return Ok(0);
    };
    let claimed = prefix.and_then(|p| hash_from_hex(p).ok());
    match claimed {
        Some(c) if recorded.len() >= k && prefix_hash(start, chunk_size, &recorded[..k]) == c => {
            Ok(start)
        }
        _ => Ok(0),
    }
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

/// `GET /v1/outbox/{id}/files/{fid}` and `/bundle` (protocol.md 7.8): 7.4 chunk records
/// and the `pd-root` trailer, `206` from a matching chunk-aligned range, else `200` from 0.
pub(crate) async fn serve_stream(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    id: String,
    target: Target,
) {
    let held = match lookup(&inner, &id, &conn.device_id()) {
        Ok(h) => h,
        Err(e) => {
            let _ = respond_error(&mut respond, &e).await;
            return;
        }
    };
    let m = &held.manifest;
    let cs = m.chunk_size;
    let size = match target {
        Target::File(fid) => m
            .files
            .get(fid as usize)
            .filter(|f| !f.in_bundle && f.size > 0)
            .map(|f| f.size),
        Target::Bundle => m.bundle.map(|b| b.size),
    };
    let Some(size) = size else {
        let _ = respond_error(&mut respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
        return;
    };
    let range = header(req.headers(), http::header::RANGE.as_str()).map(parse_range);
    if range == Some(None) {
        let e = HttpError::from_code(ErrorCode::RangeInvalid, "bad range header");
        let _ = respond_error(&mut respond, &e).await;
        return;
    }
    let range = range.flatten();
    let prefix = header(req.headers(), PREFIX_HEADER).map(str::to_owned);
    drop(req);
    let built = encoder_for(&held, target, range, prefix.as_deref(), size, cs).await;
    let (from, enc) = match built {
        Ok(v) => v,
        Err(e) => {
            let _ = respond_error(&mut respond, &e).await;
            return;
        }
    };
    let status = if from > 0 {
        StatusCode::PARTIAL_CONTENT
    } else {
        StatusCode::OK
    };
    let mut reply = Response::new(());
    *reply.status_mut() = status;
    let h = reply.headers_mut();
    h.insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static(CHUNKS_CONTENT_TYPE),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("bytes {from}-{}/{size}", size - 1)) {
        h.insert(http::header::CONTENT_RANGE, v);
    }
    h.insert(CHUNK_SIZE_HEADER, HeaderValue::from(cs));
    let hashes = match enc {
        Enc::File(fid, mut e) => {
            stream_body(&mut respond, reply, &mut e, size).await;
            (Some(fid), e.into_hashes())
        }
        Enc::Bundle(mut e) => {
            stream_body(&mut respond, reply, &mut e, size).await;
            (None, e.into_hashes())
        }
    };
    // Hashes of the chunks read are source facts even after a cut, so a later pull can
    // resume from them (7.6).
    let mut s = held.sender.lock().await;
    match hashes {
        (Some(fid), h) => s.record_hashes(fid, h),
        (None, h) => s.record_bundle_hashes(h),
    }
}

#[allow(
    clippy::large_enum_variant,
    reason = "one short-lived value per request"
)]
enum Enc {
    File(u32, ChunkEncoder<tokio::fs::File>),
    Bundle(ChunkEncoder<crate::transfer::BundleWriter>),
}

/// Picks the start (7.8) and opens the encoder. A source that changed since it was queued
/// cannot be served at all: `410 expired`.
async fn encoder_for(
    held: &Held,
    target: Target,
    range: Option<(u64, Option<u64>)>,
    prefix: Option<&str>,
    size: u64,
    cs: u32,
) -> std::result::Result<(u64, Enc), HttpError> {
    let changed = || HttpError::from_code(ErrorCode::Expired, "source changed since it was queued");
    let mut s = held.sender.lock().await;
    let recorded: Vec<ChunkHash> = match target {
        Target::File(fid) => s.hashes(fid).to_vec(),
        Target::Bundle => s.bundle_hashes().to_vec(),
    };
    let from = pull_start(range, prefix, size, cs, &recorded)
        .map_err(|()| HttpError::from_code(ErrorCode::RangeInvalid, ""))?;
    match target {
        Target::File(fid) => match s.file_encoder(fid, from).await {
            Ok(e) => Ok((from, Enc::File(fid, e))),
            Err(_) if from > 0 => {
                let e = s.file_encoder(fid, 0).await.map_err(|_| changed())?;
                Ok((0, Enc::File(fid, e)))
            }
            Err(_) => Err(changed()),
        },
        Target::Bundle => {
            let members: Vec<u32> = held
                .manifest
                .files
                .iter()
                .filter(|f| f.in_bundle)
                .map(|f| f.fid)
                .collect();
            for fid in members {
                if s.check_source(fid).await.map_err(|_| changed())? != SourceStatus::Unchanged {
                    return Err(changed());
                }
            }
            if let Ok(e) = s.bundle_encoder(from) {
                return Ok((from, Enc::Bundle(e)));
            }
            let e = s.bundle_encoder(0).map_err(|_| changed())?;
            Ok((0, Enc::Bundle(e)))
        }
    }
}

async fn stream_body<R: tokio::io::AsyncRead + Unpin + Send>(
    respond: &mut SendResponse<Bytes>,
    head: Response<()>,
    enc: &mut ChunkEncoder<R>,
    size: u64,
) {
    let mut out = match respond.send_response(head, false) {
        Ok(s) => s,
        Err(e) => {
            tracing::debug!(error = %e, "outbox response failed");
            return;
        }
    };
    while let Some(rec) = enc.next().await {
        let rec = match rec {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!(error = %e, "outbox source read failed");
                out.send_reset(h2::Reason::INTERNAL_ERROR);
                return;
            }
        };
        if send_all(&mut out, rec, false).await.is_err() {
            // The puller went away or reset the stream (7.4: it stops at a bad chunk).
            return;
        }
    }
    let Some(root) = enc.root(size) else {
        out.send_reset(h2::Reason::INTERNAL_ERROR);
        return;
    };
    let mut trailers = HeaderMap::new();
    if let Ok(v) = HeaderValue::from_str(&hash_to_hex(&root)) {
        trailers.insert(ROOT_TRAILER, v);
    }
    let _ = out.send_trailers(trailers);
}

// ---------------------------------------------------------------------------------------
// Puller
// ---------------------------------------------------------------------------------------

/// Pulls every item `peer` holds for us (protocol.md 7.8). Returns the transfer IDs being
/// pulled; progress and completion arrive as receive-side transfer events.
pub(crate) async fn pull(inner: &Arc<Inner>, peer: DeviceId) -> Result<Vec<String>> {
    if !inner.pins.contains(&peer) {
        return Err(Error::NotPaired);
    }
    let conn = connect::outbound(inner, peer).await?;
    let (status, body) = request_json(&conn, Method::GET, "/v1/outbox", None)
        .await
        .map_err(step_err)?;
    if status != StatusCode::OK {
        return Err(Error::Protocol(format!("outbox list answered {status}")));
    }
    let list: ListBody = serde_json::from_slice(&body)?;
    let mut started = Vec::new();
    for item in list.items {
        if !is_valid_transfer_id(&item.id) {
            continue;
        }
        match register(inner, &conn, peer, &item.id).await {
            Ok(()) => {
                let fresh = lock(&inner.pulling).insert(item.id.clone());
                if fresh {
                    let i2 = Arc::clone(inner);
                    let id = item.id.clone();
                    inner.spawn(async move {
                        pull_item(&i2, peer, &id).await;
                        lock(&i2.pulling).remove(&id);
                    });
                }
                started.push(item.id);
            }
            Err(e) => {
                tracing::debug!(peer = %short(&peer), error = %e, "outbox item not pulled");
            }
        }
    }
    Ok(started)
}

fn step_err(s: Step) -> Error {
    match s {
        Step::Conn(e) | Step::Fatal(e) => Error::Protocol(e),
    }
}

/// Fetches an item's manifest and registers it as an incoming offer from the holder.
async fn register(inner: &Arc<Inner>, conn: &Connection, peer: DeviceId, id: &str) -> Result<()> {
    let (status, body) = request_json(conn, Method::GET, &format!("/v1/outbox/{id}"), None)
        .await
        .map_err(step_err)?;
    if status != StatusCode::OK {
        return Err(Error::Protocol(format!("outbox item answered {status}")));
    }
    if body.len() > MAX_MANIFEST_BODY {
        return Err(Error::TooLarge("manifest over 16 MiB".into()));
    }
    let manifest: Manifest = serde_json::from_slice(&body)?;
    if manifest.id != id {
        return Err(Error::BadRequest(
            "manifest id differs from the item id".into(),
        ));
    }
    receive::register_pulled(inner, peer, &body)
        .await
        .map_err(|e| Error::Protocol(format!("{} {}", e.status, e.body.error.code)))
}

/// Where a pulled transfer stands.
enum PullState {
    /// Waiting for the user (the `files` grant is off).
    Waiting,
    /// Accepted: pull.
    Ready(TransferReceiver),
    /// Declined, expired, cancelled or gone: stop and release the item.
    Ended,
}

fn pull_state(inner: &Inner, id: &str) -> PullState {
    match receive::receiver_state(inner, id) {
        Some(Ok(rx)) => PullState::Ready(rx),
        Some(Err(true)) => PullState::Waiting,
        Some(Err(false)) | None => PullState::Ended,
    }
}

/// Pulls one item until it completes, fails, or the holder stays unreachable.
async fn pull_item(inner: &Arc<Inner>, peer: DeviceId, id: &str) {
    let mut last_ok = Instant::now();
    let mut backoff = Duration::from_millis(100);
    loop {
        let rx = match pull_state(inner, id) {
            PullState::Waiting => {
                tokio::time::sleep(DECISION_POLL).await;
                continue;
            }
            PullState::Ended => {
                // Declined or cancelled here: the holder need not offer it again.
                release(inner, peer, id).await;
                return;
            }
            PullState::Ready(rx) => rx,
        };
        match rx.transfer_state() {
            TransferState::Complete => {
                release(inner, peer, id).await;
                return;
            }
            TransferState::Failed | TransferState::Cancelled => return,
            _ => {}
        }
        let conn = match connect::outbound(inner, peer).await {
            Ok(c) => c,
            Err(e) => {
                if last_ok.elapsed() > RECONNECT_FOR {
                    fail(inner, id, &rx, format!("holder unreachable: {e}")).await;
                    return;
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(2));
                continue;
            }
        };
        last_ok = Instant::now();
        backoff = Duration::from_millis(100);
        match pull_round(inner, &conn, id, &rx).await {
            Ok(()) => {}
            Err(Step::Conn(e)) => {
                tracing::info!(peer = %short(&peer), error = %e, "outbox pull lost its connection; resuming");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(Step::Fatal(e)) => {
                fail(inner, id, &rx, e).await;
                return;
            }
        }
    }
}

/// Ends a pull that cannot go on.
async fn fail(inner: &Arc<Inner>, id: &str, rx: &TransferReceiver, why: String) {
    tracing::debug!(error = %why, "outbox pull failed");
    if !matches!(
        rx.transfer_state(),
        TransferState::Complete | TransferState::Failed | TransferState::Cancelled
    ) {
        receive::fail_local(inner, id, why).await;
    }
}

/// `DELETE /v1/outbox/{id}` on the holder, best effort.
async fn release(inner: &Arc<Inner>, peer: DeviceId, id: &str) {
    if let Ok(conn) = connect::outbound(inner, peer).await {
        let _ = request_json(&conn, Method::DELETE, &format!("/v1/outbox/{id}"), None).await;
    }
}

/// One connected round: every unfinished stream, up to 4 files plus the bundle at once.
async fn pull_round(
    inner: &Arc<Inner>,
    conn: &Arc<Connection>,
    id: &str,
    rx: &TransferReceiver,
) -> std::result::Result<(), Step> {
    let st = rx.state();
    let open = |s: FileState| matches!(s, FileState::Waiting | FileState::Receiving);
    let mut work: Vec<(Target, String)> = Vec::new();
    for f in &st.files {
        if open(f.state) {
            if let Some(t) = &f.token {
                work.push((Target::File(f.fid), t.clone()));
            }
        }
    }
    let bundle = st
        .bundle
        .as_ref()
        .filter(|b| open(b.state))
        .and_then(|b| b.token.clone());
    let files = stream::iter(work)
        .map(|(t, tok)| pull_stream(inner, conn, id, rx, t, tok))
        .buffer_unordered(FILE_CONCURRENCY)
        .collect::<Vec<_>>();
    let bundle = async {
        match bundle {
            Some(tok) => vec![pull_stream(inner, conn, id, rx, Target::Bundle, tok).await],
            None => Vec::new(),
        }
    };
    let (mut results, b) = tokio::join!(files, bundle);
    results.extend(b);
    for r in results {
        r?;
    }
    Ok(())
}

/// Pulls one stream until it verifies or fails, resuming from the receiver's verified
/// offset with its prefix (7.8). A chunk hash mismatch re-asks from the bad chunk; the
/// receiver fails the stream after 3 re-sends (SR-22).
async fn pull_stream(
    inner: &Arc<Inner>,
    conn: &Connection,
    id: &str,
    rx: &TransferReceiver,
    target: Target,
    token: String,
) -> std::result::Result<(), Step> {
    loop {
        let st = rx.state();
        let (state, offset, prefix, size) = match target {
            Target::File(fid) => {
                let Some(f) = st.files.iter().find(|f| f.fid == fid) else {
                    return Err(Step::Fatal(format!("no file {fid}")));
                };
                (f.state, f.verified_offset, f.prefix.clone(), f.size)
            }
            Target::Bundle => {
                let Some(b) = &st.bundle else {
                    return Err(Step::Fatal("no bundle".into()));
                };
                (b.state, b.verified_offset, b.prefix.clone(), b.size)
            }
        };
        if !matches!(state, FileState::Waiting | FileState::Receiving) {
            return Ok(());
        }
        let path = match target {
            Target::File(fid) => format!("/v1/outbox/{id}/files/{fid}"),
            Target::Bundle => format!("/v1/outbox/{id}/bundle"),
        };
        let mut b = Request::builder().method(Method::GET).uri(path);
        if offset > 0 && offset < size {
            b = b.header(http::header::RANGE, format!("bytes={offset}-"));
            if let Some(p) = &prefix {
                b = b.header(PREFIX_HEADER, p.as_str());
            }
        }
        let req = b.body(()).map_err(|e| Step::Fatal(e.to_string()))?;
        let (resp, _send) = conn
            .request(req, true)
            .await
            .map_err(|e| Step::Conn(e.to_string()))?;
        let resp = resp.await.map_err(|e| Step::Conn(e.to_string()))?;
        let start = match resp.status() {
            StatusCode::OK => 0,
            StatusCode::PARTIAL_CONTENT => offset,
            s => {
                let bytes = BodyReader::new(resp.into_body())
                    .read_to_limit(64 * 1024)
                    .await
                    .unwrap_or_default();
                let (code, _, msg) = send::error_code(&bytes);
                return Err(Step::Fatal(format!("holder answered {s} {code} {msg}")));
            }
        };
        let begun = match target {
            Target::File(fid) => rx.begin_file(fid, &token, start, size - 1).await,
            Target::Bundle => rx.begin_bundle(&token, start, size - 1).await,
        };
        let mut upload = match begun {
            Ok(u) => u,
            Err(PutError::Core(Error::OffsetMismatch { .. })) => continue,
            Err(e) => return Err(Step::Fatal(e.to_string())),
        };
        let mut body = BodyReader::new(resp.into_body());
        let received = upload.receive(&mut body).await;
        let outcome = match received {
            Ok(()) => {
                let root = match body.trailers().await {
                    Ok(Some(t)) => header(&t, ROOT_TRAILER).and_then(|s| hash_from_hex(s).ok()),
                    _ => None,
                };
                upload.finish(root).await.map(drop)
            }
            Err(e) => {
                drop(upload);
                Err(e)
            }
        };
        drop(body);
        receive::after_put(inner, id, rx, target);
        match outcome {
            // Verified or partial: look at the state again. A bad chunk: ask again from
            // the verified offset; the receiver fails the stream after 3 re-sends.
            Ok(()) | Err(PutError::Core(Error::ChunkHashMismatch { .. })) => {}
            Err(PutError::Core(Error::Io(e))) => return Err(Step::Conn(e.to_string())),
            Err(PutError::Cancelled) => return Err(Step::Fatal("cancelled".into())),
            Err(e) => return Err(Step::Fatal(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CS: u32 = 64 * 1024;

    #[test]
    fn range_header_forms() {
        assert_eq!(parse_range("bytes=309329920-"), Some((309_329_920, None)));
        assert_eq!(parse_range("bytes=0-99"), Some((0, Some(99))));
        assert_eq!(parse_range("bytes=-5"), None);
        assert_eq!(parse_range("items=0-"), None);
    }

    /// protocol.md 7.8: a chunk-aligned range with the matching prefix resumes there; a
    /// wrong or missing prefix, or lost hashes, restart at 0; a misaligned range or one
    /// past the end is `416`.
    #[test]
    fn outbox_range_and_prefix() {
        let hashes: Vec<ChunkHash> = (0u8..4).map(|i| *blake3::hash(&[i]).as_bytes()).collect();
        let size = 3 * u64::from(CS) + 10;
        let off = 2 * u64::from(CS);
        let good = hash_to_hex(&prefix_hash(off, CS, &hashes[..2]));
        let bad = hash_to_hex(&[7; 32]);
        assert_eq!(pull_start(None, None, size, CS, &hashes), Ok(0));
        assert_eq!(
            pull_start(Some((off, None)), Some(&good), size, CS, &hashes),
            Ok(off)
        );
        assert_eq!(
            pull_start(Some((off, Some(size - 1))), Some(&good), size, CS, &hashes),
            Ok(off)
        );
        assert_eq!(
            pull_start(Some((off, None)), Some(&bad), size, CS, &hashes),
            Ok(0)
        );
        assert_eq!(
            pull_start(Some((off, None)), None, size, CS, &hashes),
            Ok(0)
        );
        assert_eq!(
            pull_start(Some((off, None)), Some(&good), size, CS, &hashes[..1]),
            Ok(0)
        );
        assert_eq!(
            pull_start(Some((off + 1, None)), Some(&good), size, CS, &hashes),
            Err(())
        );
        assert_eq!(
            pull_start(Some((size, None)), None, size, CS, &hashes),
            Err(())
        );
        assert_eq!(
            pull_start(Some((off, Some(5))), Some(&good), size, CS, &hashes),
            Err(())
        );
        assert_eq!(pull_start(Some((0, None)), None, size, CS, &hashes), Ok(0));
    }

    #[test]
    fn list_body_matches_the_spec_example() {
        let json = r#"{"items":[{"id":"c1d2","files":2,"totalBytes":684388316,"note":"Holiday","createdAt":1791460990000}]}"#;
        let body: ListBody = serde_json::from_str(json).unwrap();
        assert_eq!(body.items[0].files, 2);
        assert_eq!(serde_json::to_string(&body).unwrap(), json);
    }
}
