//! Events the host reports to its UI.
//!
//! Some events carry text that must reach the user's screen but never a log: the SAS and
//! clipboard contents (SR-32). Those fields are [`Secret`], whose `Debug` is redacted, so a
//! stray `{:?}` in a log line cannot leak them. `Serialize` keeps the value, for the UI.

use std::fmt;
use std::net::SocketAddr;

use serde::Serialize;

/// A value shown to the user but never logged (SR-32).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Secret<T>(pub T);

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(redacted)")
    }
}

/// Which side of a transfer this host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    /// We send.
    Send,
    /// We receive.
    Receive,
}

/// A discovery path, for "tell the user which layer failed" (PLAN.md, discovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryPath {
    /// mDNS `_playdrop._tcp` (protocol.md 8.1).
    Mdns,
    /// The multicast beacon (protocol.md 8.1).
    Beacon,
    /// Direct dial to last-known addresses (protocol.md 3.2).
    LastKnown,
}

/// What a pairing prompt asks the user (protocol.md 6.2, 6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairingPromptKind {
    /// Compare this six-digit SAS with the other screen, then accept or reject (6.2).
    Sas {
        /// The SAS. Never logged.
        sas: Secret<String>,
    },
    /// QR displayer: "Add <name> (<Short ID>)?" (6.3).
    ScannerConfirm,
}

/// One file's verified root, reported when a send completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileRoot {
    /// Manifest path, components joined with `/`.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Hex root (protocol.md 7.4), or `None` for bundled and empty files.
    pub root: Option<String>,
}

/// Everything the host tells its UI. Peers are named by Device ID (hex) and Short ID.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum HostEvent {
    /// The server is listening.
    Listening {
        /// Bound address.
        addr: SocketAddr,
        /// This device's ID.
        device_id: String,
        /// This device's Short ID.
        short_id: String,
    },
    /// A paired peer's session is up.
    PeerOnline {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Sanitised `hello` name (display only, SR-8).
        name: String,
        /// Where it is connected.
        addr: SocketAddr,
    },
    /// A paired peer's session ended.
    PeerOffline {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// The user must compare the SAS or confirm a scanner (protocol.md 6.2, 6.3). Answer
    /// with `Host::pairing_respond`.
    PairingPrompt {
        /// Device ID.
        peer: String,
        /// Short ID, shown next to the name (SR-8).
        short_id: String,
        /// Sanitised name from the authenticated `hello`.
        name: String,
        /// What to ask.
        prompt: PairingPromptKind,
    },
    /// Pairing succeeded; the peer is pinned with default grants.
    Paired {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Local label.
        name: String,
    },
    /// A pairing attempt ended without success.
    PairingFailed {
        /// Reason (a protocol.md 6.2 reject reason or a local cause).
        reason: String,
    },
    /// The peer no longer has us pinned (protocol.md 5.4): pair again.
    RemovedByPeer {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// The peer was unpaired, locally or by the peer (protocol.md 6.4).
    Unpaired {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// An unpaired device in its "Add device" window was seen (protocol.md 8.3 rule 1).
    AddDeviceSeen {
        /// Sanitised advertised name.
        name: String,
        /// Device type token.
        device_type: String,
        /// Address to pair with.
        addr: SocketAddr,
    },
    /// An offer waits for the user because the `files` grant is off (protocol.md 6.4,
    /// 7.3). Answer with `Host::respond_offer`.
    TransferOffer {
        /// Transfer ID.
        id: String,
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Number of files.
        files: usize,
        /// Total bytes.
        total_bytes: u64,
        /// Sender's note.
        note: Option<String>,
    },
    /// Transfer progress, at most a few times per second.
    TransferProgress {
        /// Transfer ID.
        id: String,
        /// Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// File bytes moved (sent) or verified (received).
        bytes: u64,
        /// Total bytes.
        total: u64,
        /// Recent rate.
        bytes_per_sec: u64,
    },
    /// A transfer completed with every file verified or skipped.
    TransferComplete {
        /// Transfer ID.
        id: String,
        /// Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// Total bytes.
        bytes: u64,
        /// Wall time in milliseconds.
        elapsed_ms: u64,
        /// Per-file roots (sender side).
        roots: Vec<FileRoot>,
        /// Bundle root, when a bundle was sent.
        bundle_root: Option<String>,
    },
    /// A transfer failed or was cancelled.
    TransferFailed {
        /// Transfer ID.
        id: String,
        /// Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// Why, for the user ("failed integrity check from <label>", SR-22).
        reason: String,
    },
    /// A clipboard push arrived and passed the `clipboardIn` grant (protocol.md 5.7).
    Clipboard {
        /// Device ID.
        peer: String,
        /// The text; `None` for an image blob. Never logged.
        text: Secret<Option<String>>,
        /// Marked sensitive by the sender (SR-24).
        sensitive: bool,
    },
    /// A `ping` arrived (protocol.md 5.6).
    Ping {
        /// Device ID.
        peer: String,
        /// Optional message.
        message: Option<String>,
    },
    /// A `find` arrived (protocol.md 5.6).
    Find {
        /// Device ID.
        peer: String,
        /// True to start ringing, false to stop.
        start: bool,
    },
    /// A `battery` frame arrived (protocol.md 5.8).
    Battery {
        /// Device ID.
        peer: String,
        /// Percent.
        level: u8,
        /// Charging.
        charging: bool,
        /// Low.
        low: bool,
    },
    /// A `presence` frame arrived (protocol.md 5.10).
    Presence {
        /// Device ID.
        peer: String,
        /// `active`, `idle`, `locked` or `asleep`.
        state: String,
    },
    /// A frame this build parses but does not act on yet (media and input are Phase 3/5,
    /// outbox is Phase 3/4).
    Unsupported {
        /// Device ID.
        peer: String,
        /// The frame type.
        frame_type: String,
    },
    /// A pinned peer was not reached within 10 s of start; names the discovery path that
    /// produced nothing and why (PLAN.md: tell the user which layer failed).
    DiscoveryDiagnostic {
        /// Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// The path.
        path: DiscoveryPath,
        /// What went wrong on it.
        failure: String,
    },
}
