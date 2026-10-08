//! HTTP/2 over the TLS layer, using the `h2` crate directly.
//! Implements `docs/protocol.md` 3.1 to 3.4, 5.1, 7.1 and section 10 (SR-11, SR-13, SR-15,
//! SR-31).
//!
//! - [`server`]: exclusive listener, connection limits, TLS accept, HTTP/2 accept, routing
//!   to a [`Router`](server::Router), the not-paired gate of SR-13.
//! - [`client`]: staggered dialling of candidate addresses (3.2) and the HTTP/2 client
//!   handshake.
//! - [`connection`]: one authenticated HTTP/2 connection with its peer identity, TLS exporter
//!   value, close signal and keep-alive (3.4).
//!
//! This module also holds the tuned HTTP/2 settings of protocol.md section 10 and small
//! helpers for streaming bodies with flow control and for the 7.1 JSON error body.

pub mod client;
pub mod connection;
pub mod server;

use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Buf, Bytes, BytesMut};
use h2::{RecvStream, SendStream};
use http::{HeaderMap, HeaderValue, Response, StatusCode};
use serde::Serialize;
use tokio::io::{AsyncRead, ReadBuf};

use crate::session::{ErrorCode, Limits};

pub use client::{dial, DialError, DialOptions};
pub use connection::{CloseReason, Connection, Direction, KeepAlive};
pub use server::{Router, Server, TlsAcceptor};

/// HTTP/2 `INITIAL_WINDOW_SIZE` for every stream, both sides: 8 MiB (protocol.md section 10).
/// The `h2` default of 64 KiB caps throughput.
pub const INITIAL_STREAM_WINDOW: u32 = 8 * 1024 * 1024;

/// HTTP/2 connection-level receive window, both sides: 32 MiB (protocol.md section 10).
pub const CONNECTION_WINDOW: u32 = 32 * 1024 * 1024;

/// HTTP/2 `MAX_FRAME_SIZE` advertised by both sides: 64 KiB (protocol.md section 10).
pub const MAX_FRAME_SIZE: u32 = 64 * 1024;

/// Bytes `h2` may buffer per stream before a sender must wait. *Choice:* 2 MiB holds a
/// whole default 1 MiB chunk record, so the sender wakes once per record, not per frame.
pub const MAX_SEND_BUFFER: usize = 2 * 1024 * 1024;

/// Reset streams the peer may leave unaccepted before `h2` itself closes the connection
/// (the 2023 rapid-reset defence, SR-15). Our own per-second counter sits on top.
pub const MAX_PENDING_ACCEPT_RESETS: usize = 20;

/// Media type of the session stream body (protocol.md 5.1).
pub const FRAMES_CONTENT_TYPE: &str = "application/vnd.playdrop.frames";

/// Media type of a chunk-record body (protocol.md 7.4).
pub const CHUNKS_CONTENT_TYPE: &str = "application/vnd.playdrop.chunks";

/// Media type of JSON bodies.
pub const JSON_CONTENT_TYPE: &str = "application/json";

/// The `h2` server settings of protocol.md section 10 with the SR-15 stream and header
/// limits from `limits`.
pub fn server_builder(limits: &Limits) -> h2::server::Builder {
    let mut b = h2::server::Builder::new();
    b.initial_window_size(INITIAL_STREAM_WINDOW)
        .initial_connection_window_size(CONNECTION_WINDOW)
        .max_frame_size(MAX_FRAME_SIZE)
        .max_concurrent_streams(limits.max_concurrent_streams)
        .max_header_list_size(limits.max_header_list_size)
        .max_send_buffer_size(MAX_SEND_BUFFER)
        .max_pending_accept_reset_streams(MAX_PENDING_ACCEPT_RESETS);
    b
}

/// The `h2` client settings of protocol.md section 10: the same windows, frame size and
/// header limit as the server, and server push off.
pub fn client_builder(limits: &Limits) -> h2::client::Builder {
    let mut b = h2::client::Builder::new();
    b.initial_window_size(INITIAL_STREAM_WINDOW)
        .initial_connection_window_size(CONNECTION_WINDOW)
        .max_frame_size(MAX_FRAME_SIZE)
        .max_header_list_size(limits.max_header_list_size)
        .max_send_buffer_size(MAX_SEND_BUFFER)
        .max_pending_accept_reset_streams(MAX_PENDING_ACCEPT_RESETS)
        .enable_push(false);
    b
}

/// Sets `TCP_NODELAY` on a socket (protocol.md section 10). Socket buffers are left to the
/// kernel: on Linux an explicit `SO_RCVBUF`/`SO_SNDBUF` disables autotuning.
// TODO(SR-15, protocol.md 10): raise SO_RCVBUF/SO_SNDBUF to 4 MiB only where the
// tcp_rmem/tcp_wmem maximum is lower; loopback and typical desktops already allow more.
pub(crate) fn tune_socket(tcp: &tokio::net::TcpStream) {
    if let Err(e) = tcp.set_nodelay(true) {
        tracing::debug!(error = %e, "TCP_NODELAY not set");
    }
}

/// Resolves once `rx` holds `true`. Wraps `wait_for` so its borrow guard is not held across
/// an await inside `select!`, which would make the future non-`Send`.
pub(crate) async fn stopped(rx: &mut tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

/// Converts an `h2` error to `io::Error`, keeping it as the inner error so callers can
/// tell a remote reset apart.
pub(crate) fn h2_io(e: h2::Error) -> io::Error {
    if e.is_io() {
        return e
            .into_io()
            .unwrap_or_else(|| io::Error::other("h2 io error"));
    }
    io::Error::other(e)
}

/// True if `e` wraps an `h2` stream reset sent by the peer.
pub fn is_remote_reset(e: &io::Error) -> bool {
    e.get_ref()
        .and_then(|inner| inner.downcast_ref::<h2::Error>())
        .is_some_and(|h| h.is_reset() && h.is_remote())
}

/// An [`AsyncRead`] over an HTTP/2 request or response body.
///
/// Flow-control capacity is released as each DATA frame is handed to the reader, so the
/// peer's window refills at the rate the body is consumed and `h2` sends `WINDOW_UPDATE`
/// at half-window (protocol.md section 10). End of data is end of file; trailers are read
/// afterwards with [`BodyReader::trailers`].
#[derive(Debug)]
pub struct BodyReader {
    recv: RecvStream,
    buf: Bytes,
    received: u64,
}

impl BodyReader {
    /// Wraps a body.
    pub fn new(recv: RecvStream) -> Self {
        Self {
            recv,
            buf: Bytes::new(),
            received: 0,
        }
    }

    /// Body bytes received so far.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Polls for the next DATA frame, releasing its flow-control capacity.
    pub fn poll_chunk(&mut self, cx: &mut Context<'_>) -> Poll<Option<io::Result<Bytes>>> {
        if !self.buf.is_empty() {
            return Poll::Ready(Some(Ok(std::mem::take(&mut self.buf))));
        }
        match ready!(self.recv.poll_data(cx)) {
            None => Poll::Ready(None),
            Some(Err(e)) => Poll::Ready(Some(Err(h2_io(e)))),
            Some(Ok(b)) => {
                // Releasing can only fail once the stream is gone, which the next poll reports.
                let _ = self.recv.flow_control().release_capacity(b.len());
                self.received += b.len() as u64;
                Poll::Ready(Some(Ok(b)))
            }
        }
    }

    /// The next DATA frame, or `None` at end of data.
    pub async fn chunk(&mut self) -> Option<io::Result<Bytes>> {
        std::future::poll_fn(|cx| self.poll_chunk(cx)).await
    }

    /// Reads the trailers after the body (protocol.md 7.4 `pd-root`). Call after end of data.
    pub async fn trailers(&mut self) -> io::Result<Option<HeaderMap>> {
        self.recv.trailers().await.map_err(h2_io)
    }

    /// Reads the rest of the body into memory, failing once it exceeds `limit` bytes
    /// (SR-20: the manifest limit is checked while reading, never after).
    pub async fn read_to_limit(&mut self, limit: usize) -> Result<Bytes, ReadLimitError> {
        let mut out = BytesMut::new();
        while let Some(chunk) = self.chunk().await {
            let chunk = chunk.map_err(ReadLimitError::Io)?;
            if out.len() + chunk.len() > limit {
                return Err(ReadLimitError::TooLarge);
            }
            out.extend_from_slice(&chunk);
        }
        Ok(out.freeze())
    }

    /// The underlying stream.
    pub fn into_inner(self) -> RecvStream {
        self.recv
    }
}

/// Why [`BodyReader::read_to_limit`] stopped.
#[derive(Debug, thiserror::Error)]
pub enum ReadLimitError {
    /// The body exceeded the limit.
    #[error("body too large")]
    TooLarge,
    /// Transport failure.
    #[error("io: {0}")]
    Io(io::Error),
}

impl AsyncRead for BodyReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.buf.is_empty() {
            match ready!(this.poll_chunk(cx)) {
                None => return Poll::Ready(Ok(())),
                Some(Err(e)) => return Poll::Ready(Err(e)),
                Some(Ok(b)) => this.buf = b,
            }
        }
        let n = this.buf.len().min(out.remaining());
        out.put_slice(&this.buf[..n]);
        this.buf.advance(n);
        Poll::Ready(Ok(()))
    }
}

/// Sends `data` on `stream`, waiting for flow-control capacity as needed (protocol.md
/// section 10), and ends the stream if `end` is set.
pub async fn send_all(
    stream: &mut SendStream<Bytes>,
    mut data: Bytes,
    end: bool,
) -> Result<(), h2::Error> {
    while !data.is_empty() {
        stream.reserve_capacity(data.len());
        let granted = match std::future::poll_fn(|cx| stream.poll_capacity(cx)).await {
            Some(Ok(n)) => n,
            Some(Err(e)) => return Err(e),
            // The stream was closed under us; sending surfaces the reason.
            None => return stream.send_data(data, end),
        };
        if granted == 0 {
            continue;
        }
        let piece = data.split_to(granted.min(data.len()));
        let last = data.is_empty() && end;
        stream.send_data(piece, last)?;
        if last {
            return Ok(());
        }
    }
    if end {
        stream.send_data(Bytes::new(), true)?;
    }
    Ok(())
}

/// The 7.1 HTTP error body: `{"error":{"code":...,"message":...,"verifiedOffset":...}}`.
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    /// The error object.
    pub error: ErrorDetail,
}

/// The inner object of [`ErrorBody`].
#[derive(Debug, Clone, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    /// A section 9 code.
    pub code: ErrorCode,
    /// Human-readable detail; never contains secrets (SR-32).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// The receiver's verified offset, for `offset_mismatch` and integrity errors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_offset: Option<u64>,
}

/// An HTTP error reply: status plus the 7.1 body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    /// HTTP status.
    pub status: StatusCode,
    /// The body.
    pub body: ErrorBody,
}

impl HttpError {
    /// An error with `status`, `code` and `message`.
    pub fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                error: ErrorDetail {
                    code,
                    message: message.into(),
                    verified_offset: None,
                },
            },
        }
    }

    /// Adds `verifiedOffset` to the body.
    #[must_use]
    pub fn with_offset(mut self, offset: u64) -> Self {
        self.body.error.verified_offset = Some(offset);
        self
    }

    /// The default status for each section 9 code.
    pub fn from_code(code: ErrorCode, message: impl Into<String>) -> Self {
        let status = match code {
            ErrorCode::BadRequest | ErrorCode::PathInvalid => StatusCode::BAD_REQUEST,
            ErrorCode::BadToken => StatusCode::UNAUTHORIZED,
            ErrorCode::NotPaired | ErrorCode::Forbidden | ErrorCode::Declined => {
                StatusCode::FORBIDDEN
            }
            ErrorCode::NotFound => StatusCode::NOT_FOUND,
            ErrorCode::Conflict | ErrorCode::OffsetMismatch => StatusCode::CONFLICT,
            ErrorCode::Cancelled | ErrorCode::Expired => StatusCode::GONE,
            ErrorCode::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ErrorCode::RangeInvalid => StatusCode::RANGE_NOT_SATISFIABLE,
            ErrorCode::ChunkHashMismatch | ErrorCode::RootMismatch | ErrorCode::BundleMismatch => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            ErrorCode::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::NoSpace => StatusCode::INSUFFICIENT_STORAGE,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(status, code, message)
    }

    /// Serialised body.
    pub fn body_bytes(&self) -> Bytes {
        Bytes::from(serde_json::to_vec(&self.body).unwrap_or_default())
    }
}

impl From<crate::Error> for HttpError {
    /// Maps core errors to section 9 codes. Messages carry no secrets (SR-32).
    fn from(e: crate::Error) -> Self {
        use crate::Error as E;
        match e {
            E::NotPaired => Self::from_code(ErrorCode::NotPaired, ""),
            E::Forbidden(what) => Self::from_code(ErrorCode::Forbidden, what),
            E::BadRequest(m) => Self::from_code(ErrorCode::BadRequest, m),
            E::Json(err) => Self::from_code(ErrorCode::BadRequest, err.to_string()),
            E::PathInvalid(m) => Self::from_code(ErrorCode::PathInvalid, m),
            E::OffsetMismatch { expected } => {
                Self::from_code(ErrorCode::OffsetMismatch, format!("expected {expected}"))
                    .with_offset(expected)
            }
            E::ChunkHashMismatch { offset } => {
                Self::from_code(ErrorCode::ChunkHashMismatch, "").with_offset(offset)
            }
            E::RootMismatch => Self::from_code(ErrorCode::RootMismatch, ""),
            E::BundleMismatch(m) => Self::from_code(ErrorCode::BundleMismatch, m),
            E::RateLimited => Self::from_code(ErrorCode::RateLimited, ""),
            E::NoSpace => Self::from_code(ErrorCode::NoSpace, ""),
            E::TooLarge(m) => Self::from_code(ErrorCode::TooLarge, m),
            E::NotFound => Self::from_code(ErrorCode::NotFound, ""),
            E::BadToken => Self::from_code(ErrorCode::BadToken, ""),
            E::Cancelled => Self::from_code(ErrorCode::Cancelled, ""),
            other => {
                tracing::debug!(error = %other, "internal error answered with 500");
                Self::from_code(ErrorCode::Internal, "")
            }
        }
    }
}

/// Builds a response head with a content type.
pub fn response_head(status: StatusCode, content_type: Option<&'static str>) -> Response<()> {
    let mut r = Response::new(());
    *r.status_mut() = status;
    if let Some(ct) = content_type {
        r.headers_mut()
            .insert(http::header::CONTENT_TYPE, HeaderValue::from_static(ct));
    }
    r
}

/// Sends a complete response: head plus `body` (empty body ends the stream with the head).
pub async fn respond(
    respond: &mut h2::server::SendResponse<Bytes>,
    status: StatusCode,
    content_type: Option<&'static str>,
    body: Bytes,
) -> Result<(), h2::Error> {
    let head = response_head(status, content_type);
    if body.is_empty() {
        respond.send_response(head, true)?;
        return Ok(());
    }
    let mut stream = respond.send_response(head, false)?;
    send_all(&mut stream, body, true).await
}

/// Sends a JSON response.
pub async fn respond_json<T: Serialize>(
    r: &mut h2::server::SendResponse<Bytes>,
    status: StatusCode,
    value: &T,
) -> Result<(), h2::Error> {
    let body = Bytes::from(serde_json::to_vec(value).unwrap_or_default());
    respond(r, status, Some(JSON_CONTENT_TYPE), body).await
}

/// Sends a 7.1 error response.
pub async fn respond_error(
    r: &mut h2::server::SendResponse<Bytes>,
    err: &HttpError,
) -> Result<(), h2::Error> {
    respond(r, err.status, Some(JSON_CONTENT_TYPE), err.body_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::connection::{Direction, KeepAlive};
    use super::*;
    use std::time::Duration;

    #[test]
    fn error_codes_map_to_section_9_statuses() {
        let cases = [
            (ErrorCode::BadToken, 401),
            (ErrorCode::NotPaired, 403),
            (ErrorCode::NotFound, 404),
            (ErrorCode::OffsetMismatch, 409),
            (ErrorCode::Cancelled, 410),
            (ErrorCode::TooLarge, 413),
            (ErrorCode::ChunkHashMismatch, 422),
            (ErrorCode::RateLimited, 429),
            (ErrorCode::NoSpace, 507),
        ];
        for (code, status) in cases {
            assert_eq!(
                HttpError::from_code(code.clone(), "").status.as_u16(),
                status,
                "{code}"
            );
        }
    }

    #[test]
    fn error_body_matches_the_7_1_shape() {
        let e = HttpError::from(crate::Error::OffsetMismatch {
            expected: 4_194_304,
        });
        let v: serde_json::Value = serde_json::from_slice(&e.body_bytes()).unwrap();
        assert_eq!(v["error"]["code"], "offset_mismatch");
        assert_eq!(v["error"]["verifiedOffset"], 4_194_304);
    }

    #[test]
    fn only_the_shorter_interval_pings() {
        let desk = Duration::from_secs(60);
        let mobile = Duration::from_secs(300);
        let ka = KeepAlive {
            local: desk,
            peer: mobile,
        };
        assert!(ka.we_ping(Direction::Inbound));
        let ka = KeepAlive {
            local: mobile,
            peer: desk,
        };
        assert!(!ka.we_ping(Direction::Outbound));
        let ka = KeepAlive {
            local: desk,
            peer: desk,
        };
        assert!(ka.we_ping(Direction::Outbound) && !ka.we_ping(Direction::Inbound));
        assert_eq!(
            KeepAlive {
                local: desk,
                peer: mobile
            }
            .min_idle_close(),
            Duration::from_secs(750)
        );
    }
}
