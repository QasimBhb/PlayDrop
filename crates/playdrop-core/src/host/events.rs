//! Events the host reports to its UI.
//!
//! Some events carry text that must reach the user's screen but never a log: the SAS and
//! clipboard contents (SR-32). Those fields are [`Secret`], whose `Debug` is redacted, so a
//! stray `{:?}` in a log line cannot leak them. `Serialize` and `Deserialize` keep the
//! value, so a UI process can decode events received over IPC.

use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::session::types::{Input, MediaAction};
pub use crate::transfer::DeliveredFile;

use super::status::MediaState;

/// A value shown to the user but never logged (SR-32).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Secret<T>(pub T);

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("(redacted)")
    }
}

/// Bytes that travel through events and IPC as one standard base64 string (clipboard
/// images, `docs/core-api.md` section 5). `Debug` shows only the length.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Base64Bytes(pub Vec<u8>);

impl fmt::Debug for Base64Bytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Base64Bytes({} bytes)", self.0.len())
    }
}

impl From<Base64Bytes> for Vec<u8> {
    fn from(b: Base64Bytes) -> Self {
        b.0
    }
}

impl From<Vec<u8>> for Base64Bytes {
    fn from(v: Vec<u8>) -> Self {
        Self(v)
    }
}

impl Serialize for Base64Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Base64Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let s = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map(Base64Bytes)
            .map_err(serde::de::Error::custom)
    }
}

/// Which side of a transfer this host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferDirection {
    /// We send.
    Send,
    /// We receive.
    Receive,
}

/// A discovery path, for "tell the user which layer failed" (PLAN.md, discovery).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Most entries [`HostEvent::TransferOffer`] lists in `files`. *Choice:* 1,000 names is
/// more than any prompt shows, and keeps the event small enough for IPC.
pub const OFFER_FILES_MAX: usize = 1_000;

/// One file of an offer, for the accept prompt (gap 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferedFile {
    /// Manifest path for display, components joined with `/` (protocol.md 7.2). Already
    /// validated, but still untrusted text from the peer: show it, never use it as a path.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
}

/// One file's verified root, reported when a transfer completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRoot {
    /// Manifest path, components joined with `/`.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Hex root (protocol.md 7.4), or `None` for bundled and empty files.
    pub root: Option<String>,
}

/// Everything the host tells its UI. Peers are named by Device ID (hex) and Short ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
        /// Offered files in manifest order, for the prompt (gap 10). Capped at
        /// [`OFFER_FILES_MAX`] entries so a 100,000-file offer does not become a multi-MB
        /// event; `file_count` is always the full count.
        files: Vec<OfferedFile>,
        /// Number of files in the manifest.
        file_count: usize,
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
        /// Files finished so far: sent whole (send side) or verified (receive side).
        /// Bundled files count once the bundle is verified. Absent from older hosts.
        #[serde(default)]
        files_done: usize,
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
        /// Per-file roots in manifest order, both sides. Bundled and empty files have no
        /// root of their own (protocol.md 7.7).
        roots: Vec<FileRoot>,
        /// Bundle root, when a bundle was sent.
        bundle_root: Option<String>,
        /// Receive side: final path of every received file in manifest order, after any
        /// " (n)" collision rename (SR-18), for "Open". Empty on the send side.
        #[serde(default)]
        saved: Vec<PathBuf>,
        /// Receive side: the receive root the files were written beneath, for "Show in
        /// folder". `None` on the send side and in `Staged` mode.
        #[serde(default)]
        folder: Option<PathBuf>,
        /// Receive side, both modes: where every received file went, in manifest order
        /// (the final path in `Directory` mode, the sink's string in `Staged` mode). Empty
        /// on the send side (`docs/core-api.md` section 2).
        #[serde(default)]
        delivered: Vec<DeliveredFile>,
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
    /// A text clipboard push arrived and passed the `clipboardIn` grant (protocol.md 5.7).
    /// Text over 32 KiB was fetched from the sender's blob (5.15) and verified first.
    Clipboard {
        /// Device ID.
        peer: String,
        /// The text, always `Some` for text clips (the `Option` is kept for older UIs).
        /// Never logged.
        text: Secret<Option<String>>,
        /// Marked sensitive by the sender (SR-24).
        sensitive: bool,
    },
    /// An image clipboard arrived through a verified blob and passed the `clipboardIn`
    /// grant (protocol.md 5.7, 5.15).
    ClipboardImage {
        /// Device ID.
        peer: String,
        /// Image MIME type from the frame.
        mime: String,
        /// The image bytes, a base64 string on the wire. Never logged.
        data: Secret<Base64Bytes>,
        /// Marked sensitive by the sender (SR-24).
        sensitive: bool,
    },
    /// A peer's media player state (protocol.md 5.9), with the `media` grant.
    MediaState {
        /// Device ID.
        peer: String,
        /// The player's state.
        state: MediaState,
    },
    /// A peer asks this host to control one of its players (protocol.md 5.9), with the
    /// `media` grant.
    MediaCommand {
        /// Device ID.
        peer: String,
        /// Target player ID.
        player: String,
        /// What to do.
        action: MediaAction,
        /// Seek position in ms or volume 0 to 100.
        value: Option<u64>,
    },
    /// A peer holds outbox items for this host (protocol.md 5.11, 7.8); pull them with
    /// `Host::pull_outbox`.
    OutboxReady {
        /// Device ID.
        peer: String,
        /// Items waiting.
        count: u64,
        /// Total bytes waiting.
        bytes: u64,
        /// Item IDs.
        ids: Vec<String>,
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
    /// Remote input from a peer that holds the `input` grant, inside an input session it
    /// opened, while this desktop is unlocked (protocol.md 5.12, SR-25). The host's
    /// platform backend applies it. `Session { Stop }` always arrives last: when the peer
    /// stops, when its connection drops, after 10 idle minutes, on lock, and from
    /// `Host::stop_input_sessions`; release every key and button on it.
    Input {
        /// Device ID.
        peer: String,
        /// The frame.
        input: Input,
    },
    /// The peer answered one of our frames with an `error` (protocol.md 5.3, 5.14): most
    /// often `forbidden`, meaning the peer has no grant for that frame type from us (a
    /// desktop with remote input off for this phone). `frame_type` names the frame we
    /// sent when it is still known.
    PeerRefused {
        /// Device ID.
        peer: String,
        /// The frame type we sent, if known.
        frame_type: Option<String>,
        /// Error code, such as `forbidden` or `not_paired`.
        code: String,
        /// The peer's message, if any.
        message: Option<String>,
    },
    /// The target of an input session we control ended it (protocol.md 11.1 `error`
    /// `input_session_ended`): it locked, saw no input for 10 minutes, or its user hit the
    /// kill switch. The controlling UI leaves its input screen and shows `reason`.
    InputSessionEnded {
        /// Device ID.
        peer: String,
        /// Why, in the target's words.
        reason: String,
    },
    /// A frame this build parses but does not act on. No frame uses it since Phase 5
    /// (remote input is applied); kept so older IPC clients still decode the tag.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A UI process decodes what the daemon encodes (IPC), and secrets never reach `Debug`
    /// (SR-32).
    #[test]
    #[allow(clippy::too_many_lines, reason = "one sample per variant")]
    fn events_round_trip_and_secrets_stay_redacted() {
        let events = vec![
            HostEvent::PairingPrompt {
                peer: "ab".repeat(32),
                short_id: "k3qd-7f2m-xa9p-41bc".into(),
                name: "Phone".into(),
                prompt: PairingPromptKind::Sas {
                    sas: Secret("123456".into()),
                },
            },
            HostEvent::Clipboard {
                peer: "ab".repeat(32),
                text: Secret(Some("hunter2".into())),
                sensitive: true,
            },
            HostEvent::TransferOffer {
                id: "t".into(),
                peer: "ab".repeat(32),
                short_id: "s".into(),
                files: vec![OfferedFile {
                    path: "Docs/a.pdf".into(),
                    size: 3,
                }],
                file_count: 1,
                total_bytes: 3,
                note: None,
            },
            HostEvent::TransferComplete {
                id: "t".into(),
                peer: "ab".repeat(32),
                direction: TransferDirection::Receive,
                bytes: 3,
                elapsed_ms: 1,
                roots: vec![FileRoot {
                    path: "Docs/a.pdf".into(),
                    size: 3,
                    root: None,
                }],
                bundle_root: Some("00".into()),
                saved: vec![PathBuf::from("/home/u/Downloads/Docs/a.pdf")],
                folder: Some(PathBuf::from("/home/u/Downloads")),
                delivered: vec![DeliveredFile {
                    path: "Docs/a.pdf".into(),
                    location: "/home/u/Downloads/Docs/a.pdf".into(),
                }],
            },
            HostEvent::ClipboardImage {
                peer: "ab".repeat(32),
                mime: "image/png".into(),
                data: Secret(Base64Bytes(b"hunter2".to_vec())),
                sensitive: false,
            },
            HostEvent::MediaCommand {
                peer: "ab".repeat(32),
                player: "spotify".into(),
                action: MediaAction::Seek,
                value: Some(90_000),
            },
            HostEvent::OutboxReady {
                peer: "ab".repeat(32),
                count: 1,
                bytes: 10,
                ids: vec!["c1".into()],
            },
            HostEvent::DiscoveryDiagnostic {
                peer: "ab".repeat(32),
                short_id: "s".into(),
                path: DiscoveryPath::LastKnown,
                failure: "refused".into(),
            },
            HostEvent::Input {
                peer: "ab".repeat(32),
                input: Input::Key(crate::session::types::KeyInput {
                    key: Some("Enter".into()),
                    text: None,
                    mods: vec![crate::session::types::Modifier::Ctrl],
                    action: crate::session::types::KeyAction::Press,
                }),
            },
            HostEvent::PeerRefused {
                peer: "ab".repeat(32),
                frame_type: Some("input".into()),
                code: "forbidden".into(),
                message: None,
            },
            HostEvent::InputSessionEnded {
                peer: "ab".repeat(32),
                reason: "the desktop is locked".into(),
            },
        ];
        for ev in events {
            let json = serde_json::to_string(&ev).unwrap();
            let back: HostEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(back, ev, "{json}");
            let dbg = format!("{ev:?}");
            assert!(!dbg.contains("123456") && !dbg.contains("hunter2"), "{dbg}");
        }
        let json = serde_json::to_value(Base64Bytes(vec![0, 1, 2, 255])).unwrap();
        assert_eq!(json, "AAEC/w==");
        let json = r#"{"event":"clipboard","peer":"p","text":"x","sensitive":false}"#;
        let HostEvent::Clipboard { text, .. } = serde_json::from_str(json).unwrap() else {
            panic!("wrong variant");
        };
        assert_eq!(text.0.as_deref(), Some("x"));
    }
}
