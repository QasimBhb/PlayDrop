//! Named pipe server (`docs/ipc.md` section 3, SR-27). Untested: written on Linux, where
//! Windows code does not compile.
//!
//! The first instance is created with `FILE_FLAG_FIRST_PIPE_INSTANCE`, which is the
//! single-instance check. Every instance rejects remote clients and carries a DACL for the
//! user's SID and `SYSTEM` only. Each accepted connection's client token SID must equal
//! ours before the hello is answered; the line protocol itself is `crate::pipe`.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::windows::named_pipe::{NamedPipeServer, PipeMode, ServerOptions as PipeOptions};
use tokio::sync::broadcast;
use tokio::task::JoinSet;

use super::{ServeError, ServerOptions};
use crate::pipe::serve_connection;
use crate::util::AbortOnDrop;
use crate::win::{
    current_user_sid, pipe_client_pid, process_user_sid, SecurityDescriptor, ERROR_ACCESS_DENIED,
};
use crate::{Event, RequestHandler, PIPE_PREFIX};

/// Pause before retrying when creating the next pipe instance fails.
const RECREATE_BACKOFF: Duration = Duration::from_secs(1);

/// The running pipe server.
pub(super) struct Server {
    _accept: AbortOnDrop,
}

impl Server {
    pub(super) async fn shutdown(self) {
        // Aborting the accept task drops the listening instance and every connection.
    }
}

/// What every connection needs.
struct Shared {
    name: String,
    descriptor: SecurityDescriptor,
    own_sid: String,
    handler: Arc<dyn RequestHandler>,
    events: broadcast::Sender<Event>,
    daemon_version: String,
}

#[allow(unsafe_code)]
fn create_instance(
    name: &str,
    descriptor: &SecurityDescriptor,
    first: bool,
) -> std::io::Result<NamedPipeServer> {
    let mut attributes = descriptor.attributes();
    let mut options = PipeOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .pipe_mode(PipeMode::Byte);
    // SAFETY: `attributes` is a valid SECURITY_ATTRIBUTES whose descriptor is owned by
    // `descriptor`, which outlives this call; CreateNamedPipeW only reads it.
    unsafe {
        options
            .create_with_security_attributes_raw(name, std::ptr::from_mut(&mut attributes).cast())
    }
}

pub(super) async fn start(
    handler: Arc<dyn RequestHandler>,
    events: broadcast::Sender<Event>,
    options: ServerOptions,
) -> Result<Server, ServeError> {
    let own_sid = current_user_sid().map_err(|e| ServeError::Transport(e.to_string()))?;
    let name = format!("{PIPE_PREFIX}{own_sid}");
    let descriptor = SecurityDescriptor::user_and_system(&own_sid)
        .map_err(|e| ServeError::Transport(e.to_string()))?;
    let first = create_instance(&name, &descriptor, true).map_err(|e| {
        if e.raw_os_error() == Some(ERROR_ACCESS_DENIED) {
            ServeError::AlreadyRunning
        } else {
            ServeError::Transport(e.to_string())
        }
    })?;
    tracing::info!("ipc server on the per-user named pipe");
    let shared = Arc::new(Shared {
        name,
        descriptor,
        own_sid,
        handler,
        events,
        daemon_version: options.daemon_version(),
    });
    let task = tokio::spawn(accept_loop(first, shared));
    Ok(Server {
        _accept: AbortOnDrop::new(task),
    })
}

/// Accept clients forever. The connected instance is handed to a task and a fresh instance
/// takes its place, so another client can connect meanwhile.
async fn accept_loop(mut listening: NamedPipeServer, shared: Arc<Shared>) {
    let mut connections = JoinSet::new();
    loop {
        while connections.try_join_next().is_some() {}
        let connected = listening.connect().await;
        // Subscribe at accept time so the client sees every event from now on.
        let events = shared.events.subscribe();
        let next = loop {
            match create_instance(&shared.name, &shared.descriptor, false) {
                Ok(next) => break next,
                Err(e) => {
                    tracing::error!(error = %e, "cannot create the next pipe instance");
                    tokio::time::sleep(RECREATE_BACKOFF).await;
                }
            }
        };
        let pipe = std::mem::replace(&mut listening, next);
        match connected {
            Ok(()) => {
                connections.spawn(handle_connection(pipe, events, Arc::clone(&shared)));
            }
            Err(e) => tracing::debug!(error = %e, "a pipe client vanished before connecting"),
        }
    }
}

async fn handle_connection(
    pipe: NamedPipeServer,
    events: broadcast::Receiver<Event>,
    shared: Arc<Shared>,
) {
    // SR-27: the client process must run as our user.
    let verdict = pipe_client_pid(&pipe).and_then(process_user_sid);
    match verdict {
        Ok(sid) if sid == shared.own_sid => {}
        Ok(_) => {
            tracing::warn!("ipc client refused: another user");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "ipc client refused: identity unreadable");
            return;
        }
    }
    if let Err(e) = serve_connection(
        pipe,
        Arc::clone(&shared.handler),
        events,
        &shared.daemon_version,
    )
    .await
    {
        tracing::debug!(error = %e, "ipc pipe connection ended");
    }
}
