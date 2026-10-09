//! Blobs (protocol.md 5.15): bytes that never travel in frames (long clipboard text and
//! images, 5.7). The holder keeps them in memory, bound to the Device ID each was announced
//! to (SR-13), for 5 minutes; the receiver fetches `GET /v1/blobs/{id}` over a connection it
//! dialled (11.1: only the dialler sends requests), reads at most the announced size and
//! checks that the BLAKE3 of the bytes equals the ID before anything reaches the UI.
//!
//! SR-24: clipboard sync stays capped at 1 MiB of text and 16 MiB per image.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use h2::server::SendResponse;
use http::{HeaderValue, Method, Request, Response, StatusCode};
use tokio::time::Instant;

use super::events::{Base64Bytes, HostEvent, Secret};
use super::{connect, lock, Inner};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::net::{respond_error, send_all, BodyReader, Connection, HttpError};
use crate::session::types::{BlobRef, Clipboard, ClipboardKind};
use crate::session::ErrorCode;

/// A blob lives this long after its frame (protocol.md 5.15).
pub(crate) const BLOB_TTL: Duration = Duration::from_mins(5);

/// Most bytes held in blobs at once. *Choice:* 64 MiB holds four full-size images; the
/// oldest blob goes first when a new one does not fit.
pub(crate) const BLOB_CAP: usize = 64 * 1024 * 1024;

/// Longest clipboard text synced, as a blob (SR-24).
pub(crate) const TEXT_BLOB_MAX: usize = 1024 * 1024;

/// Largest clipboard image (`docs/core-api.md` section 4).
pub(crate) const IMAGE_BLOB_MAX: usize = 16 * 1024 * 1024;

/// How long an expired blob is still answered with `410 expired` rather than `404`.
const TOMBSTONE_FOR: Duration = Duration::from_mins(30);

/// Most expired IDs remembered for the `410` answer.
const MAX_TOMBSTONES: usize = 1024;

/// How long a fetch may take before it is dropped.
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug)]
struct Blob {
    bytes: Bytes,
    mime: String,
    peers: HashSet<DeviceId>,
    announced: Instant,
}

/// Why a blob is not served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BlobMiss {
    /// Unknown, or announced to another device (SR-13: the same answer).
    NotFound,
    /// Its 5 minutes are over.
    Expired,
}

/// In-memory blobs of this host, keyed by lowercase hex BLAKE3.
#[derive(Debug, Default)]
pub(crate) struct BlobStore {
    live: HashMap<String, Blob>,
    /// Expired IDs with the peers they were announced to, for `410 expired`.
    expired: HashMap<String, (HashSet<DeviceId>, Instant)>,
    total: usize,
}

/// Lowercase hex BLAKE3, the blob ID (protocol.md 5.15).
pub(crate) fn blob_id(bytes: &[u8]) -> String {
    crate::transfer::hash_to_hex(blake3::hash(bytes).as_bytes())
}

impl BlobStore {
    /// Holds `bytes` for `peer` and returns its ID. Announcing the same bytes again (to the
    /// same or another peer) restarts the 5 minutes. `None` if the bytes exceed the cap.
    pub(crate) fn put(
        &mut self,
        bytes: Bytes,
        mime: &str,
        peer: DeviceId,
        now: Instant,
    ) -> Option<String> {
        if bytes.len() > BLOB_CAP {
            return None;
        }
        self.prune(now);
        let id = blob_id(&bytes);
        if let Some(b) = self.live.get_mut(&id) {
            b.peers.insert(peer);
            b.announced = now;
            return Some(id);
        }
        while self.total + bytes.len() > BLOB_CAP {
            let Some(oldest) = self
                .live
                .iter()
                .min_by_key(|(_, b)| b.announced)
                .map(|(k, _)| k.clone())
            else {
                break;
            };
            self.retire(&oldest, now);
        }
        self.total += bytes.len();
        self.expired.remove(&id);
        self.live.insert(
            id.clone(),
            Blob {
                bytes,
                mime: mime.to_owned(),
                peers: HashSet::from([peer]),
                announced: now,
            },
        );
        Some(id)
    }

    /// The bytes and MIME type of `id` if it was announced to `peer` and is still live.
    pub(crate) fn get(
        &mut self,
        id: &str,
        peer: &DeviceId,
        now: Instant,
    ) -> Result<(Bytes, String), BlobMiss> {
        self.prune(now);
        if let Some(b) = self.live.get(id) {
            return if b.peers.contains(peer) {
                Ok((b.bytes.clone(), b.mime.clone()))
            } else {
                Err(BlobMiss::NotFound)
            };
        }
        match self.expired.get(id) {
            Some((peers, _)) if peers.contains(peer) => Err(BlobMiss::Expired),
            _ => Err(BlobMiss::NotFound),
        }
    }

    /// Bytes held now.
    #[cfg(test)]
    pub(crate) fn total(&self) -> usize {
        self.total
    }

    fn retire(&mut self, id: &str, now: Instant) {
        if let Some(b) = self.live.remove(id) {
            self.total -= b.bytes.len();
            self.expired.insert(id.to_owned(), (b.peers, now));
        }
    }

    fn prune(&mut self, now: Instant) {
        let old: Vec<String> = self
            .live
            .iter()
            .filter(|(_, b)| now.duration_since(b.announced) >= BLOB_TTL)
            .map(|(k, _)| k.clone())
            .collect();
        for id in old {
            self.retire(&id, now);
        }
        self.expired
            .retain(|_, (_, at)| now.duration_since(*at) < TOMBSTONE_FOR);
        if self.expired.len() > MAX_TOMBSTONES {
            let mut by_age: Vec<(String, Instant)> = self
                .expired
                .iter()
                .map(|(k, (_, at))| (k.clone(), *at))
                .collect();
            by_age.sort_by_key(|(_, at)| *at);
            for (k, _) in by_age.into_iter().take(self.expired.len() - MAX_TOMBSTONES) {
                self.expired.remove(&k);
            }
        }
    }
}

/// `GET /v1/blobs/{id}` (protocol.md 5.15, 7.1): the bytes with `content-type` and
/// `content-length`; `404 not_found` for an unknown ID or one announced to another device,
/// `410 expired` after the 5 minutes.
pub(crate) async fn serve(
    inner: &Inner,
    conn: &Connection,
    respond: &mut SendResponse<Bytes>,
    id: &str,
) {
    let got = lock(&inner.blobs).get(id, &conn.device_id(), Instant::now());
    let (bytes, mime) = match got {
        Ok(v) => v,
        Err(miss) => {
            let code = match miss {
                BlobMiss::NotFound => ErrorCode::NotFound,
                BlobMiss::Expired => ErrorCode::Expired,
            };
            let _ = respond_error(respond, &HttpError::from_code(code, "")).await;
            return;
        }
    };
    let mut head = Response::new(());
    *head.status_mut() = StatusCode::OK;
    let ct = HeaderValue::from_str(&mime)
        .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
    head.headers_mut().insert(http::header::CONTENT_TYPE, ct);
    head.headers_mut()
        .insert(http::header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
    match respond.send_response(head, bytes.is_empty()) {
        Ok(mut stream) if !bytes.is_empty() => {
            let _ = send_all(&mut stream, bytes, true).await;
        }
        Ok(_) => {}
        Err(e) => tracing::debug!(error = %e, "blob response failed"),
    }
}

/// Converts CRLF and lone CR to LF, the form clipboard text is hashed in (protocol.md
/// 5.7). A text blob carries this form so its ID equals the clip's `hash`.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// A clipboard frame that carries its content as a blob (protocol.md 5.7, 5.15).
pub(crate) fn blob_clipboard(
    kind: ClipboardKind,
    mime: &str,
    id: String,
    size: usize,
    sensitive: bool,
) -> Clipboard {
    Clipboard {
        kind,
        mime: mime.to_owned(),
        hash: id.clone(),
        text: None,
        blob: Some(BlobRef {
            id,
            size: size as u64,
        }),
        sensitive,
    }
}

/// Fetches the blob a `clipboard` frame names and reports the clip (protocol.md 5.7,
/// 5.15). A failed fetch is logged at debug and reports nothing; the echo hash is
/// recorded only after the bytes verified.
pub(crate) fn fetch_clipboard(inner: &Arc<Inner>, peer: DeviceId, clip: Clipboard) {
    let i2 = Arc::clone(inner);
    inner.spawn(async move {
        let fetched = tokio::time::timeout(FETCH_TIMEOUT, fetch_verified(&i2, peer, &clip)).await;
        let bytes = match fetched {
            Ok(Ok(b)) => b,
            Ok(Err(e)) => {
                tracing::debug!(peer = %short(&peer), error = %e, "clipboard blob not fetched");
                return;
            }
            Err(_) => {
                tracing::debug!(peer = %short(&peer), "clipboard blob fetch timed out");
                return;
            }
        };
        let ev = if clip.kind == ClipboardKind::Image {
            HostEvent::ClipboardImage {
                peer: peer.to_hex(),
                mime: clip.mime.clone(),
                data: Secret(Base64Bytes(bytes.to_vec())),
                sensitive: clip.sensitive,
            }
        } else {
            let Ok(text) = String::from_utf8(bytes.to_vec()) else {
                tracing::debug!(peer = %short(&peer), "clipboard blob is not UTF-8; dropped");
                return;
            };
            HostEvent::Clipboard {
                peer: peer.to_hex(),
                text: Secret(Some(text)),
                sensitive: clip.sensitive,
            }
        };
        lock(&i2.clipboard_echo).last_applied = Some(clip.hash.clone());
        i2.emit(ev);
    });
}

/// `GET /v1/blobs/{id}` on our outbound connection to `peer`, bounded by the announced
/// size and the SR-24 cap for the kind, and checked against the ID and the clip hash.
async fn fetch_verified(
    inner: &Arc<Inner>,
    peer: DeviceId,
    clip: &Clipboard,
) -> Result<Bytes, String> {
    let blob = clip.blob.as_ref().ok_or("no blob")?;
    let cap = if clip.kind == ClipboardKind::Image {
        IMAGE_BLOB_MAX
    } else {
        TEXT_BLOB_MAX
    };
    let size = usize::try_from(blob.size).map_err(|_| "size")?;
    if size > cap {
        return Err(format!("announced size {size} over the {cap} byte cap"));
    }
    if blob.id != clip.hash || !is_hex_id(&blob.id) {
        return Err("blob id must equal the clip hash".into());
    }
    let conn = connect::outbound(inner, peer)
        .await
        .map_err(|e| e.to_string())?;
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("/v1/blobs/{}", blob.id))
        .body(())
        .map_err(|e| e.to_string())?;
    let (resp, _send) = conn.request(req, true).await.map_err(|e| e.to_string())?;
    let resp = resp.await.map_err(|e| e.to_string())?;
    if resp.status() != StatusCode::OK {
        return Err(format!("holder answered {}", resp.status()));
    }
    let bytes = BodyReader::new(resp.into_body())
        .read_to_limit(size)
        .await
        .map_err(|_| "body over the announced size or cut".to_owned())?;
    if bytes.len() != size {
        return Err(format!("got {} bytes, announced {size}", bytes.len()));
    }
    if blob_id(&bytes) != blob.id {
        return Err("BLAKE3 of the bytes differs from the blob id".into());
    }
    Ok(bytes)
}

fn is_hex_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids() -> (DeviceId, DeviceId) {
        (
            "ab".repeat(32).parse().unwrap(),
            "cd".repeat(32).parse().unwrap(),
        )
    }

    /// protocol.md 5.15: served only to the announced Device ID, 410 after 5 minutes, the
    /// ID is the BLAKE3 of the bytes.
    #[test]
    fn blob_ttl_and_binding() {
        let (a, b) = ids();
        let mut st = BlobStore::default();
        let t0 = Instant::now();
        let id = st
            .put(Bytes::from_static(b"hello"), "text/plain", a, t0)
            .unwrap();
        assert_eq!(id, blob_id(b"hello"));
        assert_eq!(id, crate::session::types::clipboard_text_hash("hello"));
        assert_eq!(st.get(&id, &a, t0).unwrap().0, Bytes::from_static(b"hello"));
        assert_eq!(st.get(&id, &b, t0), Err(BlobMiss::NotFound));
        assert_eq!(st.get("00", &a, t0), Err(BlobMiss::NotFound));
        // Announced to b as well: both may fetch, and the 5 minutes restart.
        let t1 = t0 + Duration::from_mins(4);
        st.put(Bytes::from_static(b"hello"), "text/plain", b, t1)
            .unwrap();
        let t2 = t0 + Duration::from_mins(6);
        assert!(st.get(&id, &a, t2).is_ok());
        let t3 = t1 + BLOB_TTL;
        assert_eq!(st.get(&id, &a, t3), Err(BlobMiss::Expired));
        assert_eq!(st.get(&id, &b, t3), Err(BlobMiss::Expired));
        assert_eq!(st.total(), 0);
        let (_, c) = (a, "ef".repeat(32).parse::<DeviceId>().unwrap());
        assert_eq!(
            st.get(&id, &c, t3),
            Err(BlobMiss::NotFound),
            "SR-13: same as unknown"
        );
        assert_eq!(st.get(&id, &a, t3 + TOMBSTONE_FOR), Err(BlobMiss::NotFound));
    }

    /// The 64 MiB cap evicts the oldest blob first.
    #[test]
    fn blob_cap_evicts_oldest() {
        let (a, _) = ids();
        let mut st = BlobStore::default();
        let t0 = Instant::now();
        let big = |n: u8| Bytes::from(vec![n; 30 * 1024 * 1024]);
        let first = st.put(big(1), "image/png", a, t0).unwrap();
        let second = st
            .put(big(2), "image/png", a, t0 + Duration::from_secs(1))
            .unwrap();
        let third = st
            .put(big(3), "image/png", a, t0 + Duration::from_secs(2))
            .unwrap();
        let now = t0 + Duration::from_secs(3);
        assert_eq!(st.get(&first, &a, now), Err(BlobMiss::Expired));
        assert!(st.get(&second, &a, now).is_ok());
        assert!(st.get(&third, &a, now).is_ok());
        assert!(st.total() <= BLOB_CAP);
        assert!(st
            .put(Bytes::from(vec![0; BLOB_CAP + 1]), "x", a, now)
            .is_none());
    }

    #[test]
    fn newlines_normalise_to_the_hash_form() {
        let t = "a\r\nb\rc\n";
        assert_eq!(normalize_newlines(t), "a\nb\nc\n");
        assert_eq!(
            blob_id(normalize_newlines(t).as_bytes()),
            crate::session::types::clipboard_text_hash(t)
        );
    }
}
