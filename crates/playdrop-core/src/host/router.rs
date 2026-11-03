//! Request routing for the host's server (protocol.md 5.15, 7.1, 7.8). The server has already applied
//! the SR-13 gate: unpaired connections reach only `POST /v1/session`.

use std::sync::Arc;

use bytes::Bytes;
use h2::server::SendResponse;
use h2::RecvStream;
use http::{Method, Request};

use super::receive::{self, Target};
use super::{blobs, connect, outbox, session, Inner};
use crate::net::connection::DUPLICATE_PING_DEADLINE;
use crate::net::server::{HandlerFuture, SESSION_PATH};
use crate::net::{respond_error, CloseReason, Connection, Direction, HttpError, Router};
use crate::session::ErrorCode;

/// The host's [`Router`].
pub(crate) struct HostRouter {
    pub(crate) inner: Arc<Inner>,
}

impl Router for HostRouter {
    fn connection_opened(&self, conn: &Arc<Connection>) {
        // Tracked from the handshake, so shutdown and unpair close it (SR-14) even if it
        // never carries a request. A connection that finished TLS before an unpair can get
        // here after it: unpair removes the pin before it lists the tracked connections, so
        // tracking first and then checking the pin leaves no gap.
        self.inner.registry.track(conn);
        if conn.is_trusted() && !self.inner.pins.contains(&conn.device_id()) {
            conn.close(CloseReason::Unpaired);
        }
    }

    /// Counts for protocol.md 3.3 only from its first request other than a path probe
    /// (3.5), so a connection the peer holds for probing never replaces the one in use.
    fn connection_in_use(&self, conn: &Arc<Connection>) {
        if !conn.is_trusted() {
            return;
        }
        let attached = self.inner.registry.attach(conn);
        if let Some(old) = attached.replaced {
            connect::close_if_silent(&self.inner, old);
        }
        // protocol.md 3.3: the lower device getting an inbound while holding an outbound
        // PINGs the outbound; no ACK in 2 s means it keeps the inbound instead.
        if let Some(out) = attached
            .other
            .filter(|o| o.direction() == Direction::Outbound)
        {
            if self.inner.local_id < conn.device_id() {
                self.inner.spawn(async move {
                    if out.ping(DUPLICATE_PING_DEADLINE).await.is_err() {
                        out.close(CloseReason::Dead);
                    }
                });
            }
        }
    }

    fn handle(
        &self,
        conn: Arc<Connection>,
        req: Request<RecvStream>,
        respond: SendResponse<Bytes>,
    ) -> HandlerFuture {
        let inner = Arc::clone(&self.inner);
        Box::pin(self.inner.guard_task(route(inner, conn, req, respond)))
    }

    fn connection_closed(&self, conn: &Arc<Connection>) {
        self.inner.registry.detach(conn);
    }
}

async fn route(
    inner: Arc<Inner>,
    conn: Arc<Connection>,
    req: Request<RecvStream>,
    mut respond: SendResponse<Bytes>,
) {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (&method, parts.as_slice()) {
        (&Method::POST, _) if path == SESSION_PATH => {
            session::accept_inbound(inner, conn, req, respond).await;
        }
        (&Method::POST, ["v1", "transfers"]) => receive::offer(inner, conn, req, respond).await,
        (&Method::PUT, ["v1", "transfers", id, "files", fid]) => match fid.parse::<u32>() {
            Ok(fid) => {
                let id = (*id).to_owned();
                receive::put(inner, conn, req, respond, id, Target::File(fid)).await;
            }
            Err(_) => not_found(&mut respond).await,
        },
        (&Method::PUT, ["v1", "transfers", id, "bundle"]) => {
            let id = (*id).to_owned();
            receive::put(inner, conn, req, respond, id, Target::Bundle).await;
        }
        (&Method::GET, ["v1", "transfers", id]) => {
            let id = (*id).to_owned();
            receive::get_state(&inner, &conn, &mut respond, &id).await;
        }
        (&Method::DELETE, ["v1", "transfers", id]) => {
            let id = (*id).to_owned();
            receive::cancel(inner, conn, req, respond, id).await;
        }
        (&Method::GET, ["v1", "blobs", id]) => {
            blobs::serve(&inner, &conn, &mut respond, id).await;
        }
        (&Method::GET, ["v1", "outbox"]) => {
            outbox::serve_list(&inner, &conn, &mut respond).await;
        }
        (&Method::GET, ["v1", "outbox", id]) => {
            outbox::serve_manifest(&inner, &conn, &mut respond, id).await;
        }
        (&Method::GET, ["v1", "outbox", id, "files", fid]) => match fid.parse::<u32>() {
            Ok(fid) => {
                let id = (*id).to_owned();
                outbox::serve_stream(inner, conn, req, respond, id, Target::File(fid)).await;
            }
            Err(_) => not_found(&mut respond).await,
        },
        (&Method::GET, ["v1", "outbox", id, "bundle"]) => {
            let id = (*id).to_owned();
            outbox::serve_stream(inner, conn, req, respond, id, Target::Bundle).await;
        }
        (&Method::DELETE, ["v1", "outbox", id]) => {
            outbox::serve_delete(&inner, &conn, &mut respond, id).await;
        }
        _ => not_found(&mut respond).await,
    }
}

async fn not_found(respond: &mut SendResponse<Bytes>) {
    let _ = respond_error(respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
}
