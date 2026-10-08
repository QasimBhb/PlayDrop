//! Request routing for the host's server (protocol.md 7.1). The server has already applied
//! the SR-13 gate: unpaired connections reach only `POST /v1/session`.

use std::sync::Arc;

use bytes::Bytes;
use h2::server::SendResponse;
use h2::RecvStream;
use http::{Method, Request, StatusCode};

use super::receive::{self, Target};
use super::{session, Inner};
use crate::net::connection::DUPLICATE_PING_DEADLINE;
use crate::net::server::{HandlerFuture, SESSION_PATH};
use crate::net::{
    respond_error, respond_json, CloseReason, Connection, Direction, HttpError, Router,
};
use crate::session::ErrorCode;

/// The host's [`Router`].
pub(crate) struct HostRouter {
    pub(crate) inner: Arc<Inner>,
}

impl Router for HostRouter {
    fn connection_opened(&self, conn: &Arc<Connection>) {
        self.inner.registry.track(conn);
        if !conn.is_trusted() {
            return;
        }
        let other = self.inner.registry.attach(conn);
        // protocol.md 3.3: the lower device getting an inbound while holding an outbound
        // PINGs the outbound; no ACK in 2 s means it keeps the inbound instead.
        if let Some(out) = other.filter(|o| o.direction() == Direction::Outbound) {
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
        // TODO(Phase 3/4): the outbox (protocol.md 7.8). Nothing is ever queued yet, so the
        // list is empty and every item is unknown.
        (&Method::GET, ["v1", "outbox"]) => {
            let _ = respond_json(
                &mut respond,
                StatusCode::OK,
                &serde_json::json!({ "items": [] }),
            )
            .await;
        }
        // TODO(Phase 2/3): blobs (protocol.md 5.15) for long clipboard text and images.
        _ => not_found(&mut respond).await,
    }
}

async fn not_found(respond: &mut SendResponse<Bytes>) {
    let _ = respond_error(respond, &HttpError::from_code(ErrorCode::NotFound, "")).await;
}
