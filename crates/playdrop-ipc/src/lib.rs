//! Local IPC between `playdropd` and its UI clients.
//!
//! The daemon owns the listening socket and the `playdrop_core::host::Host`; every UI (the
//! Slint window, the tray, a future CLI) is a thin client that talks to the daemon through
//! this crate and never to the network (PLAN.md "Architecture", `docs/ipc.md`).
//!
//! Layout:
//! - [`Request`], [`Response`], [`Event`] and their payloads: the transport-independent
//!   contract, JSON on the wire on both platforms.
//! - [`server`]: what the daemon runs. D-Bus on Linux (`org.playdrop.PlayDrop.Daemon1`, SR-26),
//!   a per-user named pipe on Windows (SR-27). Both authenticate the caller before any
//!   request reaches the daemon.
//! - [`client`]: what a UI uses. Opens the transport, verifies the daemon's identity
//!   (SR-26, SR-27), sends requests, streams events.
//!
//! Secrets (`Secret<T>` inside events) are serialised for the UI but never logged (SR-32).

#![forbid(clippy::unwrap_used)]

pub mod client;
pub mod server;
pub mod wire;

#[cfg(target_os = "linux")]
mod linux;
mod pipe;
mod util;
#[cfg(windows)]
mod win;

use std::net::SocketAddr;
use std::path::PathBuf;

use playdrop_core::host::{
    AddDeviceCandidate, Diagnostics, HostEvent, OutboxItem, PeerSummary, TransferSnapshot,
};
use playdrop_core::pin_store::Grants;
use serde::{Deserialize, Serialize};

/// IPC contract version. Bumped on any change a client could misread; the daemon refuses
/// clients that ask for a different major version.
pub const IPC_VERSION: u32 = 1;

/// Linux: the D-Bus well-known name the daemon owns on the session bus. Prefixed with the
/// Flatpak app ID `org.playdrop.PlayDrop` so the sandbox can export its activation file.
pub const DBUS_NAME: &str = "org.playdrop.PlayDrop.Daemon1";
/// Linux: the D-Bus object path.
pub const DBUS_PATH: &str = "/org/playdrop/PlayDrop/Daemon1";
/// Linux: the D-Bus interface name.
pub const DBUS_INTERFACE: &str = "org.playdrop.PlayDrop.Daemon1";
/// Windows: pipe name prefix; the daemon appends the user's SID (SR-27).
pub const PIPE_PREFIX: &str = r"\\.\pipe\playdrop-";

/// Linux: the Flatpak application ID of `PlayDrop`. A caller running inside a Flatpak sandbox
/// must carry this ID in its `/proc/<pid>/root/.flatpak-info`, or the daemon refuses it (SR-26).
pub const FLATPAK_APP_ID: &str = "org.playdrop.PlayDrop";

/// Most files or folders one [`Request::SendFiles`] or [`Request::QueueOutbox`] may carry. On Linux each item travels as a
/// file descriptor and the kernel passes at most 253 descriptors in one message
/// (`SCM_MAX_FD`), so the client refuses more with [`ErrorCode::TooLarge`]. Sending a folder
/// counts as one item however many files it holds.
pub const MAX_SEND_ITEMS: usize = 250;

/// What a request that carries files does with them. On Linux both travel as file
/// descriptors through their own D-Bus method (SR-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilesOp {
    /// [`Request::SendFiles`], D-Bus `SendFiles`.
    Send,
    /// [`Request::QueueOutbox`], D-Bus `QueueOutboxFiles`.
    QueueOutbox,
}

impl FilesOp {
    /// The request the daemon handles once the files are paths again.
    pub(crate) fn request(self, peer: String, paths: Vec<PathBuf>) -> Request {
        match self {
            Self::Send => Request::SendFiles { peer, paths },
            Self::QueueOutbox => Request::QueueOutbox { peer, paths },
        }
    }
}

/// Largest JSON message either side accepts, in bytes. Requests are small; the biggest
/// responses are diagnostics and transfer lists.
pub const MAX_MESSAGE_BYTES: usize = 4 * 1024 * 1024;

/// A request from a UI client to the daemon. One variant per daemon capability; the daemon
/// maps most of them onto one `Host` method (`docs/phases/phase-1-core.md` section 3.3).
///
/// Peers are hex Device IDs, Short IDs or labels, resolved with `Host::resolve_peer`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Request {
    /// Daemon identity, versions and settings.
    Status,
    /// Paired devices.
    Peers,
    /// Every transfer the daemon still remembers (active and recently finished).
    Transfers,
    /// Network self-check: interfaces, multicast, port and per-peer last failure.
    Diagnose,
    /// Dial a peer's last-known addresses now.
    Connect {
        /// Peer query.
        peer: String,
    },
    /// Change a peer's grants. Only a local UI may do this (SR-14); the transport enforces
    /// that the caller is the same user.
    SetGrants {
        /// Peer query.
        peer: String,
        /// New grants.
        grants: Grants,
    },
    /// Change the local label of a peer (SR-8).
    SetLabel {
        /// Peer query.
        peer: String,
        /// New label; sanitised by the core.
        label: String,
    },
    /// Forget a peer.
    Unpair {
        /// Peer query.
        peer: String,
    },
    /// Open the 120 s "Add device" window (SR-6). Refused while the desktop is locked.
    OpenAddDeviceWindow,
    /// Close the window and drop any QR offer.
    CloseAddDeviceWindow,
    /// Devices seen in their own "Add device" window.
    AddDeviceList,
    /// Start SAS pairing with a device from the add-device list or a typed address.
    PairWith {
        /// Address to dial.
        addr: SocketAddr,
    },
    /// Answer the current pairing prompt.
    PairingRespond {
        /// Accept or reject.
        accept: bool,
    },
    /// Open the window and return a one-use QR URI (SR-9). Never log the result.
    ShowQr,
    /// Pair as the QR scanner.
    ScanQr {
        /// The scanned URI.
        uri: String,
    },
    /// Send files or folders to a peer.
    ///
    /// On Linux a client never sends this variant directly: [`client::Client::send_files`]
    /// opens the paths and passes file descriptors over D-Bus (SR-26), and the server turns
    /// them back into paths before the daemon sees the request. On Windows paths travel as is,
    /// protected by the pipe's SID check (SR-27).
    SendFiles {
        /// Peer query.
        peer: String,
        /// Absolute paths.
        paths: Vec<PathBuf>,
    },
    /// Answer an incoming transfer offer.
    RespondOffer {
        /// Transfer ID.
        id: String,
        /// Accept or decline.
        accept: bool,
    },
    /// Cancel a transfer in either direction.
    CancelTransfer {
        /// Transfer ID.
        id: String,
    },
    /// Ping a peer.
    Ping {
        /// Peer query.
        peer: String,
        /// Optional message, cut to 256 bytes by the core.
        message: Option<String>,
    },
    /// Queue files or folders for a peer to pull later (protocol.md 7.8): the outbox an
    /// iPhone, which cannot receive in the background, pulls from when it opens `PlayDrop`.
    ///
    /// Travels like [`Request::SendFiles`]: on Linux [`client::Client::queue_outbox`] passes
    /// file descriptors through the `QueueOutboxFiles` method (SR-26) and paths are refused
    /// over the bus; on Windows paths travel as is (SR-27). Answers [`Ok::OutboxQueued`].
    QueueOutbox {
        /// Peer query.
        peer: String,
        /// Absolute paths.
        paths: Vec<PathBuf>,
    },
    /// Items this daemon holds for peers. Answers [`Ok::Outbox`].
    Outbox,
    /// Drop a held outbox item.
    RemoveOutbox {
        /// Item ID from [`Request::Outbox`] or [`Ok::OutboxQueued`].
        id: String,
    },
    /// Pull every item a peer holds for us (protocol.md 7.8). Answers [`Ok::OutboxPulled`];
    /// progress and completion arrive as receive-side transfer events.
    PullOutbox {
        /// Peer query.
        peer: String,
    },
    /// Push clipboard text to every online peer with `clipboardOut` (protocol.md 5.7).
    PushClipboard {
        /// Text, at most 1 MiB; over 32 KiB it travels as a blob (protocol.md 5.15).
        text: String,
        /// Password-manager clip sent by hand (SR-24).
        sensitive: bool,
    },
    /// Push clipboard text to one peer.
    SendClipboard {
        /// Peer query.
        peer: String,
        /// Text.
        text: String,
        /// SR-24 flag.
        sensitive: bool,
    },
    /// Read the daemon settings.
    GetSettings,
    /// Change daemon settings. Fields left `None` keep their value. Changing `receive_dir`
    /// or `device_name` restarts the host; the response says so.
    SetSettings(SettingsPatch),
    /// Open a received file with the default application. Executables, scripts, `.desktop`,
    /// `.lnk` and `.msi` open their folder instead (SR-23). The daemon only opens paths
    /// under its receive folder.
    OpenReceived {
        /// Path reported in a `TransferComplete` event.
        path: PathBuf,
    },
    /// Reveal a received file in the file manager (SR-23 rule as above).
    ShowInFolder {
        /// Path reported in a `TransferComplete` event.
        path: PathBuf,
    },
    /// Tell the daemon the OS reported a network change (interfaces up or down, Wi-Fi switch).
    NetworkChanged,
    /// End every remote input session this desktop is the target of, now (the SR-25 kill
    /// switch; the tray has the same action). Answers `done` whether or not one was open.
    StopInput,
    /// Ask a peer to control one of its media players (protocol.md 5.9). Needs the peer
    /// online with `media.control` in its capabilities; the peer applies it only if it
    /// granted us `media`, and answers a refusal that arrives as a `PeerRefused` host event.
    MediaCommand {
        /// Peer query.
        peer: String,
        /// Player ID from the peer's `MediaState` event.
        player: String,
        /// What to do.
        action: playdrop_core::session::types::MediaAction,
        /// `seek`: absolute position in ms; `volume`: 0 to 100; otherwise `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<u64>,
    },
    /// Ask the daemon to exit cleanly.
    Shutdown,
}

impl Request {
    /// The `op` tag of this request, as it appears on the wire. Transports log this and
    /// nothing else about a request (SR-32).
    pub fn op(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Peers => "peers",
            Self::Transfers => "transfers",
            Self::Diagnose => "diagnose",
            Self::Connect { .. } => "connect",
            Self::SetGrants { .. } => "set_grants",
            Self::SetLabel { .. } => "set_label",
            Self::Unpair { .. } => "unpair",
            Self::OpenAddDeviceWindow => "open_add_device_window",
            Self::CloseAddDeviceWindow => "close_add_device_window",
            Self::AddDeviceList => "add_device_list",
            Self::PairWith { .. } => "pair_with",
            Self::PairingRespond { .. } => "pairing_respond",
            Self::ShowQr => "show_qr",
            Self::ScanQr { .. } => "scan_qr",
            Self::SendFiles { .. } => "send_files",
            Self::QueueOutbox { .. } => "queue_outbox",
            Self::Outbox => "outbox",
            Self::RemoveOutbox { .. } => "remove_outbox",
            Self::PullOutbox { .. } => "pull_outbox",
            Self::RespondOffer { .. } => "respond_offer",
            Self::CancelTransfer { .. } => "cancel_transfer",
            Self::Ping { .. } => "ping",
            Self::PushClipboard { .. } => "push_clipboard",
            Self::SendClipboard { .. } => "send_clipboard",
            Self::GetSettings => "get_settings",
            Self::SetSettings(_) => "set_settings",
            Self::OpenReceived { .. } => "open_received",
            Self::ShowInFolder { .. } => "show_in_folder",
            Self::NetworkChanged => "network_changed",
            Self::StopInput => "stop_input",
            Self::MediaCommand { .. } => "media_command",
            Self::Shutdown => "shutdown",
        }
    }
}

impl IpcError {
    /// Build an error from a code and a message.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl Response {
    /// A failure response with `code` and `message`.
    pub fn err(code: ErrorCode, message: impl Into<String>) -> Self {
        Self::Err(IpcError::new(code, message))
    }

    /// The error code when this is a failure, for logging (SR-32: codes, never bodies).
    pub fn error_code(&self) -> Option<ErrorCode> {
        match self {
            Self::Ok(_) => None,
            Self::Err(e) => Some(e.code),
        }
    }
}

/// Daemon settings a UI may read and change.
// Each bool is an independent user toggle, not a state machine, so a struct of flags is
// the honest shape.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Folder received files land in (SR-23).
    pub receive_dir: PathBuf,
    /// Our advertised name (SR-8).
    pub device_name: String,
    /// Watch the local clipboard and push changes to peers (COSMIC and Windows only).
    pub clipboard_sync: bool,
    /// Sync clipboard images as well as text, both ways (PNG, at most 16 MiB). Missing from
    /// an older daemon's answer means on, its only behaviour.
    #[serde(default = "default_true")]
    pub clipboard_images: bool,
    /// Whether the platform can watch the clipboard at all; `false` on GNOME, where the
    /// window offers a button instead (anti-pattern 18: say why, do not fail silently).
    pub clipboard_watch_supported: bool,
    /// Whether this desktop can apply remote input (`SendInput` on Windows, the
    /// `RemoteDesktop` portal on Linux). `false` hides the `input` grant toggle, with
    /// `remote_input_reason` as the explanation. Missing from an older daemon reads as
    /// `false`.
    #[serde(default)]
    pub remote_input_supported: bool,
    /// Why remote input is unavailable, when it is (anti-pattern 18).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_input_reason: Option<String>,
    /// Start the daemon at login.
    pub autostart: bool,
    /// Why start at login is off although it was asked for: a sentence worded for the
    /// user (the Background portal or the desktop refused, or the login entry could not
    /// be written). `None` when it was never refused or the setting was turned on again
    /// successfully. Missing from an older daemon's answer reads as `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autostart_refused: Option<String>,
    /// Show desktop notifications.
    pub notifications: bool,
}

fn default_true() -> bool {
    true
}

/// A partial [`Settings`] update.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettingsPatch {
    /// New receive folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receive_dir: Option<PathBuf>,
    /// New device name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    /// Clipboard sync on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_sync: Option<bool>,
    /// Clipboard images on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_images: Option<bool>,
    /// Autostart on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub autostart: Option<bool>,
    /// Notifications on or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notifications: Option<bool>,
}

/// Daemon identity and state, the answer to [`Request::Status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    /// Our hex Device ID.
    pub device_id: String,
    /// Our Short ID (SR-8).
    pub short_id: String,
    /// Bound address.
    pub addr: SocketAddr,
    /// Daemon package version.
    pub daemon_version: String,
    /// IPC contract version ([`IPC_VERSION`]).
    pub ipc_version: u32,
    /// Current settings.
    pub settings: Settings,
    /// Whether the "Add device" window is open.
    pub add_device_window_open: bool,
    /// Whether the desktop is locked as far as the daemon knows (SR-6).
    pub locked: bool,
    /// Labels of the peers with an open remote input session on this desktop right now
    /// (SR-25), so a UI that starts mid-session can show its banner. Missing from an
    /// older daemon reads as none.
    #[serde(default)]
    pub input_from: Vec<String>,
}

/// What a port probe found, the daemon's part of [`Request::Diagnose`].
///
/// The daemon cannot see a firewall from inside; it dials its own port on loopback and on
/// every LAN interface address. Loopback reachable but LAN unreachable is the signature of a
/// host firewall and is reported as "port 42187 blocked" rather than an empty list
/// (PLAN.md Phase 2 "done when").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortProbe {
    /// The port probed.
    pub port: u16,
    /// Loopback dial succeeded.
    pub loopback_ok: bool,
    /// Per interface address: reachable or the error text.
    pub lan: Vec<AddrProbe>,
    /// Firewall tool detected (`ufw`, `firewalld`, `windows`) and whether it reports the port
    /// as allowed, when the daemon can tell without privileges.
    pub firewall: Option<FirewallHint>,
}

/// One address of the port probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddrProbe {
    /// The address dialled.
    pub addr: SocketAddr,
    /// Reachable.
    pub ok: bool,
    /// Error text when not.
    pub error: Option<String>,
}

/// A best-effort reading of the host firewall.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FirewallHint {
    /// `ufw`, `firewalld` or `windows`.
    pub tool: String,
    /// Active at all.
    pub active: bool,
    /// Port allowed, if known.
    pub port_allowed: Option<bool>,
    /// Human-readable fix, e.g. the `ufw allow` line.
    pub fix: String,
}

/// Full diagnostics: the core's view plus the daemon's port probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticsReport {
    /// From `Host::diagnose`.
    pub core: Diagnostics,
    /// From the daemon.
    pub port: PortProbe,
    /// One line per finding, already worded for the user, most important first.
    pub summary: Vec<String>,
}

/// Successful payload of a [`Response`]. Variants pair with [`Request`] variants; requests
/// without data answer [`Ok::Done`].
///
/// Wire shape: `{"result": "ok", "kind": "<variant>", "data": <payload>}` (no `data` for
/// [`Ok::Done`]). Adjacent tagging is required because several variants carry a list or a
/// string, which serde cannot flatten into an internally tagged map.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Ok {
    /// No data.
    Done,
    /// [`Request::Status`].
    Status(Status),
    /// [`Request::Peers`].
    Peers(Vec<PeerSummary>),
    /// [`Request::Transfers`].
    Transfers(Vec<TransferSnapshot>),
    /// [`Request::Diagnose`].
    Diagnostics(DiagnosticsReport),
    /// [`Request::AddDeviceList`].
    AddDeviceList(Vec<AddDeviceCandidate>),
    /// [`Request::ShowQr`]: the one-use URI. Never log it (SR-32).
    QrUri(String),
    /// [`Request::SendFiles`]: the new transfer ID.
    TransferStarted {
        /// Transfer ID.
        id: String,
    },
    /// [`Request::QueueOutbox`]: the new outbox item ID.
    OutboxQueued {
        /// Item ID.
        id: String,
    },
    /// [`Request::Outbox`]: held items, oldest first.
    Outbox(Vec<OutboxItem>),
    /// [`Request::PullOutbox`]: the incoming transfers started, one per item.
    OutboxPulled {
        /// Transfer IDs.
        ids: Vec<String>,
    },
    /// [`Request::PushClipboard`] and [`Request::SendClipboard`].
    ClipboardPushed {
        /// Hex Device IDs the clip was sent to.
        sent_to: Vec<String>,
        /// It matched the last clip applied or sent, so nothing was sent (protocol.md 5.7).
        skipped_echo: bool,
    },
    /// [`Request::GetSettings`] and [`Request::SetSettings`].
    Settings {
        /// Settings after the change.
        settings: Settings,
        /// The host was restarted to apply them.
        restarted: bool,
    },
}

/// Why a request failed. `code` is stable and machine-readable; `message` is for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code}: {message}")]
pub struct IpcError {
    /// Stable code, see [`ErrorCode`].
    pub code: ErrorCode,
    /// Human-readable text.
    pub message: String,
}

/// Stable error codes a UI can switch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErrorCode {
    /// Request did not parse or named an unknown operation.
    BadRequest,
    /// The caller is not the daemon's user or is another sandboxed app (SR-26, SR-27).
    Unauthorized,
    /// No such peer, transfer or path.
    NotFound,
    /// Peer query matched more than one device.
    Ambiguous,
    /// Peer is not online.
    Offline,
    /// Refused by policy: desktop locked (SR-6), grant missing (SR-14), path outside the
    /// receive folder.
    Forbidden,
    /// Clipboard text over the inline limit.
    TooLarge,
    /// Pairing or window state does not allow the request (window closed, pairing busy).
    InvalidState,
    /// Anything else; `message` carries the core error text.
    Internal,
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::BadRequest => "bad_request",
            Self::Unauthorized => "unauthorized",
            Self::NotFound => "not_found",
            Self::Ambiguous => "ambiguous",
            Self::Offline => "offline",
            Self::Forbidden => "forbidden",
            Self::TooLarge => "too_large",
            Self::InvalidState => "invalid_state",
            Self::Internal => "internal",
        };
        f.write_str(s)
    }
}

/// The daemon's answer to one [`Request`].
// `Ok` carries whole diagnostics reports; boxing would only move the allocation.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    /// Success.
    Ok(Ok),
    /// Failure.
    Err(IpcError),
}

impl From<Result<Ok, IpcError>> for Response {
    fn from(r: Result<Ok, IpcError>) -> Self {
        match r {
            Result::Ok(ok) => Self::Ok(ok),
            Result::Err(e) => Self::Err(e),
        }
    }
}

impl From<Response> for Result<Ok, IpcError> {
    fn from(r: Response) -> Self {
        match r {
            Response::Ok(ok) => Result::Ok(ok),
            Response::Err(e) => Result::Err(e),
        }
    }
}

/// Something the daemon tells every connected client, unprompted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Event {
    /// A core event, forwarded as is (`docs/phases/phase-1-core.md` section 3.5).
    Host(HostEvent),
    /// A daemon-level event.
    Daemon(DaemonEvent),
}

/// Events that originate in the daemon rather than the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum DaemonEvent {
    /// Settings changed (by any client or the tray).
    SettingsChanged {
        /// New settings.
        settings: Settings,
    },
    /// The local clipboard was pushed to peers.
    ClipboardPushed {
        /// Hex Device IDs.
        sent_to: Vec<String>,
    },
    /// A received clipboard was applied locally.
    ClipboardApplied {
        /// Hex Device ID.
        peer: String,
        /// SR-24 flag.
        sensitive: bool,
    },
    /// Desktop lock state changed (SR-6).
    LockChanged {
        /// Locked now.
        locked: bool,
    },
    /// The host was restarted (settings change or recovery).
    HostRestarted,
    /// A peer started (`active`) or ended (`active: false`) a remote input session on this
    /// desktop (protocol.md 5.12, SR-25). The window shows "Remote input from <label>" with
    /// a Stop button while one is active.
    InputSession {
        /// Hex Device ID.
        peer: String,
        /// The peer's local label.
        label: String,
        /// Open now.
        active: bool,
    },
    /// The daemon is shutting down; clients should disconnect.
    ShuttingDown,
}

/// What the server needs from the daemon: a handler for authenticated requests. The
/// transports call it only after the caller passed the SR-26 / SR-27 checks.
pub trait RequestHandler: Send + Sync + 'static {
    /// Handle one request.
    fn handle(&self, request: Request) -> futures_util::future::BoxFuture<'_, Response>;
}

impl<F, Fut> RequestHandler for F
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    fn handle(&self, request: Request) -> futures_util::future::BoxFuture<'_, Response> {
        Box::pin(self(request))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_as_tagged_json() {
        let r = Request::SetLabel {
            peer: "k3qd".into(),
            label: "Phone".into(),
        };
        let json = serde_json::to_string(&r).expect("serialise");
        assert!(json.contains(r#""op":"set_label""#));
        let back: Request = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, r);
    }

    #[test]
    fn response_error_carries_stable_code() {
        let r = Response::Err(IpcError {
            code: ErrorCode::Forbidden,
            message: "desktop is locked".into(),
        });
        let json = serde_json::to_string(&r).expect("serialise");
        assert!(json.contains(r#""code":"forbidden""#));
        let back: Response = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, r);
    }

    #[test]
    fn every_ok_shape_round_trips() {
        let oks = [
            Ok::Done,
            Ok::Peers(Vec::new()),
            Ok::Transfers(Vec::new()),
            Ok::AddDeviceList(Vec::new()),
            Ok::QrUri("playdrop://x".into()),
            Ok::TransferStarted { id: "t1".into() },
            Ok::OutboxQueued { id: "o1".into() },
            Ok::Outbox(vec![OutboxItem {
                id: "o1".into(),
                peer: "ab".repeat(32),
                file_count: 2,
                total_bytes: 2048,
                note: None,
                created_at_ms: 1,
            }]),
            Ok::OutboxPulled {
                ids: vec!["t1".into()],
            },
            Ok::ClipboardPushed {
                sent_to: vec!["ab".into()],
                skipped_echo: false,
            },
        ];
        for ok in oks {
            let r = Response::Ok(ok);
            let json = serde_json::to_string(&r).expect("serialise");
            assert!(json.contains(r#""result":"ok""#), "{json}");
            let back: Response = serde_json::from_str(&json).expect("parse");
            assert_eq!(back, r);
        }
    }

    #[test]
    fn op_matches_the_serde_tag() {
        let requests = [
            Request::Status,
            Request::SetSettings(SettingsPatch::default()),
            Request::SendFiles {
                peer: "p".into(),
                paths: vec![],
            },
            Request::OpenAddDeviceWindow,
            Request::Ping {
                peer: "p".into(),
                message: None,
            },
            Request::QueueOutbox {
                peer: "p".into(),
                paths: vec![],
            },
            Request::Outbox,
            Request::RemoveOutbox { id: "o1".into() },
            Request::PullOutbox { peer: "p".into() },
        ];
        for r in requests {
            let value = serde_json::to_value(&r).expect("serialise");
            assert_eq!(value["op"], r.op());
        }
    }

    #[test]
    fn settings_from_an_older_daemon_default_images_on() {
        let json = r#"{"receive_dir":"/tmp/in","device_name":"a","clipboard_sync":true,
            "clipboard_watch_supported":true,"autostart":false,"notifications":true}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        assert!(s.clipboard_images);
    }

    #[test]
    fn unknown_events_are_an_error_the_client_can_skip() {
        // A newer daemon's event this build does not know must fail to decode (the client
        // then ignores it) rather than decode as something else.
        let json = r#"{"source":"host","event":"something_new","peer":"ab"}"#;
        assert!(serde_json::from_str::<Event>(json).is_err());
    }
}
