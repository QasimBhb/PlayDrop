//! The daemon side of the IPC: owns the transport, authenticates callers, forwards requests
//! to a [`RequestHandler`] and fans events out to every connected client.
//!
//! Linux (`linux` module): a `zbus` object at [`crate::DBUS_PATH`] on the session bus owning
//! [`crate::DBUS_NAME`]. Before each call the server resolves the caller's unique name to a
//! UID with `GetConnectionUnixUser` and refuses any UID but its own; it also resolves the
//! PID with `GetConnectionUnixProcessID` and refuses callers whose
//! `/proc/<pid>/root/.flatpak-info` names an app other than ours (SR-26). File sends arrive
//! as fds (`h`) and are resolved to paths through `/proc/self/fd/<n>` before the handler
//! sees a [`crate::Request::SendFiles`].
//!
//! Windows (`windows` module): a named pipe `\\.\pipe\playdrop-<user SID>` created with
//! `FILE_FLAG_FIRST_PIPE_INSTANCE`, `PIPE_REJECT_REMOTE_CLIENTS` and a DACL granting only
//! the user's SID and SYSTEM; each connection's client token SID is checked before the hello
//! is answered (SR-27).
//!
//! Refused callers get a [`crate::Response::Err`] with [`crate::ErrorCode::Unauthorized`]
//! (D-Bus) or a closed pipe (Windows). Transports log request `op` names and error codes
//! only (SR-32).

use std::sync::Arc;

use tokio::sync::broadcast;

use crate::{Event, RequestHandler};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
use linux as imp;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
use self::windows as imp;

#[cfg(not(any(target_os = "linux", windows)))]
mod imp {
    use super::{Arc, Event, RequestHandler, ServeError, ServerOptions};
    use tokio::sync::broadcast;

    pub(super) struct Server;

    impl Server {
        pub(super) async fn shutdown(self) {}
    }

    pub(super) async fn start(
        _handler: Arc<dyn RequestHandler>,
        _events: broadcast::Sender<Event>,
        _options: ServerOptions,
    ) -> Result<Server, ServeError> {
        Err(ServeError::Transport(
            "no IPC transport on this platform".to_owned(),
        ))
    }
}

/// A running IPC server. Dropping it (or calling [`Server::shutdown`]) releases the bus name
/// or pipe.
pub struct Server {
    inner: imp::Server,
}

/// Why the server could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// Another daemon already owns the bus name or the pipe (single instance).
    #[error("another PlayDrop daemon is already running")]
    AlreadyRunning,
    /// Transport failure.
    #[error("ipc transport: {0}")]
    Transport(String),
}

/// Knobs for [`Server::start_with`]. The daemon uses [`Server::start`], which takes the
/// defaults.
#[derive(Debug, Clone, Default)]
pub struct ServerOptions {
    /// Linux: D-Bus address to serve on instead of the session bus (tests run a private
    /// `dbus-daemon`). Ignored on Windows.
    pub bus_address: Option<String>,
    /// Version string the daemon reports to clients. Defaults to this crate's version, which
    /// is the workspace version the daemon is built with.
    pub daemon_version: Option<String>,
}

impl ServerOptions {
    pub(crate) fn daemon_version(&self) -> String {
        self.daemon_version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned())
    }
}

impl Server {
    /// Start serving. `events` is the broadcast channel the daemon publishes [`Event`]s on;
    /// every connected client receives every event sent after it connected.
    ///
    /// Returns [`ServeError::AlreadyRunning`] when the name or pipe is taken, which the
    /// daemon uses as its single-instance check.
    pub async fn start(
        handler: Arc<dyn RequestHandler>,
        events: broadcast::Sender<Event>,
    ) -> Result<Self, ServeError> {
        Self::start_with(handler, events, ServerOptions::default()).await
    }

    /// [`Server::start`] with explicit options.
    pub async fn start_with(
        handler: Arc<dyn RequestHandler>,
        events: broadcast::Sender<Event>,
        options: ServerOptions,
    ) -> Result<Self, ServeError> {
        let inner = imp::start(handler, events, options).await?;
        Ok(Self { inner })
    }

    /// Stop serving and release the bus name or pipe.
    pub async fn shutdown(self) {
        self.inner.shutdown().await;
    }
}
