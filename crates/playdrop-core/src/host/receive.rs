//! Receive side of the transfer plane over HTTP/2 (protocol.md 7.1 to 7.7): offers, the
//! `files` grant (6.4), file and bundle `PUT`s onto [`TransferReceiver`], state, cancel,
//! offer expiry, and reload after a restart (7.6). SR-13: every transfer ID is bound to
//! the Device ID that offered it; any other device gets `404`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use h2::server::SendResponse;
use h2::RecvStream;
use http::{HeaderMap, Request, StatusCode};
use serde::{Deserialize, Serialize};

use super::events::{FileRoot, HostEvent, OfferedFile, TransferDirection, OFFER_FILES_MAX};
use super::transfers::{TransferSnapshot, TransferStatus};
use super::{lock, Inner, TransferEnd};
use crate::identity::{secure_fs, DeviceId};
use crate::net::connection::short;
use crate::net::{
    is_remote_reset, respond_error, respond_json, BodyReader, Connection, HttpError, ReadLimitError,
};
use crate::session::types::{Frame, TransferFrame, TransferState as FrameState};
use crate::session::ErrorCode;
use crate::transfer::manifest::MAX_MANIFEST_BODY;
use crate::transfer::receiver::{cleanup_idle, FileStateView, StreamStateView, IDLE_TIMEOUT};
use crate::transfer::{
    hash_from_hex, AcceptOptions, FileState, Manifest, PutError, SyncPolicy, TransferReceiver,
    TransferState, TransferStateView,
};
use crate::{Error, Result};

/// A pending offer expires after 10 minutes (protocol.md 7.3).
pub(crate) const OFFER_TTL: Duration = Duration::from_mins(10);

/// At most 4 concurrent transfers per peer (protocol.md 7.2, SR-20).
pub(crate) const MAX_PER_PEER: usize = 4;

/// Header carrying the per-file token (protocol.md 7.3).
pub(crate) const TOKEN_HEADER: &str = "pd-token";
/// Header carrying the binding chunk size (protocol.md 7.4).
pub(crate) const CHUNK_SIZE_HEADER: &str = "pd-chunk-size";
/// Trailer carrying the root hash (protocol.md 7.4).
pub(crate) const ROOT_TRAILER: &str = "pd-root";

/// How often idle `.part` cleanup runs (SR-20).
const CLEANUP_EVERY: Duration = Duration::from_secs(3600);

/// Which stream a `PUT` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    /// One streamed file.
    File(u32),
    /// The tar bundle (7.7).
    Bundle,
}

pub(crate) enum OfferState {
    Pending {
        expires_at_ms: u64,
        deadline: Instant,
    },
    Accepting,
    Accepted(TransferReceiver),
    Declined,
    Expired,
    Cancelled,
}

/// One incoming transfer.
pub(crate) struct Incoming {
    owner: DeviceId,
    manifest: Arc<Manifest>,
    state: OfferState,
    started: Instant,
    reported: bool,
    last: (Instant, u64),
}

impl Incoming {
    fn new(owner: DeviceId, manifest: Arc<Manifest>, state: OfferState) -> Self {
        Self {
            owner,
            manifest,
            state,
            started: Instant::now(),
            reported: false,
            last: (Instant::now(), 0),
        }
    }

    fn is_active(&self) -> bool {
        match &self.state {
            OfferState::Pending { .. } | OfferState::Accepting => true,
            OfferState::Accepted(rx) => matches!(
                rx.transfer_state(),
                TransferState::Accepted | TransferState::Active | TransferState::Pending
            ),
            _ => false,
        }
    }
}

/// The 201 body (protocol.md 7.3).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Accepted201 {
    id: String,
    state: &'static str,
    chunk_size: u32,
    files: Vec<TokenEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle: Option<BundleToken>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TokenEntry {
    fid: u32,
    token: String,
    verified_offset: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BundleToken {
    token: String,
    verified_offset: u64,
}

/// The 202 body (protocol.md 7.3).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Pending202 {
    id: String,
    state: &'static str,
    expires_at: u64,
}

#[derive(Serialize, Deserialize)]
struct OwnerRecord {
    owner: DeviceId,
}

fn index_path(inner: &Inner, id: &str) -> std::path::PathBuf {
    inner.data_dir.join("incoming").join(format!("{id}.json"))
}

/// Persists which device owns a transfer, so SR-13 binding survives a restart (7.6).
async fn write_owner(inner: &Inner, id: &str, owner: DeviceId) -> Result<()> {
    let dir = inner.data_dir.join("incoming");
    let path = index_path(inner, id);
    tokio::task::spawn_blocking(move || {
        secure_fs::ensure_private_dir(&dir)?;
        let json = serde_json::to_vec(&OwnerRecord { owner })?;
        secure_fs::write_private_atomic(&path, &json)
    })
    .await
    .map_err(|e| Error::Io(std::io::Error::other(e)))?
}

fn remove_owner(inner: &Inner, id: &str) {
    let _ = std::fs::remove_file(index_path(inner, id));
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

fn pending_view(manifest: &Manifest, state: TransferState) -> TransferStateView {
    TransferStateView {
        id: manifest.id.clone(),
        state,
        chunk_size: manifest.chunk_size,
        total_bytes: manifest.total_bytes,
        bytes_verified: 0,
        files: manifest
            .files
            .iter()
            .map(|f| FileStateView {
                fid: f.fid,
                state: FileState::Waiting,
                size: f.size,
                verified_offset: 0,
                root: None,
                prefix: None,
                token: None,
            })
            .collect(),
        bundle: manifest.bundle.map(|b| StreamStateView {
            state: FileState::Waiting,
            size: b.size,
            verified_offset: 0,
            root: None,
            prefix: None,
            token: None,
        }),
    }
}

fn view_of(entry: &Incoming) -> TransferStateView {
    match &entry.state {
        OfferState::Accepted(rx) => rx.state(),
        OfferState::Pending { .. } | OfferState::Accepting => {
            pending_view(&entry.manifest, TransferState::Pending)
        }
        OfferState::Declined => pending_view(&entry.manifest, TransferState::Declined),
        OfferState::Expired => pending_view(&entry.manifest, TransferState::Expired),
        OfferState::Cancelled => pending_view(&entry.manifest, TransferState::Cancelled),
    }
}

fn accepted_body(rx: &TransferReceiver) -> Accepted201 {
    let st = rx.state();
    Accepted201 {
        id: st.id.clone(),
        state: "accepted",
        chunk_size: st.chunk_size,
        files: st
            .files
            .iter()
            .filter_map(|f| {
                f.token.clone().map(|token| TokenEntry {
                    fid: f.fid,
                    token,
                    verified_offset: f.verified_offset,
                })
            })
            .collect(),
        bundle: st.bundle.as_ref().and_then(|b| {
            b.token.clone().map(|token| BundleToken {
                token,
                verified_offset: b.verified_offset,
            })
        }),
    }
}

/// `POST /v1/transfers` (protocol.md 7.2, 7.3).
pub(crate) async fn offer(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let peer = conn.device_id();
    let mut body = BodyReader::new(req.into_body());
    let bytes = match body.read_to_limit(MAX_MANIFEST_BODY).await {
        Ok(b) => b,
        Err(ReadLimitError::TooLarge) => {
            let e = HttpError::from_code(ErrorCode::TooLarge, "manifest over 16 MiB");
            let _ = respond_error(&mut respond, &e).await;
            return;
        }
        Err(ReadLimitError::Io(_)) => return,
    };
    let result = offer_inner(&inner, peer, &bytes).await;
    match result {
        Ok(OfferReply::Created(body)) => {
            let _ = respond_json(&mut respond, StatusCode::CREATED, &body).await;
        }
        Ok(OfferReply::Pending(body)) => {
            let _ = respond_json(&mut respond, StatusCode::ACCEPTED, &body).await;
        }
        Ok(OfferReply::Known(view)) => {
            let _ = respond_json(&mut respond, StatusCode::OK, &view).await;
        }
        Err(e) => {
            let _ = respond_error(&mut respond, &e).await;
        }
    }
}

enum OfferReply {
    Created(Accepted201),
    Pending(Pending202),
    Known(Box<TransferStateView>),
}

/// Registers an outbox item pulled from `peer` exactly as an offer from it (protocol.md
/// 7.8): the `files` grant decides between accepting now and asking the user.
pub(crate) async fn register_pulled(
    inner: &Arc<Inner>,
    peer: DeviceId,
    manifest: &[u8],
) -> std::result::Result<(), HttpError> {
    offer_inner(inner, peer, manifest).await.map(drop)
}

/// The receiver of an incoming transfer: `Ok` once accepted, `Err(true)` while it waits
/// for the user, `Err(false)` once declined, expired or cancelled. `None` if unknown.
pub(crate) fn receiver_state(
    inner: &Inner,
    id: &str,
) -> Option<std::result::Result<TransferReceiver, bool>> {
    lock(&inner.incoming).get(id).map(|e| match &e.state {
        OfferState::Accepted(rx) => Ok(rx.clone()),
        OfferState::Pending { .. } | OfferState::Accepting => Err(true),
        _ => Err(false),
    })
}

/// Fails an incoming transfer from inside (an outbox pull that cannot go on): deletes its
/// `.part` files and reports it once.
pub(crate) async fn fail_local(inner: &Arc<Inner>, id: &str, why: String) {
    let (owner, rx) = {
        let mut map = lock(&inner.incoming);
        let Some(e) = map.get_mut(id) else { return };
        if e.reported {
            return;
        }
        e.reported = true;
        let rx = if let OfferState::Accepted(rx) = &e.state {
            Some(rx.clone())
        } else {
            e.state = OfferState::Cancelled;
            None
        };
        (e.owner, rx)
    };
    if let Some(rx) = rx {
        let _ = rx.cancel().await;
    }
    remove_owner(inner, id);
    inner.transfer_ended(
        id,
        &owner,
        TransferDirection::Receive,
        TransferEnd::Failed(why),
    );
}

async fn offer_inner(
    inner: &Arc<Inner>,
    peer: DeviceId,
    bytes: &[u8],
) -> std::result::Result<OfferReply, HttpError> {
    let manifest: Manifest = serde_json::from_slice(bytes)
        .map_err(|e| HttpError::from_code(ErrorCode::BadRequest, e.to_string()))?;
    manifest.validate().map_err(HttpError::from)?;
    let id = manifest.id.clone();
    let manifest = Arc::new(manifest);
    let grants = inner.pins.get(&peer).ok_or(Error::NotPaired)?.grants;
    {
        let mut map = lock(&inner.incoming);
        if let Some(ex) = map.get(&id) {
            return if ex.owner == peer && *ex.manifest == *manifest {
                Ok(OfferReply::Known(Box::new(view_of(ex))))
            } else {
                Err(HttpError::from_code(
                    ErrorCode::Conflict,
                    "transfer ID already used with another manifest",
                ))
            };
        }
        let active = map
            .values()
            .filter(|e| e.owner == peer && e.is_active())
            .count();
        if active >= MAX_PER_PEER {
            return Err(HttpError::from_code(
                ErrorCode::RateLimited,
                "4 transfers already in progress",
            ));
        }
        let space = inner
            .free_space
            .space(inner.receive_root())
            .map_err(|e| HttpError::from(Error::Io(e)))?;
        if !space.fits(manifest.total_bytes) {
            return Err(HttpError::from_code(ErrorCode::NoSpace, ""));
        }
        let state = if grants.files {
            OfferState::Accepting
        } else {
            let expires_at_ms = unix_ms() + u64::try_from(OFFER_TTL.as_millis()).unwrap_or(0);
            OfferState::Pending {
                expires_at_ms,
                deadline: Instant::now() + OFFER_TTL,
            }
        };
        let status = if grants.files {
            TransferStatus::Active
        } else {
            TransferStatus::Offered
        };
        map.insert(
            id.clone(),
            Incoming::new(peer, Arc::clone(&manifest), state),
        );
        lock(&inner.transfers).begin(snapshot_of(&peer, &manifest, status));
    }
    if !grants.files {
        let expires_at = match lock(&inner.incoming).get(&id).map(|e| &e.state) {
            Some(OfferState::Pending { expires_at_ms, .. }) => *expires_at_ms,
            _ => 0,
        };
        inner.emit(HostEvent::TransferOffer {
            id: id.clone(),
            peer: peer.to_hex(),
            short_id: peer.short_id(),
            files: manifest
                .files
                .iter()
                .take(OFFER_FILES_MAX)
                .map(|f| OfferedFile {
                    path: f.path.join("/"),
                    size: f.size,
                })
                .collect(),
            file_count: manifest.files.len(),
            total_bytes: manifest.total_bytes,
            note: manifest.note.clone(),
        });
        return Ok(OfferReply::Pending(Pending202 {
            id,
            state: "pending",
            expires_at,
        }));
    }
    match accept_now(inner, &id, peer, &manifest).await {
        Ok(rx) => Ok(OfferReply::Created(accepted_body(&rx))),
        Err(e) => {
            lock(&inner.incoming).remove(&id);
            inner.transfer_ended(
                &id,
                &peer,
                TransferDirection::Receive,
                TransferEnd::Failed(format!("could not accept: {e}")),
            );
            Err(HttpError::from(e))
        }
    }
}

/// A fresh book entry for an incoming manifest.
fn snapshot_of(peer: &DeviceId, manifest: &Manifest, status: TransferStatus) -> TransferSnapshot {
    TransferSnapshot::new(
        &manifest.id,
        peer,
        TransferDirection::Receive,
        status,
        manifest.total_bytes,
        manifest.files.len(),
        manifest.note.clone(),
    )
}

/// Accepts a manifest into staging and records its owner (protocol.md 7.3, SR-13). If the
/// user cancelled while staging was being created, the new staging is removed again and
/// [`Error::Cancelled`] is returned.
async fn accept_now(
    inner: &Arc<Inner>,
    id: &str,
    owner: DeviceId,
    manifest: &Manifest,
) -> Result<TransferReceiver> {
    let opts = AcceptOptions {
        sink: inner.sink_binding(&owner),
        ..AcceptOptions::default()
    };
    let rx = TransferReceiver::accept(
        inner.receive_root(),
        manifest.clone(),
        Arc::clone(&inner.free_space),
        opts,
    )
    .await?;
    write_owner(inner, id, owner).await?;
    let cancelled = {
        let mut map = lock(&inner.incoming);
        match map.get_mut(id) {
            Some(e) if matches!(e.state, OfferState::Accepting) => {
                e.state = OfferState::Accepted(rx.clone());
                e.started = Instant::now();
                false
            }
            _ => true,
        }
    };
    if cancelled {
        let _ = rx.cancel().await;
        remove_owner(inner, id);
        return Err(Error::Cancelled);
    }
    tracing::debug!(peer = %short(&owner), files = manifest.files.len(), "transfer accepted");
    Ok(rx)
}

/// Finds the receiver for `id` owned by `peer` (SR-13: another device gets 404).
fn lookup(
    inner: &Inner,
    id: &str,
    peer: &DeviceId,
) -> std::result::Result<TransferReceiver, HttpError> {
    let map = lock(&inner.incoming);
    let e = map
        .get(id)
        .filter(|e| e.owner == *peer)
        .ok_or_else(|| HttpError::from_code(ErrorCode::NotFound, ""))?;
    match &e.state {
        OfferState::Accepted(rx) => Ok(rx.clone()),
        OfferState::Pending { .. } | OfferState::Accepting => Err(HttpError::from_code(
            ErrorCode::Conflict,
            "offer not accepted yet",
        )),
        OfferState::Declined => Err(HttpError::from_code(ErrorCode::Declined, "")),
        OfferState::Expired => Err(HttpError::from_code(ErrorCode::Expired, "")),
        OfferState::Cancelled => Err(HttpError::from_code(ErrorCode::Cancelled, "")),
    }
}

/// `content-range: bytes <start>-<end>/<total>` (protocol.md 7.4).
pub(crate) fn parse_content_range(v: &str) -> Option<(u64, u64, u64)> {
    let rest = v.trim().strip_prefix("bytes ")?;
    let (range, total) = rest.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn put_error(e: PutError) -> HttpError {
    match e {
        PutError::BadToken => HttpError::from_code(ErrorCode::BadToken, ""),
        PutError::Cancelled => HttpError::from_code(ErrorCode::Cancelled, ""),
        PutError::Core(e) => HttpError::from(e),
    }
}

/// `PUT /v1/transfers/{id}/files/{fid}` and `/bundle` (protocol.md 7.4, 7.7).
pub(crate) async fn put(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    id: String,
    target: Target,
) {
    let peer = conn.device_id();
    let rx = match lookup(&inner, &id, &peer) {
        Ok(rx) => rx,
        Err(e) => {
            let _ = respond_error(&mut respond, &e).await;
            return;
        }
    };
    let (head, body) = req.into_parts();
    let checked = check_put_headers(&head.headers, &rx, target);
    let (token, start, end) = match checked {
        Ok(v) => v,
        Err(e) => {
            let _ = respond_error(&mut respond, &e).await;
            return;
        }
    };
    tracing::debug!(?target, start, end, "PUT begin");
    let begun = match target {
        Target::File(fid) => rx.begin_file(fid, &token, start, end).await,
        Target::Bundle => rx.begin_bundle(&token, start, end).await,
    };
    let mut upload = match begun {
        Ok(u) => u,
        Err(e) => {
            tracing::debug!(error = %e, ?target, "PUT refused at begin");
            let _ = respond_error(&mut respond, &put_error(e)).await;
            return;
        }
    };
    let mut body = BodyReader::new(body);
    let received = upload.receive(&mut body).await;
    let reply = match received {
        Ok(()) => {
            let root = match body.trailers().await {
                Ok(Some(t)) => header(&t, ROOT_TRAILER).and_then(|s| hash_from_hex(s).ok()),
                _ => None,
            };
            match upload.finish(root).await {
                Ok(r) => Ok(r),
                Err(e) => {
                    tracing::debug!(error = %e, ?target, staged = ?std::fs::read_dir(rx.staging_dir()).map(|d| d.flatten().map(|e| e.file_name()).collect::<Vec<_>>()), "PUT failed at finish");
                    Err(put_error(e))
                }
            }
        }
        Err(PutError::Core(Error::ChunkHashMismatch { .. })) => {
            let r = upload.response();
            Err(HttpError::from_code(ErrorCode::ChunkHashMismatch, "")
                .with_offset(r.verified_offset))
        }
        Err(PutError::Core(Error::Io(e))) if body_was_cut(&body, &e) => {
            // The body was cut; whole verified chunks are kept (7.4). Nobody to answer.
            if is_remote_reset(&e) {
                conn.record_reset();
            }
            after_put(&inner, &id, &rx, target);
            return;
        }
        Err(PutError::Core(Error::Io(e))) => {
            // A local failure (disk, staging folder) with the body still flowing. Answer
            // it: without a reply the sender sees a reset, takes it for a lost connection
            // and resumes into the same failure forever.
            tracing::warn!(error = %e, ?target, "PUT failed writing to storage");
            Err(put_error(PutError::Core(Error::Io(e))))
        }
        Err(e) => {
            tracing::debug!(error = %e, ?target, "PUT failed while receiving");
            Err(put_error(e))
        }
    };
    match reply {
        Ok(r) => {
            let _ = respond_json(&mut respond, StatusCode::OK, &r).await;
        }
        Err(e) => {
            // The 422 replies stop the sender; dropping the unread body afterwards makes
            // `h2` reset the stream with NO_ERROR (protocol.md 7.4, SR-22).
            let _ = respond_error(&mut respond, &e).await;
        }
    }
    drop(body);
    after_put(&inner, &id, &rx, target);
}

/// True when a receive error came from the body itself (stream reset, connection lost,
/// body ended mid-record) rather than from this host's storage.
pub(crate) fn body_was_cut(body: &BodyReader, e: &std::io::Error) -> bool {
    body.transport_failed() || e.kind() == std::io::ErrorKind::UnexpectedEof
}

fn check_put_headers(
    h: &HeaderMap,
    rx: &TransferReceiver,
    target: Target,
) -> std::result::Result<(String, u64, u64), HttpError> {
    let token = header(h, TOKEN_HEADER)
        .ok_or_else(|| HttpError::from_code(ErrorCode::BadToken, "missing pd-token"))?
        .to_owned();
    let (start, end, total) = header(h, http::header::CONTENT_RANGE.as_str())
        .and_then(parse_content_range)
        .ok_or_else(|| HttpError::from_code(ErrorCode::BadRequest, "bad content-range"))?;
    if let Some(cs) = header(h, CHUNK_SIZE_HEADER) {
        if cs.parse::<u32>().ok() != Some(rx.chunk_size()) {
            return Err(HttpError::from_code(
                ErrorCode::BadRequest,
                format!("pd-chunk-size must be {}", rx.chunk_size()),
            ));
        }
    }
    let declared = match target {
        Target::File(fid) => rx.manifest().files.get(fid as usize).map(|f| f.size),
        Target::Bundle => rx.manifest().bundle.map(|b| b.size),
    }
    .ok_or_else(|| HttpError::from_code(ErrorCode::NotFound, ""))?;
    if total != declared {
        return Err(HttpError::from_code(
            ErrorCode::BadRequest,
            "content-range total differs from the declared size",
        ));
    }
    Ok((token, start, end))
}

/// After every `PUT` (or outbox pull): report integrity failures and completion once.
pub(crate) fn after_put(inner: &Arc<Inner>, id: &str, rx: &TransferReceiver, target: Target) {
    let st = rx.state();
    let failed_stream = match target {
        Target::File(fid) => st
            .files
            .iter()
            .find(|f| f.fid == fid)
            .is_some_and(|f| f.state == FileState::Failed),
        Target::Bundle => st
            .bundle
            .as_ref()
            .is_some_and(|b| b.state == FileState::Failed),
    };
    let owner = lock(&inner.incoming).get(id).map(|e| e.owner);
    let Some(owner) = owner else { return };
    // A sink refusal is not an integrity failure; the final report carries its message.
    if failed_stream && rx.failure().is_none() {
        inner.emit(HostEvent::TransferFailed {
            id: id.to_owned(),
            peer: owner.to_hex(),
            direction: TransferDirection::Receive,
            reason: format!("failed integrity check from {}", inner.label(&owner)),
        });
    }
    report_if_done(inner, id, rx);
}

fn report_if_done(inner: &Arc<Inner>, id: &str, rx: &TransferReceiver) {
    let state = rx.transfer_state();
    if !matches!(
        state,
        TransferState::Complete | TransferState::Failed | TransferState::Cancelled
    ) {
        return;
    }
    let (owner, started) = {
        let mut map = lock(&inner.incoming);
        let Some(e) = map.get_mut(id) else { return };
        if e.reported {
            return;
        }
        e.reported = true;
        (e.owner, e.started)
    };
    remove_owner(inner, id);
    let end = match state {
        TransferState::Complete => {
            let st = rx.state();
            let manifest = rx.manifest();
            let roots = manifest
                .files
                .iter()
                .zip(&st.files)
                .map(|(f, v)| FileRoot {
                    path: f.path.join("/"),
                    size: f.size,
                    root: (!f.in_bundle && f.size > 0)
                        .then(|| v.root.clone())
                        .flatten(),
                })
                .collect();
            TransferEnd::Complete {
                bytes: st.total_bytes,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                roots,
                bundle_root: st.bundle.and_then(|b| b.root),
                saved: rx.saved_paths(),
                folder: (!rx.is_staged()).then(|| rx.receive_root().to_path_buf()),
                delivered: rx.delivered(),
            }
        }
        TransferState::Cancelled => TransferEnd::Cancelled("transfer cancelled".into()),
        _ => TransferEnd::Failed(
            rx.failure()
                .unwrap_or_else(|| format!("transfer {state:?}").to_lowercase()),
        ),
    };
    inner.transfer_ended(id, &owner, TransferDirection::Receive, end);
}

/// `GET /v1/transfers/{id}` (protocol.md 7.5).
pub(crate) async fn get_state(
    inner: &Arc<Inner>,
    conn: &Connection,
    respond: &mut SendResponse<Bytes>,
    id: &str,
) {
    let peer = conn.device_id();
    let view = lock(&inner.incoming)
        .get(id)
        .filter(|e| e.owner == peer)
        .map(view_of);
    match view {
        Some(v) => {
            let _ = respond_json(respond, StatusCode::OK, &v).await;
        }
        None => {
            let _ = respond_error(respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
        }
    }
}

#[derive(Deserialize)]
struct CancelBody {
    #[serde(default)]
    reason: Option<String>,
}

/// `DELETE /v1/transfers/{id}` (protocol.md 7.5).
pub(crate) async fn cancel(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
    id: String,
) {
    let peer = conn.device_id();
    let mut body = BodyReader::new(req.into_body());
    let reason = body
        .read_to_limit(4096)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<CancelBody>(&b).ok())
        .and_then(|b| b.reason)
        .unwrap_or_else(|| "user".into());
    let found = {
        let mut map = lock(&inner.incoming);
        map.get_mut(&id).filter(|e| e.owner == peer).map(|e| {
            if let OfferState::Accepted(rx) = &e.state {
                Some(rx.clone())
            } else {
                e.state = OfferState::Cancelled;
                None
            }
        })
    };
    let Some(rx) = found else {
        let _ = respond_error(&mut respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
        return;
    };
    if let Some(rx) = &rx {
        if let Err(e) = rx.cancel().await {
            tracing::debug!(error = %e, "cancel cleanup failed");
        }
    }
    let _ = crate::net::respond(&mut respond, StatusCode::NO_CONTENT, None, Bytes::new()).await;
    let reported = lock(&inner.incoming)
        .get_mut(&id)
        .is_some_and(|e| std::mem::replace(&mut e.reported, true));
    remove_owner(&inner, &id);
    if !reported {
        let safe_reason: String = reason
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(32)
            .collect();
        inner.transfer_ended(
            &id,
            &peer,
            TransferDirection::Receive,
            TransferEnd::Cancelled(format!("cancelled by sender ({safe_reason})")),
        );
    }
}

/// Cancels an incoming transfer at the local user's request (protocol.md 7.5): deletes
/// every `.part`, makes later `PUT`s fail with `410 cancelled`, and pushes a `transfer`
/// frame with state `cancelled`, reason `user`, so the sender stops (5.13). A pending
/// offer is withdrawn the same way.
pub(crate) async fn cancel_local(inner: &Arc<Inner>, id: &str) -> Result<()> {
    let (owner, rx) = {
        let mut map = lock(&inner.incoming);
        let e = map.get_mut(id).ok_or(Error::NotFound)?;
        if e.reported {
            return Err(Error::NotFound);
        }
        let rx = match &e.state {
            OfferState::Accepted(rx) => {
                if matches!(
                    rx.transfer_state(),
                    TransferState::Complete | TransferState::Failed | TransferState::Cancelled
                ) {
                    return Err(Error::NotFound);
                }
                Some(rx.clone())
            }
            OfferState::Pending { .. } | OfferState::Accepting => {
                e.state = OfferState::Cancelled;
                None
            }
            OfferState::Declined | OfferState::Expired | OfferState::Cancelled => {
                return Err(Error::NotFound)
            }
        };
        e.reported = true;
        (e.owner, rx)
    };
    if let Some(rx) = &rx {
        if let Err(e) = rx.cancel().await {
            tracing::debug!(error = %e, "cancel cleanup failed");
        }
    }
    remove_owner(inner, id);
    send_decision(inner, owner, id, FrameState::Cancelled, Some("user"));
    inner.transfer_ended(
        id,
        &owner,
        TransferDirection::Receive,
        TransferEnd::Cancelled("cancelled by you".into()),
    );
    Ok(())
}

/// The user's decision on a pending offer (protocol.md 7.3); the sender learns it from a
/// `transfer` frame (5.13).
pub(crate) async fn respond_offer(inner: &Arc<Inner>, id: &str, accept: bool) -> Result<()> {
    let (owner, manifest) = {
        let mut map = lock(&inner.incoming);
        let e = map.get_mut(id).ok_or(Error::NotFound)?;
        if !matches!(e.state, OfferState::Pending { .. }) {
            return Err(Error::BadRequest("offer is not pending".into()));
        }
        e.state = if accept {
            OfferState::Accepting
        } else {
            OfferState::Declined
        };
        (e.owner, Arc::clone(&e.manifest))
    };
    let state = if accept {
        match accept_now(inner, id, owner, &manifest).await {
            Ok(_) => {
                lock(&inner.transfers).set_state(
                    id,
                    TransferDirection::Receive,
                    TransferStatus::Active,
                );
                FrameState::Accepted
            }
            // Cancelled by the user while staging was created: already reported.
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                if let Some(en) = lock(&inner.incoming).get_mut(id) {
                    en.state = OfferState::Cancelled;
                    en.reported = true;
                }
                send_decision(inner, owner, id, FrameState::Failed, Some("error"));
                inner.transfer_ended(
                    id,
                    &owner,
                    TransferDirection::Receive,
                    TransferEnd::Failed(format!("could not accept: {e}")),
                );
                return Err(e);
            }
        }
    } else {
        if let Some(en) = lock(&inner.incoming).get_mut(id) {
            en.reported = true;
        }
        inner.transfer_ended(
            id,
            &owner,
            TransferDirection::Receive,
            TransferEnd::Cancelled("declined".into()),
        );
        FrameState::Declined
    };
    send_decision(inner, owner, id, state, None);
    Ok(())
}

fn send_decision(
    inner: &Inner,
    owner: DeviceId,
    id: &str,
    state: FrameState,
    reason: Option<&str>,
) {
    if let Some(s) = inner.registry.session(&owner) {
        s.send(Frame::Transfer(TransferFrame {
            id: id.to_owned(),
            state,
            reason: reason.map(str::to_owned),
        }));
    }
}

/// Reloads unfinished incoming transfers after a restart (protocol.md 7.6): each `.part`
/// is truncated to its durable verified offset.
pub(crate) async fn load_persisted(inner: &Arc<Inner>) {
    let dir = inner.data_dir.join("incoming");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut found: Vec<(String, DeviceId)> = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else {
            continue;
        };
        let rec = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<OwnerRecord>(&b).ok());
        match rec {
            Some(r) => found.push((id, r.owner)),
            None => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    for (id, owner) in found {
        let loaded = TransferReceiver::load_with(
            inner.receive_root(),
            &id,
            Arc::clone(&inner.free_space),
            SyncPolicy::default(),
            inner.sink_binding(&owner),
        )
        .await;
        match loaded {
            Ok(rx)
                if !matches!(
                    rx.transfer_state(),
                    TransferState::Complete | TransferState::Cancelled | TransferState::Failed
                ) =>
            {
                let manifest = Arc::new(rx.manifest().clone());
                let bytes = rx.state().bytes_verified;
                let mut snap = snapshot_of(&owner, &manifest, TransferStatus::Active);
                snap.bytes = bytes;
                lock(&inner.transfers).begin(snap);
                let mut e = Incoming::new(owner, manifest, OfferState::Accepted(rx));
                e.last = (Instant::now(), bytes);
                lock(&inner.incoming).insert(id, e);
            }
            Ok(rx) => {
                tracing::debug!(state = ?rx.transfer_state(), "persisted transfer already finished");
                remove_owner(inner, &id);
            }
            Err(e) => {
                tracing::info!(error = %e, "persisted transfer could not be reloaded; dropped");
                remove_owner(inner, &id);
            }
        }
    }
}

/// Cancels and forgets every transfer from an unpaired peer (protocol.md 6.4).
pub(crate) async fn drop_peer(inner: &Arc<Inner>, peer: &DeviceId) {
    let gone: Vec<(String, Option<TransferReceiver>)> = {
        let mut map = lock(&inner.incoming);
        let ids: Vec<String> = map
            .iter()
            .filter(|(_, e)| e.owner == *peer)
            .map(|(k, _)| k.clone())
            .collect();
        ids.into_iter()
            .filter_map(|id| {
                map.remove(&id).map(|e| {
                    if !e.reported {
                        lock(&inner.transfers).finish(
                            &id,
                            TransferDirection::Receive,
                            TransferStatus::Cancelled,
                            Some("device unpaired".into()),
                        );
                    }
                    let rx = match e.state {
                        OfferState::Accepted(rx) => Some(rx),
                        _ => None,
                    };
                    (id, rx)
                })
            })
            .collect()
    };
    for (id, rx) in gone {
        if let Some(rx) = rx {
            let _ = rx.cancel().await;
        }
        remove_owner(inner, &id);
    }
}

/// Once a second: expires pending offers (10 min, 7.3), reports receive progress, and
/// hourly deletes `.part` folders idle for 24 h (SR-20).
pub(crate) async fn maintenance(inner: Arc<Inner>) {
    let mut last_cleanup: Option<Instant> = None;
    loop {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let now = Instant::now();
        let mut progress = Vec::new();
        let mut expired = Vec::new();
        {
            let mut map = lock(&inner.incoming);
            for (id, e) in map.iter_mut() {
                match &e.state {
                    OfferState::Pending { deadline, .. } if now >= *deadline => {
                        e.state = OfferState::Expired;
                        e.reported = true;
                        expired.push((id.clone(), e.owner));
                    }
                    OfferState::Accepted(rx) if !e.reported => {
                        let bytes = rx.state().bytes_verified;
                        if bytes != e.last.1 {
                            let dt = now.duration_since(e.last.0).as_secs_f64().max(0.001);
                            #[allow(
                                clippy::cast_precision_loss,
                                clippy::cast_possible_truncation,
                                clippy::cast_sign_loss
                            )] // display rate only
                            let rate = (bytes.saturating_sub(e.last.1) as f64 / dt) as u64;
                            e.last = (now, bytes);
                            progress.push((
                                id.clone(),
                                e.owner,
                                bytes,
                                e.manifest.total_bytes,
                                rate,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        for (id, owner, bytes, total, rate) in progress {
            inner.transfer_progress(&id, &owner, TransferDirection::Receive, bytes, total, rate);
        }
        for (id, owner) in expired {
            inner.transfer_ended(
                &id,
                &owner,
                TransferDirection::Receive,
                TransferEnd::Failed("offer expired".into()),
            );
        }
        if last_cleanup.is_none_or(|t| now.duration_since(t) >= CLEANUP_EVERY) {
            last_cleanup = Some(now);
            match cleanup_idle(inner.receive_root(), IDLE_TIMEOUT).await {
                Ok(ids) => {
                    let gone: Vec<(String, Incoming)> = {
                        let mut map: std::sync::MutexGuard<'_, HashMap<String, Incoming>> =
                            lock(&inner.incoming);
                        ids.into_iter()
                            .filter_map(|id| map.remove(&id).map(|e| (id, e)))
                            .collect()
                    };
                    for (id, e) in gone {
                        remove_owner(&inner, &id);
                        if !e.reported {
                            inner.transfer_ended(
                                &id,
                                &e.owner,
                                TransferDirection::Receive,
                                TransferEnd::Failed("no data for 24 hours; removed".into()),
                            );
                        }
                    }
                }
                Err(e) => tracing::debug!(error = %e, "idle cleanup failed"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_content_range;

    #[test]
    fn content_range_parses_the_7_4_form() {
        assert_eq!(
            parse_content_range("bytes 0-681574399/681574400"),
            Some((0, 681_574_399, 681_574_400))
        );
        assert_eq!(parse_content_range("bytes 5-4"), None);
        assert_eq!(parse_content_range("items 0-1/2"), None);
        assert_eq!(parse_content_range("bytes -1/2"), None);
    }
}
