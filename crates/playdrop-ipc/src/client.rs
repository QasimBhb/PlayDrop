//! The UI side of the IPC: connect to the daemon, verify it, send requests, stream events.
//!
//! Linux: before sending the daemon anything, the client asks the bus for the owner of
//! [`crate::DBUS_NAME`], checks the owner's UID is its own and `/proc/<pid>/exe` is a
//! `playdropd` binary (SR-26), and from then on addresses that verified unique name only, so
//! a process that grabs the name later is never talked to. [`Client::send_files`] opens each
//! path (`O_RDONLY` for files, `O_PATH | O_DIRECTORY` for folders) and passes the fds, never
//! the paths.
//!
//! Starting the daemon: when nobody owns the name, the client asks the bus to start it
//! (`StartServiceByName`). The bus does that only if the `org.playdrop.Daemon1.service`
//! file installed by `packaging/` exists, which is how opening the window starts the daemon;
//! without the file the client reports [`ConnectError::NotRunning`]. The client itself never
//! spawns a process.
//!
//! Windows: connects to `\\.\pipe\playdrop-<own SID>` and checks the server's SID and image
//! path before sending the hello (SR-27).
//!
//! A client is `Send + Sync`; share it behind an `Arc`. If the daemon exits, requests fail
//! with [`crate::ErrorCode::Internal`] and the event stream ends; connect again.

use std::path::PathBuf;

use tokio_stream::wrappers::BroadcastStream;

use crate::{ErrorCode, Event, FilesOp, IpcError, Ok, Request, MAX_SEND_ITEMS};

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
    use super::{BroadcastStream, ClientOptions, ConnectError, Event, IpcError, Ok, PathBuf};
    use crate::{FilesOp, Request};

    pub(super) struct Client;

    impl Client {
        pub(super) async fn connect(_options: ClientOptions) -> Result<Self, ConnectError> {
            Err(ConnectError::Transport(
                "no IPC transport on this platform".to_owned(),
            ))
        }
        pub(super) fn daemon_version(&self) -> &str {
            ""
        }
        pub(super) async fn request(&self, _request: &Request) -> Result<Ok, IpcError> {
            unreachable!("a client cannot be constructed on this platform")
        }
        pub(super) async fn files(
            &self,
            _op: FilesOp,
            _peer: &str,
            _paths: &[PathBuf],
        ) -> Result<Ok, IpcError> {
            unreachable!("a client cannot be constructed on this platform")
        }
        pub(super) fn events(&self) -> BroadcastStream<Event> {
            unreachable!("a client cannot be constructed on this platform")
        }
    }
}

/// A connection to the daemon.
pub struct Client {
    inner: imp::Client,
}

/// Why connecting failed.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// No daemon owns the name or pipe. The UI offers to start one (D-Bus activation on
    /// Linux starts it automatically; the UI may also spawn `playdropd`).
    #[error("PlayDrop daemon is not running")]
    NotRunning,
    /// The name owner is not our user or not a `playdropd` binary (SR-26, SR-27).
    #[error("refusing to talk to an unverified daemon: {0}")]
    Unverified(String),
    /// The daemon speaks another IPC version.
    #[error("daemon IPC version {daemon} does not match client {client}")]
    Version {
        /// Daemon's version.
        daemon: u32,
        /// Ours.
        client: u32,
    },
    /// Transport failure.
    #[error("ipc transport: {0}")]
    Transport(String),
}

/// Knobs for [`Client::connect_with`]. UIs use [`Client::connect`], which takes the
/// defaults.
#[derive(Debug, Clone)]
pub struct ClientOptions {
    /// Check that the daemon process is a `playdropd` binary (`/proc/<pid>/exe` on Linux,
    /// the image path on Windows). Default `true`. Only tests, whose server runs inside the
    /// test binary, set it to `false`; production callers never do. The UID or SID check
    /// always runs. A daemon started with `cargo run -p playdropd` passes, since its binary
    /// is `target/<profile>/playdropd`.
    pub verify_exe: bool,
    /// Linux: D-Bus address to use instead of the session bus (tests run a private
    /// `dbus-daemon`). Ignored on Windows.
    pub bus_address: Option<String>,
}

/// Whether a connect may ask the bus to start the daemon (Linux D-Bus activation).
///
/// [`Client::connect`] and [`Client::connect_with`] use [`Activation::Allow`], which is how
/// opening the window starts the daemon. A client that reconnects on its own, such as a panel
/// applet, uses [`Activation::Never`] so it never starts a daemon the user quit or chose not
/// to start at login. Windows has no activation; the value changes nothing there.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Activation {
    /// Ask the bus to start the daemon when nobody owns its name.
    #[default]
    Allow,
    /// Only connect to a daemon that is already running; otherwise
    /// [`ConnectError::NotRunning`].
    Never,
}

impl Default for ClientOptions {
    fn default() -> Self {
        Self {
            verify_exe: true,
            bus_address: None,
        }
    }
}

impl Client {
    /// Connect and verify the daemon.
    pub async fn connect() -> Result<Self, ConnectError> {
        Self::connect_with(ClientOptions::default()).await
    }

    /// [`Client::connect`] with explicit options.
    pub async fn connect_with(options: ClientOptions) -> Result<Self, ConnectError> {
        Self::connect_with_activation(options, Activation::Allow).await
    }

    /// Connect only to a daemon that is already running: never asks the bus to start one.
    /// Answers [`ConnectError::NotRunning`] when nobody owns the name.
    pub async fn connect_without_activation() -> Result<Self, ConnectError> {
        Self::connect_with_activation(ClientOptions::default(), Activation::Never).await
    }

    /// [`Client::connect_with`] with an explicit [`Activation`] choice.
    pub async fn connect_with_activation(
        options: ClientOptions,
        activation: Activation,
    ) -> Result<Self, ConnectError> {
        #[cfg(target_os = "linux")]
        let inner = imp::Client::connect(options, activation).await?;
        #[cfg(not(target_os = "linux"))]
        let inner = {
            let _ = activation;
            imp::Client::connect(options).await?
        };
        Result::Ok(Self { inner })
    }

    /// Waits until some process owns the daemon's D-Bus name, without starting it. A thin
    /// wrapper over [`DaemonWatch`] for one-off use: it opens a bus connection per call, so a
    /// UI that waits repeatedly keeps one [`DaemonWatch`] instead.
    ///
    /// Linux only. Elsewhere it never resolves, so it can sit in a `select!` beside a poll.
    pub async fn wait_for_daemon(options: &ClientOptions) -> Result<(), ConnectError> {
        DaemonWatch::new(options).await?.appeared().await
    }

    /// The daemon's package version, as it reported on connect.
    pub fn daemon_version(&self) -> &str {
        self.inner.daemon_version()
    }

    /// Send one request and wait for its answer. A transport failure is reported as an
    /// [`IpcError`] with [`crate::ErrorCode::Internal`].
    ///
    /// A [`Request::SendFiles`] or [`Request::QueueOutbox`] is routed through
    /// [`Client::send_files`] or [`Client::queue_outbox`], so on Linux it travels as file
    /// descriptors like any other send.
    pub async fn request(&self, request: Request) -> Result<Ok, IpcError> {
        match request {
            Request::SendFiles { peer, paths } => self.send_files(&peer, paths).await,
            Request::QueueOutbox { peer, paths } => self.queue_outbox(&peer, paths).await,
            other => self.inner.request(&other).await,
        }
    }

    /// Send files or folders. Opens the paths and passes descriptors on Linux (SR-26);
    /// passes paths on Windows (SR-27). Answers [`Ok::TransferStarted`].
    ///
    /// At most [`MAX_SEND_ITEMS`] items per call; more answer [`ErrorCode::TooLarge`].
    pub async fn send_files(&self, peer: &str, paths: Vec<PathBuf>) -> Result<Ok, IpcError> {
        self.files(FilesOp::Send, peer, &paths).await
    }

    /// Queue files or folders for `peer` to pull later (protocol.md 7.8). Travels exactly
    /// like [`Client::send_files`] and answers [`Ok::OutboxQueued`].
    pub async fn queue_outbox(&self, peer: &str, paths: Vec<PathBuf>) -> Result<Ok, IpcError> {
        self.files(FilesOp::QueueOutbox, peer, &paths).await
    }

    async fn files(&self, op: FilesOp, peer: &str, paths: &[PathBuf]) -> Result<Ok, IpcError> {
        if paths.is_empty() {
            return Err(IpcError::new(ErrorCode::BadRequest, "nothing to send"));
        }
        if paths.len() > MAX_SEND_ITEMS {
            return Err(IpcError::new(
                ErrorCode::TooLarge,
                format!("send at most {MAX_SEND_ITEMS} items at once, or their folder"),
            ));
        }
        self.inner.files(op, peer, paths).await
    }

    /// Events from the daemon, from the moment of the call. A slow consumer that falls
    /// behind sees a lag error item, never a stalled daemon. The stream ends when the
    /// daemon goes away.
    pub fn events(&self) -> BroadcastStream<Event> {
        self.inner.events()
    }
}

/// Watches the session bus for the daemon's name gaining an owner, without ever starting the
/// daemon, so a UI that is not connected learns at once that the daemon came up (a restart,
/// or a start from the window, the CLI or `systemctl --user`).
///
/// It keeps one bus connection and one `NameOwnerChanged` subscription for its whole life,
/// so it can be awaited again and again (between back-off polls) at no extra cost. The owner
/// is not verified here: connect afterwards, which runs the SR-26 checks.
///
/// Linux only. Elsewhere [`DaemonWatch::new`] succeeds and [`DaemonWatch::appeared`] never
/// resolves, so it can sit in a `select!` beside a poll.
pub struct DaemonWatch {
    #[cfg(target_os = "linux")]
    inner: linux::Watch,
}

impl std::fmt::Debug for DaemonWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonWatch").finish_non_exhaustive()
    }
}

impl DaemonWatch {
    /// Connects to the session bus (or `options.bus_address`) and subscribes.
    pub async fn new(options: &ClientOptions) -> Result<Self, ConnectError> {
        #[cfg(target_os = "linux")]
        {
            Result::Ok(Self {
                inner: linux::Watch::new(options).await?,
            })
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Nothing to connect to here. The await keeps this an async function with the
            // same signature on every platform.
            let _ = options;
            std::future::ready(()).await;
            Result::Ok(Self {})
        }
    }

    /// Returns as soon as the daemon's name has an owner, at once when it already has one.
    /// Changes that happened while nobody was waiting are not replayed: only the name's state
    /// now and later counts. An error means the bus connection is gone; make a new watch.
    pub async fn appeared(&mut self) -> Result<(), ConnectError> {
        #[cfg(target_os = "linux")]
        {
            self.inner.appeared().await
        }
        #[cfg(not(target_os = "linux"))]
        {
            std::future::pending::<Result<(), ConnectError>>().await
        }
    }
}

/// Whether a Windows image path names `playdropd.exe` (SR-27). Pure, so it is tested on
/// every platform.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn is_daemon_image(path: &str) -> bool {
    const SUFFIX: &str = "\\playdropd.exe";
    path.len() > SUFFIX.len()
        && path
            .get(path.len() - SUFFIX.len()..)
            .is_some_and(|tail| tail.eq_ignore_ascii_case(SUFFIX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_image_names() {
        assert!(is_daemon_image(r"C:\Program Files\PlayDrop\playdropd.exe"));
        assert!(is_daemon_image(r"C:\Program Files\PlayDrop\PlayDropD.EXE"));
        assert!(!is_daemon_image(r"C:\Temp\evil-playdropd.exe"));
        assert!(!is_daemon_image(r"C:\Temp\playdropd.exe.bak"));
        assert!(!is_daemon_image("playdropd.exe"));
    }

    #[test]
    fn activation_defaults_to_allow_so_existing_callers_keep_starting_the_daemon() {
        assert_eq!(Activation::default(), Activation::Allow);
    }

    #[test]
    fn client_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Client>();
        assert_send_sync::<crate::server::Server>();
    }
}
