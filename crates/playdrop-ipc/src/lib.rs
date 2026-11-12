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
//! - [`types`]: every `playdrop-core` type the contract carries, re-exported, so a client
//!   needs no `playdrop-core` import of its own for them.
//!
//! Secrets (`Secret<T>` inside events) are serialised for the UI but never logged (SR-32).

#![forbid(clippy::unwrap_used)]

pub mod client;
pub mod server;
pub mod wire;

/// The `playdrop-core` types that appear in the IPC contract, re-exported: what [`Request`],
/// [`Ok`] and [`Event`] carry, and what those carry in turn. A client imports them from here
/// (`use playdrop_ipc::types::{PeerSummary, TransferSnapshot};`) instead of depending on
/// `playdrop-core` for the shapes. They are the core's own types, so values move between the
/// daemon and a client with no conversion.
///
/// Only contract types are listed. Core items a client may use for other reasons (the reach
/// notices, identity helpers) stay in `playdrop-core`.
pub mod types {
    pub use playdrop_core::host::{
        AddDeviceCandidate, Base64Bytes, DeliveredFile, Diagnostics, DialFailure, DiscoveryPath,
        FileRoot, HostEvent, InterfaceInfo, JoinFailure, MdnsStatus, MediaState,
        MirroredNotification, MulticastStatus, OfferedFile, OutboxItem, PairingPromptKind,
        PeerBattery, PeerDiagnostic, PeerSummary, Secret, TransferDirection, TransferFailure,
        TransferSnapshot, TransferStatus, OFFER_FILES_MAX, TRANSFER_NAMES_MAX,
    };
    pub use playdrop_core::pin_store::Grants;
    pub use playdrop_core::session::types::{
        ButtonAction, Input, KeyAction, KeyInput, MediaAction, Modifier, PointerButton,
        PointerInput, PresenterAction, ScrollUnit, SessionOp,
    };
}

#[cfg(target_os = "linux")]
mod linux;
mod pipe;
mod util;
#[cfg(windows)]
mod win;

use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::types::{
    AddDeviceCandidate, Diagnostics, Grants, HostEvent, MediaAction, OutboxItem, PeerSummary,
    TransferSnapshot,
};

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
    /// Turn a peer's `remote` grant on or off (protocol.md 12). Only a local UI may do this
    /// (SR-14). A daemon built without reach answers `invalid_state`.
    SetRemoteGrant {
        /// Peer query.
        peer: String,
        /// The new state.
        on: bool,
    },
    /// Peers currently reached across networks, with the line that says so (SR-39). Empty from a
    /// daemon built without reach.
    ReachPaths,
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
    /// Send what is on this computer's clipboard now: the daemon reads it through its own
    /// backend (text, or a PNG image while `clipboard_images` is on) and pushes it, so a client
    /// need not read the clipboard itself. Answers [`Ok::ClipboardPushed`].
    ///
    /// SR-24: a clip a password manager marked goes out only with `allow_sensitive`; without it
    /// nothing is sent, `skipped` is [`ClipSkip::Sensitive`] and the answer carries a one-use
    /// `consent` token for that clip. The client asks the user and sends again with
    /// `allow_sensitive: true` and that token: the daemon sends only if the clipboard still
    /// holds the clip that was refused (`docs/ipc.md` 1.7 has the full rules). With
    /// `allow_sensitive: true` and no token (the CLI, where typing the command is the consent)
    /// it sends whatever is there. A send with `allow_sensitive: true` always goes marked
    /// sensitive, hint or no hint. An empty
    /// clipboard answers `skipped` [`ClipSkip::Empty`]. A session where the daemon cannot read
    /// the clipboard (GNOME, the Flatpak on COSMIC) answers [`ErrorCode::InvalidState`] with the
    /// reason sentence; the client then reads the clipboard itself and uses
    /// [`Request::SendClipboard`] or [`Request::PushClipboard`], as it does when an older daemon
    /// answers [`ErrorCode::BadRequest`] to this request.
    SendLocalClipboard {
        /// Peer query, or `None` for every online peer with the `clipboardOut` grant (like
        /// [`Request::PushClipboard`]).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
        /// The user agreed to send a password-manager clip (SR-24). A send with it always
        /// goes marked sensitive.
        #[serde(default)]
        allow_sensitive: bool,
        /// The `consent` token from the refusal the user just answered: binds the consent to
        /// the clip that was refused. Never log it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        consent: Option<ConsentToken>,
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
        action: MediaAction,
        /// `seek`: absolute position in ms; `volume`: 0 to 100; otherwise `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<u64>,
    },
    /// Ask the daemon to exit cleanly.
    Shutdown,
    /// A UI opened (`force: false`) or the user pressed "Check now" (`force: true`): ask
    /// GitHub whether a newer release exists (`docs/design/update-check.md`, SR-45). The
    /// daemon never checks on its own. Without `force` a check runs at most once per 24 h
    /// (one hour after a failed one); `force` skips that wait but never the
    /// `update_check` setting: with the setting off the answer is [`UpdateState::Off`] and no
    /// request leaves the computer. Answers [`Ok::Update`] once the check is over.
    UpdateCheck {
        /// "Check now".
        #[serde(default)]
        force: bool,
    },
    /// The user pressed "Update": download, verify and install the offered release (SR-47).
    /// Answers [`Ok::Update`] at once with the download started; progress and the outcome
    /// arrive as [`DaemonEvent::UpdateChanged`]. `invalid_state` when nothing installable
    /// is offered (no offer, a Flatpak or another install kind, or one already running).
    UpdateInstall {
        /// The request comes from the window, which the Windows installer then starts again
        /// along with the daemon (`/RELAUNCH=1`); without it only the daemon is started
        /// again (`/RELAUNCH=daemon`). Ignored on Linux.
        #[serde(default)]
        window: bool,
    },
    /// The user pressed "Cancel" while the update downloads: stop the download, remove what
    /// was downloaded and offer the release again. Answers [`Ok::Update`].
    /// `invalid_state` when no download runs, including once the files were handed to the
    /// system installer, which cannot be stopped from here.
    UpdateCancel,
    /// The user pressed "Later": hide the offer until the next check that runs. Answers
    /// [`Ok::Update`].
    UpdateLater,
    /// The update state as it is now, without contacting anyone. Answers [`Ok::Update`].
    UpdateState,
    /// Open the offered release's page in the browser. The daemon opens only the page of
    /// the release it offers, never a URL a client names. `invalid_state` without an offer.
    UpdateOpenPage,
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
            Self::SetRemoteGrant { .. } => "set_remote_grant",
            Self::ReachPaths => "reach_paths",
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
            Self::SendLocalClipboard { .. } => "send_local_clipboard",
            Self::GetSettings => "get_settings",
            Self::SetSettings(_) => "set_settings",
            Self::OpenReceived { .. } => "open_received",
            Self::ShowInFolder { .. } => "show_in_folder",
            Self::NetworkChanged => "network_changed",
            Self::StopInput => "stop_input",
            Self::MediaCommand { .. } => "media_command",
            Self::Shutdown => "shutdown",
            Self::UpdateCheck { .. } => "update_check",
            Self::UpdateInstall { .. } => "update_install",
            Self::UpdateCancel => "update_cancel",
            Self::UpdateLater => "update_later",
            Self::UpdateState => "update_state",
            Self::UpdateOpenPage => "update_open_page",
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
    /// Ask GitHub for the latest version when a UI opens, at most once a day
    /// (`docs/design/update-check.md`, SR-45). Default on. Missing from an older daemon's
    /// answer reads as on; such a daemon has no update check at all, so a client also
    /// treats a `bad_request` answer to [`Request::UpdateCheck`] as "not available".
    #[serde(default = "default_true")]
    pub update_check: bool,
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
    /// The update check on or off. A patch from an older client has no such field, which
    /// reads as `None` and leaves the setting as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_check: Option<bool>,
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

/// A peer reached across networks, for [`Request::ReachPaths`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReachPathLine {
    /// Short ID of the peer.
    pub short_id: String,
    /// What to print: which path, and that it is the slow one. No URL and no Endpoint ID (SR-40).
    pub line: String,
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
    /// [`Request::ReachPaths`].
    ReachPaths(Vec<ReachPathLine>),
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
    /// [`Request::PushClipboard`], [`Request::SendClipboard`] and
    /// [`Request::SendLocalClipboard`].
    ClipboardPushed {
        /// Hex Device IDs the clip was sent to.
        sent_to: Vec<String>,
        /// It matched the last clip applied or sent, so nothing was sent (protocol.md 5.7).
        /// Kept for older clients; it is true exactly when `skipped` is [`ClipSkip::Echo`].
        skipped_echo: bool,
        /// Why nothing was sent; `None` when something was. Absent from an older daemon
        /// reads as `None`: use `skipped_echo` then.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        skipped: Option<ClipSkip>,
        /// With `skipped` [`ClipSkip::Sensitive`]: a one-use token (128 random bits, hex) to send
        /// back in [`Request::SendLocalClipboard`] once the user agreed. Not derived from the
        /// clip. `None` otherwise, and from an older daemon. Never log it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        consent: Option<ConsentToken>,
    },
    /// [`Request::GetSettings`] and [`Request::SetSettings`].
    Settings {
        /// Settings after the change.
        settings: Settings,
        /// The host was restarted to apply them.
        restarted: bool,
    },
    /// [`Request::UpdateCheck`], [`Request::UpdateInstall`], [`Request::UpdateLater`] and
    /// [`Request::UpdateState`].
    Update(UpdateStatus),
}

/// The update check's state (`docs/design/update-check.md`, `docs/ipc.md` 1.9): the answer
/// to every update request and the payload of [`DaemonEvent::UpdateChanged`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateStatus {
    /// Where the update stands.
    pub state: UpdateState,
    /// When a check last got an answer from GitHub, Unix milliseconds. `None` before the
    /// first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_checked_ms: Option<u64>,
    /// When the last check failed, Unix milliseconds, while that failure is newer than the
    /// last answer; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failed_ms: Option<u64>,
    /// Why that check failed, one plain sentence, while the daemon still knows it (it is
    /// not kept across a restart). A failed check leaves an offer that verified earlier on
    /// screen, so this is where Settings reads the failure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_failure: Option<String>,
    /// The version the daemon runs, exactly as built (a pre-release keeps its suffix).
    pub running_version: String,
    /// Set when this build has no update check at all, with one plain sentence for Settings
    /// ("This is not a release build, so it does not check for updates."): the running
    /// version is not a plain release, so no request is ever made and nothing offered.
    /// The state then stays `idle` (or `off` with the setting off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
}

/// Where an update stands. Tagged on `phase`. A phase added by a newer daemon reads as
/// [`UpdateState::Other`], so an answer that carries one still decodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum UpdateState {
    /// The `update_check` setting is off: nothing is asked and nothing is offered.
    Off,
    /// No check has run in this daemon's life, or the offer was put off with "Later".
    Idle,
    /// Asking GitHub now.
    Checking,
    /// The last check found no newer release.
    UpToDate,
    /// A newer, signed release exists.
    Available {
        /// The release.
        offer: UpdateOffer,
    },
    /// Downloading the release files.
    Downloading {
        /// Version being downloaded.
        version: String,
        /// Bytes so far, all files together.
        done: u64,
        /// Bytes in all, from the server's lengths; 0 while unknown.
        total: u64,
    },
    /// Comparing the download with its signed checksum.
    Verifying {
        /// Version being verified.
        version: String,
    },
    /// The system's own prompt (polkit, UAC) and the package install. On Windows this is
    /// the last state: the installer stops the daemon and starts the new one.
    Installing {
        /// Version being installed.
        version: String,
    },
    /// Installed; the daemon is starting its new version. A window that is still the old
    /// program says so (`docs/ipc.md` 1.9).
    Restarting {
        /// Version now installed.
        version: String,
    },
    /// The last step failed; the old version keeps running and nothing downloaded is kept.
    Failed {
        /// Which step.
        stage: UpdateStage,
        /// What happened, one plain sentence for the user.
        message: String,
        /// The offer, still valid, when a download or install failed: the UI may offer
        /// "Update" again.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        offer: Option<UpdateOffer>,
    },
    /// A phase this build does not know.
    #[serde(other)]
    Other,
}

/// A newer release, as the user is offered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateOffer {
    /// The release version, `major.minor.patch`.
    pub version: String,
    /// The release page ("What is new"), opened through [`Request::UpdateOpenPage`].
    pub page_url: String,
    /// Whether "Update" can install it here (the `.deb` or the Windows installer). False
    /// for the Flatpak and other installs, which get the release page instead.
    pub can_install: bool,
    /// With `can_install: false`: one sentence saying why, for the UI to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The step an update failed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStage {
    /// Asking GitHub, or checking the signature of its answer.
    Check,
    /// Downloading a release file.
    Download,
    /// Comparing a download with its signed checksum.
    Verify,
    /// The system prompt or the package install.
    Install,
    /// A stage added by a newer daemon.
    #[serde(other)]
    Other,
}

/// The one-use SR-24 consent token of [`Request::SendLocalClipboard`] and
/// [`Ok::ClipboardPushed`]: 32 lowercase hex characters (128 random bits) on the wire, a plain
/// JSON string. `Debug` never shows it, so a `{:?}` of a request, an answer or a client's own
/// state cannot leak it into a log (SR-32).
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ConsentToken(String);

impl ConsentToken {
    /// Wraps a token the daemon made.
    #[must_use]
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    /// The token text, for comparing it with the one the daemon remembers.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ConsentToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConsentToken(..)")
    }
}

/// Why a clipboard push sent nothing, in [`Ok::ClipboardPushed`]. Only reasons the host can
/// tell apart are listed. A reason from a newer daemon that this build does not know reads
/// as [`ClipSkip::Other`], so the answer still decodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipSkip {
    /// The text matches the last clip applied from a peer or sent (protocol.md 5.7).
    Echo,
    /// No connected device has clipboard sharing turned on for this computer: peers are
    /// offline, or their clipboard grant is off.
    NoPeers,
    /// Devices are connected but cannot take this clip (an image to a device without image
    /// support, or long text to one that cannot fetch it).
    Unsupported,
    /// [`Request::SendLocalClipboard`] only: a password manager marked the clip and the
    /// request did not carry `allow_sensitive` (SR-24). Ask the user, then send again with it.
    Sensitive,
    /// [`Request::SendLocalClipboard`] only: the clipboard holds nothing the daemon sends (no
    /// text, and no image or images are off).
    Empty,
    /// A reason added by a newer daemon.
    #[serde(other)]
    Other,
}

impl ClipSkip {
    /// A plain-words reason, shared by the window, the CLI and the daemon's notices.
    #[must_use]
    pub fn reason(self) -> &'static str {
        match self {
            Self::Echo => "it matches the last clip sent or received",
            Self::NoPeers => "no connected device has clipboard sharing turned on",
            Self::Unsupported => "the connected devices cannot receive this kind of clip",
            Self::Sensitive => "a password manager marked it as secret",
            Self::Empty => "the clipboard is empty",
            Self::Other => "the daemon gave a reason this version does not know",
        }
    }
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
    /// The update state changed (`docs/ipc.md` 1.9): a check started or ended, download
    /// progress, the install outcome, "Later", or the setting was switched.
    UpdateChanged(UpdateStatus),
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
                skipped: None,
                consent: None,
            },
            Ok::ClipboardPushed {
                sent_to: vec![],
                skipped_echo: false,
                skipped: Some(ClipSkip::NoPeers),
                consent: None,
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
    fn clip_skip_is_a_snake_case_string_and_optional() {
        let json = serde_json::to_string(&Ok::ClipboardPushed {
            sent_to: vec![],
            skipped_echo: false,
            skipped: Some(ClipSkip::NoPeers),
            consent: None,
        })
        .expect("serialise");
        assert!(json.contains(r#""skipped":"no_peers""#), "{json}");
        for (skip, name) in [
            (ClipSkip::Echo, "echo"),
            (ClipSkip::Unsupported, "unsupported"),
        ] {
            assert_eq!(
                serde_json::to_string(&skip).expect("json"),
                format!("\"{name}\"")
            );
        }
        // A reason from a newer daemon still decodes.
        assert_eq!(
            serde_json::from_str::<ClipSkip>("\"battery_saver\"").expect("json"),
            ClipSkip::Other
        );
        // A sent clip carries no reason at all.
        let sent = serde_json::to_string(&Ok::ClipboardPushed {
            sent_to: vec!["ab".into()],
            skipped_echo: false,
            skipped: None,
            consent: None,
        })
        .expect("serialise");
        assert!(!sent.contains("skipped\""), "{sent}");
        // An older daemon's answer has no `skipped`.
        let old: Ok = serde_json::from_str(
            r#"{"kind":"clipboard_pushed","data":{"sent_to":[],"skipped_echo":true}}"#,
        )
        .expect("old answer");
        assert_eq!(
            old,
            Ok::ClipboardPushed {
                sent_to: vec![],
                skipped_echo: true,
                skipped: None,
                consent: None,
            }
        );
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
            Request::SendLocalClipboard {
                peer: None,
                allow_sensitive: false,
                consent: None,
            },
        ];
        for r in requests {
            let value = serde_json::to_value(&r).expect("serialise");
            assert_eq!(value["op"], r.op());
        }
    }

    #[test]
    fn send_local_clipboard_round_trips_and_defaults() {
        for r in [
            Request::SendLocalClipboard {
                peer: Some("k3qd".into()),
                allow_sensitive: true,
                consent: None,
            },
            Request::SendLocalClipboard {
                peer: None,
                allow_sensitive: false,
                consent: None,
            },
        ] {
            let json = serde_json::to_string(&r).expect("serialise");
            assert!(json.contains(r#""op":"send_local_clipboard""#), "{json}");
            assert_eq!(serde_json::from_str::<Request>(&json).expect("parse"), r);
        }
        // Every peer is `peer` left out, and the SR-24 consent is off unless given.
        let bare: Request =
            serde_json::from_str(r#"{"op":"send_local_clipboard"}"#).expect("parse");
        assert_eq!(
            bare,
            Request::SendLocalClipboard {
                peer: None,
                allow_sensitive: false,
                consent: None
            }
        );
        // An older daemon's contract has no such op: it cannot decode it and answers
        // `bad_request`, which is the client's cue to read the clipboard itself.
        assert!(serde_json::from_str::<Request>(r#"{"op":"send_local_clipbord"}"#).is_err());
    }

    #[test]
    fn consent_token_travels_both_ways_and_is_optional() {
        let r = Request::SendLocalClipboard {
            peer: Some("k3qd".into()),
            allow_sensitive: true,
            consent: Some(ConsentToken::new("ab".repeat(16))),
        };
        let json = serde_json::to_string(&r).expect("json");
        assert!(
            json.contains(&format!(r#""consent":"{}""#, "ab".repeat(16))),
            "{json}"
        );
        assert_eq!(serde_json::from_str::<Request>(&json).expect("parse"), r);
        // No token: not written, and a request without the field (the CLI) reads as none.
        let bare = Request::SendLocalClipboard {
            peer: None,
            allow_sensitive: true,
            consent: None,
        };
        assert!(!serde_json::to_string(&bare)
            .expect("json")
            .contains("consent"));
        assert_eq!(
            serde_json::from_str::<Request>(
                r#"{"op":"send_local_clipboard","allow_sensitive":true}"#
            )
            .expect("parse"),
            bare
        );
        // The refusal carries it; an answer without it (any older daemon) reads as none.
        let refused = Ok::ClipboardPushed {
            sent_to: vec![],
            skipped_echo: false,
            skipped: Some(ClipSkip::Sensitive),
            consent: Some(ConsentToken::new("cd".repeat(16))),
        };
        let json = serde_json::to_string(&refused).expect("json");
        assert_eq!(serde_json::from_str::<Ok>(&json).expect("parse"), refused);
        let old: Ok = serde_json::from_str(
            r#"{"kind":"clipboard_pushed","data":{"sent_to":["ab"],"skipped_echo":false}}"#,
        )
        .expect("old");
        assert!(matches!(old, Ok::ClipboardPushed { consent: None, .. }));
    }

    #[test]
    fn debug_never_shows_the_consent_token() {
        let token = "0123456789abcdef0123456789abcdef";
        let request = Request::SendLocalClipboard {
            peer: Some("k3qd".into()),
            allow_sensitive: true,
            consent: Some(ConsentToken::new(token)),
        };
        let answer = Response::Ok(Ok::ClipboardPushed {
            sent_to: vec![],
            skipped_echo: false,
            skipped: Some(ClipSkip::Sensitive),
            consent: Some(ConsentToken::new(token)),
        });
        for shown in [format!("{request:?}"), format!("{answer:?}")] {
            assert!(!shown.contains(token), "{shown}");
            assert!(shown.contains("ConsentToken(..)"), "{shown}");
        }
        // The wire still carries it as a plain string.
        let json = serde_json::to_string(&request).expect("json");
        assert!(json.contains(&format!(r#""consent":"{token}""#)), "{json}");
        let back: ConsentToken = serde_json::from_str(&format!("\"{token}\"")).expect("parse");
        assert_eq!(back.as_str(), token);
    }

    #[test]
    fn transfer_names_more_is_optional() {
        let t: TransferSnapshot = serde_json::from_str(
            r#"{"id":"t","peer":"ab","short_id":"k","direction":"send","state":"active",
            "bytes":0,"total":0,"bytes_per_sec":0,"started_at":0,"file_count":9,"note":null,
            "names":["a","b","c"],"names_more":true}"#,
        )
        .expect("parse");
        assert!(t.names_more);
        let json = serde_json::to_string(&TransferSnapshot {
            names_more: false,
            ..t
        })
        .expect("json");
        assert!(!json.contains("names_more"), "{json}");
    }

    #[test]
    fn new_clip_skips_are_snake_case_and_older_builds_read_them_as_other() {
        // A 0.8.0 client's `ClipSkip` had no `sensitive`; its `#[serde(other)]` arm is what
        // this build's `Other` is, so the same answer decodes there as `other`.
        #[derive(Debug, Deserialize, PartialEq)]
        #[serde(rename_all = "snake_case")]
        enum OldClipSkip {
            Echo,
            NoPeers,
            Unsupported,
            #[serde(other)]
            Other,
        }
        for (skip, name) in [
            (ClipSkip::Sensitive, "sensitive"),
            (ClipSkip::Empty, "empty"),
        ] {
            assert_eq!(
                serde_json::to_string(&skip).expect("json"),
                format!("\"{name}\"")
            );
            let back: ClipSkip = serde_json::from_str(&format!("\"{name}\"")).expect("json");
            assert_eq!(back, skip);
            assert_ne!(skip.reason(), "");
        }
        let refused = Response::Ok(Ok::ClipboardPushed {
            sent_to: vec![],
            skipped_echo: false,
            skipped: Some(ClipSkip::Sensitive),
            consent: None,
        });
        let json = serde_json::to_string(&refused).expect("serialise");
        assert!(json.contains(r#""skipped":"sensitive""#), "{json}");
        assert_eq!(
            serde_json::from_str::<OldClipSkip>("\"sensitive\"").expect("old"),
            OldClipSkip::Other
        );
    }

    #[test]
    fn peers_and_transfers_from_an_older_daemon_decode() {
        // 0.8.0 shapes: no `battery` on a peer, no `names` on a transfer.
        let peers = r#"{"kind":"peers","data":[{"device_id":"ab","short_id":"k3qd",
            "label":"Phone","online":true,"grants":{"files":true,"clipboardIn":true,
            "clipboardOut":true,"input":false,"media":false,
            "find":true},"last_known":[],"paired_at":1}]}"#;
        let Ok::Peers(p) = serde_json::from_str::<Ok>(peers).expect("old peers") else {
            panic!("peers");
        };
        assert_eq!(p[0].battery, None);
        let transfers = r#"{"kind":"transfers","data":[{"id":"t1","peer":"ab",
            "short_id":"k3qd","direction":"send","state":"active","bytes":1,"total":2,
            "bytes_per_sec":0,"started_at":1,"file_count":5000,"note":null}]}"#;
        let Ok::Transfers(t) = serde_json::from_str::<Ok>(transfers).expect("old transfers") else {
            panic!("transfers");
        };
        assert_eq!(t[0].names, Vec::<String>::new());
        assert_eq!(t[0].file_count, 5000);
    }

    #[test]
    fn peers_and_transfers_round_trip_with_the_new_fields() {
        use crate::types::{PeerBattery, TransferDirection, TransferStatus};
        let peer = PeerSummary {
            device_id: "ab".repeat(32),
            short_id: "k3qd-7f2m-xa9p-41bc".into(),
            label: "Phone".into(),
            online: true,
            grants: Grants::default(),
            last_known: vec![],
            paired_at: 1,
            last_seen: Some(2),
            removed_by_peer: false,
            capabilities: vec!["battery".into()],
            battery: Some(PeerBattery {
                level: 41,
                charging: true,
                low: false,
            }),
        };
        let json = serde_json::to_string(&Ok::Peers(vec![peer.clone()])).expect("json");
        assert!(
            json.contains(r#""battery":{"level":41,"charging":true,"low":false}"#),
            "{json}"
        );
        let Ok::Peers(back) = serde_json::from_str::<Ok>(&json).expect("parse") else {
            panic!("peers");
        };
        assert_eq!(back, vec![peer.clone()]);
        // Offline peers carry no battery, and none is written.
        let offline = PeerSummary {
            online: false,
            battery: None,
            ..peer
        };
        let json = serde_json::to_string(&offline).expect("json");
        assert!(!json.contains(r#""battery":"#), "{json}");

        let snap = TransferSnapshot {
            id: "t1".into(),
            peer: "ab".repeat(32),
            short_id: "k3qd".into(),
            direction: TransferDirection::Receive,
            state: TransferStatus::Active,
            bytes: 1,
            total: 2,
            bytes_per_sec: 3,
            files_done: 0,
            started_at: 4,
            file_count: 5000,
            names: vec!["Photos".into(), "notes.txt".into()],
            names_more: false,
            note: None,
            saved: vec![],
            folder: None,
            delivered: vec![],
            resumable: false,
            failure: None,
        };
        let json = serde_json::to_string(&Ok::Transfers(vec![snap.clone()])).expect("json");
        assert!(json.contains(r#""names":["Photos","notes.txt"]"#), "{json}");
        let Ok::Transfers(back) = serde_json::from_str::<Ok>(&json).expect("parse") else {
            panic!("transfers");
        };
        assert_eq!(back, vec![snap]);
    }

    #[test]
    fn settings_from_an_older_daemon_default_images_on() {
        let json = r#"{"receive_dir":"/tmp/in","device_name":"a","clipboard_sync":true,
            "clipboard_watch_supported":true,"autostart":false,"notifications":true}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        assert!(s.clipboard_images);
    }

    #[test]
    fn update_check_setting_defaults_on_and_an_old_patch_leaves_it_alone() {
        let json = r#"{"receive_dir":"/tmp/in","device_name":"a","clipboard_sync":true,
            "clipboard_watch_supported":true,"autostart":false,"notifications":true}"#;
        let s: Settings = serde_json::from_str(json).expect("parse");
        assert!(s.update_check, "an older daemon's answer reads as on");
        // A 0.10.0 client's patch has no such field: it must not touch the setting.
        let old: SettingsPatch = serde_json::from_str(r#"{"notifications":false}"#).expect("old");
        assert_eq!(old.update_check, None);
        let off = SettingsPatch {
            update_check: Some(false),
            ..SettingsPatch::default()
        };
        let json = serde_json::to_string(&off).expect("json");
        assert_eq!(json, r#"{"update_check":false}"#);
        assert_eq!(
            serde_json::from_str::<SettingsPatch>(&json).expect("parse"),
            off
        );
    }

    #[test]
    fn update_requests_have_their_tags_and_force_defaults_off() {
        let requests = [
            Request::UpdateCheck { force: true },
            Request::UpdateInstall { window: true },
            Request::UpdateInstall { window: false },
            Request::UpdateCancel,
            Request::UpdateLater,
            Request::UpdateState,
            Request::UpdateOpenPage,
        ];
        for r in requests {
            let value = serde_json::to_value(&r).expect("serialise");
            assert_eq!(value["op"], r.op());
            let back: Request = serde_json::from_value(value).expect("parse");
            assert_eq!(back, r);
        }
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"update_check"}"#).expect("bare"),
            Request::UpdateCheck { force: false }
        );
        // A client that names no window gets the daemon-only relaunch.
        assert_eq!(
            serde_json::from_str::<Request>(r#"{"op":"update_install"}"#).expect("bare"),
            Request::UpdateInstall { window: false }
        );
    }

    fn offer() -> UpdateOffer {
        UpdateOffer {
            version: "0.11.0".into(),
            page_url: "https://github.com/QasimBhb/PlayDrop/releases/tag/v0.11.0".into(),
            can_install: true,
            reason: None,
        }
    }

    #[test]
    fn update_states_round_trip_in_answers_and_events() {
        let states = [
            UpdateState::Off,
            UpdateState::Idle,
            UpdateState::Checking,
            UpdateState::UpToDate,
            UpdateState::Available { offer: offer() },
            UpdateState::Downloading {
                version: "0.11.0".into(),
                done: 12,
                total: 48,
            },
            UpdateState::Verifying {
                version: "0.11.0".into(),
            },
            UpdateState::Installing {
                version: "0.11.0".into(),
            },
            UpdateState::Restarting {
                version: "0.11.0".into(),
            },
            UpdateState::Failed {
                stage: UpdateStage::Verify,
                message: "The download did not match.".into(),
                offer: Some(offer()),
            },
        ];
        for state in states {
            let report = UpdateStatus {
                state,
                last_checked_ms: Some(5),
                last_failed_ms: None,
                last_failure: None,
                running_version: "0.10.0".into(),
                unavailable: None,
            };
            let answer = Response::Ok(Ok::Update(report.clone()));
            let json = serde_json::to_string(&answer).expect("json");
            assert!(json.contains(r#""kind":"update""#), "{json}");
            assert_eq!(
                serde_json::from_str::<Response>(&json).expect("parse"),
                answer
            );
            let event = Event::Daemon(DaemonEvent::UpdateChanged(report));
            let json = serde_json::to_string(&event).expect("json");
            assert!(json.contains(r#""event":"update_changed""#), "{json}");
            assert_eq!(serde_json::from_str::<Event>(&json).expect("parse"), event);
        }
        let json = serde_json::to_string(&UpdateState::Available { offer: offer() }).expect("j");
        assert_eq!(
            json,
            r#"{"phase":"available","offer":{"version":"0.11.0","page_url":"https://github.com/QasimBhb/PlayDrop/releases/tag/v0.11.0","can_install":true}}"#
        );
    }

    #[test]
    fn an_update_phase_or_stage_from_a_newer_daemon_still_decodes() {
        let status: UpdateStatus = serde_json::from_str(
            r#"{"state":{"phase":"rolling_back","step":3},"running_version":"0.11.0"}"#,
        )
        .expect("parse");
        assert_eq!(status.state, UpdateState::Other);
        assert_eq!(status.last_checked_ms, None);
        assert_eq!(status.unavailable, None);
        let not_release: UpdateStatus = serde_json::from_str(
            r#"{"state":{"phase":"idle"},"running_version":"0.11.0-rc.1","unavailable":"No."}"#,
        )
        .expect("parse");
        assert_eq!(not_release.unavailable.as_deref(), Some("No."));
        let failed: UpdateState =
            serde_json::from_str(r#"{"phase":"failed","stage":"quarantine","message":"m"}"#)
                .expect("parse");
        assert_eq!(
            failed,
            UpdateState::Failed {
                stage: UpdateStage::Other,
                message: "m".into(),
                offer: None
            }
        );
    }

    #[test]
    fn unknown_events_are_an_error_the_client_can_skip() {
        // A newer daemon's event this build does not know must fail to decode (the client
        // then ignores it) rather than decode as something else.
        let json = r#"{"source":"host","event":"something_new","peer":"ab"}"#;
        assert!(serde_json::from_str::<Event>(json).is_err());
    }
}
