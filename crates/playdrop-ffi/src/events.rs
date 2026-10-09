//! The event stream as the bindings see it: one enum mirroring `playdrop_core::host::HostEvent`
//! with FFI-friendly fields. Secrets (SAS, clipboard contents) arrive as plain values; the
//! foreign side must show them and never log them (SR-32).

use playdrop_core::host::HostEvent as CoreEvent;

use crate::types::{
    DeliveredFile, DiscoveryPath, FileRoot, MediaAction, MediaState, OfferedFile, PairingPrompt,
    TransferDirection,
};

/// Everything the host tells the app. Peers are hex Device IDs.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum HostEvent {
    /// The server is listening.
    Listening {
        /// Bound address as `ip:port`.
        addr: String,
        /// Our hex Device ID.
        device_id: String,
        /// Our Short ID.
        short_id: String,
    },
    /// A paired peer's session is up.
    PeerOnline {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Sanitised hello name, display only (SR-8).
        name: String,
        /// Remote address as `ip:port`.
        addr: String,
    },
    /// A paired peer's session ended.
    PeerOffline {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// Compare the SAS or confirm a scanner; answer with `pairingRespond`.
    PairingPrompt {
        /// Hex Device ID.
        peer: String,
        /// Short ID, shown next to the name (SR-8).
        short_id: String,
        /// Sanitised name from the authenticated hello.
        name: String,
        /// What to ask.
        prompt: PairingPrompt,
    },
    /// Pairing succeeded.
    Paired {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Local label.
        name: String,
    },
    /// Pairing ended without success.
    PairingFailed {
        /// Reason.
        reason: String,
    },
    /// The peer no longer has us pinned: pair again.
    RemovedByPeer {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// The peer was unpaired.
    Unpaired {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
    },
    /// A device in its "Add device" window was seen; list it, dial only on user pick.
    AddDeviceSeen {
        /// Sanitised name.
        name: String,
        /// Device type token.
        device_type: String,
        /// `ip:port`.
        addr: String,
    },
    /// An offer waits for the user (the `files` grant is off); answer with `respondOffer`.
    TransferOffer {
        /// Transfer ID.
        id: String,
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Files, capped at 1,000 entries.
        files: Vec<OfferedFile>,
        /// Full file count.
        file_count: u64,
        /// Total bytes.
        total_bytes: u64,
        /// Sender's note.
        note: Option<String>,
    },
    /// Progress, a few times per second.
    TransferProgress {
        /// Transfer ID.
        id: String,
        /// Hex Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// Bytes moved or verified.
        bytes: u64,
        /// Total bytes.
        total: u64,
        /// Recent rate.
        bytes_per_sec: u64,
    },
    /// Every file verified or skipped.
    TransferComplete {
        /// Transfer ID.
        id: String,
        /// Hex Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// Total bytes.
        bytes: u64,
        /// Wall time in ms.
        elapsed_ms: u64,
        /// Per-file roots.
        roots: Vec<FileRoot>,
        /// Bundle root.
        bundle_root: Option<String>,
        /// Receive side: where each file went.
        delivered: Vec<DeliveredFile>,
    },
    /// Failed or cancelled.
    TransferFailed {
        /// Transfer ID.
        id: String,
        /// Hex Device ID.
        peer: String,
        /// Our side.
        direction: TransferDirection,
        /// Reason for the user (SR-22 wording).
        reason: String,
    },
    /// Clipboard text from a peer that passed the `clipboardIn` grant. Never log it.
    Clipboard {
        /// Hex Device ID.
        peer: String,
        /// The text.
        text: String,
        /// Marked sensitive by the sender (SR-24): apply with `EXTRA_IS_SENSITIVE`.
        sensitive: bool,
    },
    /// A clipboard image from a peer. Never log it.
    ClipboardImage {
        /// Hex Device ID.
        peer: String,
        /// MIME type.
        mime: String,
        /// The bytes.
        data: Vec<u8>,
        /// Marked sensitive by the sender.
        sensitive: bool,
    },
    /// A ping: show a notification.
    Ping {
        /// Hex Device ID.
        peer: String,
        /// Optional message.
        message: Option<String>,
    },
    /// Ring at full volume until `start` is false or the user dismisses.
    Find {
        /// Hex Device ID.
        peer: String,
        /// Start or stop.
        start: bool,
    },
    /// The peer's battery.
    Battery {
        /// Hex Device ID.
        peer: String,
        /// Percent.
        level: u8,
        /// Charging.
        charging: bool,
        /// Low.
        low: bool,
    },
    /// The peer's presence: `active`, `idle`, `locked` or `asleep`.
    Presence {
        /// Hex Device ID.
        peer: String,
        /// State token.
        state: String,
    },
    /// A media player state from the peer.
    MediaState {
        /// Hex Device ID.
        peer: String,
        /// State.
        state: MediaState,
    },
    /// A media command from the peer (it holds the `media` grant).
    MediaCommand {
        /// Hex Device ID.
        peer: String,
        /// Player ID.
        player: String,
        /// Action.
        action: MediaAction,
        /// Seek ms or volume.
        value: Option<u64>,
    },
    /// The peer holds outbox items for us; call `pullOutbox` when ready.
    OutboxReady {
        /// Hex Device ID.
        peer: String,
        /// Items.
        count: u64,
        /// Bytes.
        bytes: u64,
        /// Item IDs.
        ids: Vec<String>,
    },
    /// A frame this build parses but does not act on (remote input, Phase 5).
    Unsupported {
        /// Hex Device ID.
        peer: String,
        /// Frame type.
        frame_type: String,
    },
    /// A pinned peer was not reached within 10 s of start; names the failing path.
    DiscoveryDiagnostic {
        /// Hex Device ID.
        peer: String,
        /// Short ID.
        short_id: String,
        /// Path.
        path: DiscoveryPath,
        /// What went wrong.
        failure: String,
    },
    /// An event added to the core after this binding was built (`HostEvent` is
    /// non-exhaustive). `description` is the redacted debug form.
    Unknown {
        /// Redacted debug form.
        description: String,
    },
}

impl From<CoreEvent> for HostEvent {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per event variant, read top to bottom"
    )]
    fn from(ev: CoreEvent) -> Self {
        match ev {
            CoreEvent::Listening {
                addr,
                device_id,
                short_id,
            } => Self::Listening {
                addr: addr.to_string(),
                device_id,
                short_id,
            },
            CoreEvent::PeerOnline {
                peer,
                short_id,
                name,
                addr,
            } => Self::PeerOnline {
                peer,
                short_id,
                name,
                addr: addr.to_string(),
            },
            CoreEvent::PeerOffline { peer, short_id } => Self::PeerOffline { peer, short_id },
            CoreEvent::PairingPrompt {
                peer,
                short_id,
                name,
                prompt,
            } => Self::PairingPrompt {
                peer,
                short_id,
                name,
                prompt: prompt.into(),
            },
            CoreEvent::Paired {
                peer,
                short_id,
                name,
            } => Self::Paired {
                peer,
                short_id,
                name,
            },
            CoreEvent::PairingFailed { reason } => Self::PairingFailed { reason },
            CoreEvent::RemovedByPeer { peer, short_id } => Self::RemovedByPeer { peer, short_id },
            CoreEvent::Unpaired { peer, short_id } => Self::Unpaired { peer, short_id },
            CoreEvent::AddDeviceSeen {
                name,
                device_type,
                addr,
            } => Self::AddDeviceSeen {
                name,
                device_type,
                addr: addr.to_string(),
            },
            CoreEvent::TransferOffer {
                id,
                peer,
                short_id,
                files,
                file_count,
                total_bytes,
                note,
            } => Self::TransferOffer {
                id,
                peer,
                short_id,
                files: files.into_iter().map(Into::into).collect(),
                file_count: file_count as u64,
                total_bytes,
                note,
            },
            CoreEvent::TransferProgress {
                id,
                peer,
                direction,
                bytes,
                total,
                bytes_per_sec,
            } => Self::TransferProgress {
                id,
                peer,
                direction: direction.into(),
                bytes,
                total,
                bytes_per_sec,
            },
            CoreEvent::TransferComplete {
                id,
                peer,
                direction,
                bytes,
                elapsed_ms,
                roots,
                bundle_root,
                delivered,
                ..
            } => Self::TransferComplete {
                id,
                peer,
                direction: direction.into(),
                bytes,
                elapsed_ms,
                roots: roots.into_iter().map(Into::into).collect(),
                bundle_root,
                delivered: delivered.into_iter().map(Into::into).collect(),
            },
            CoreEvent::TransferFailed {
                id,
                peer,
                direction,
                reason,
            } => Self::TransferFailed {
                id,
                peer,
                direction: direction.into(),
                reason,
            },
            CoreEvent::Clipboard {
                peer,
                text,
                sensitive,
            } => Self::Clipboard {
                peer,
                text: text.0.unwrap_or_default(),
                sensitive,
            },
            CoreEvent::ClipboardImage {
                peer,
                mime,
                data,
                sensitive,
            } => Self::ClipboardImage {
                peer,
                mime,
                data: data.0.into(),
                sensitive,
            },
            CoreEvent::Ping { peer, message } => Self::Ping { peer, message },
            CoreEvent::Find { peer, start } => Self::Find { peer, start },
            CoreEvent::Battery {
                peer,
                level,
                charging,
                low,
            } => Self::Battery {
                peer,
                level,
                charging,
                low,
            },
            CoreEvent::Presence { peer, state } => Self::Presence { peer, state },
            CoreEvent::MediaState { peer, state } => Self::MediaState {
                peer,
                state: state.into(),
            },
            CoreEvent::MediaCommand {
                peer,
                player,
                action,
                value,
            } => Self::MediaCommand {
                peer,
                player,
                action: action.into(),
                value,
            },
            CoreEvent::OutboxReady {
                peer,
                count,
                bytes,
                ids,
            } => Self::OutboxReady {
                peer,
                count,
                bytes,
                ids,
            },
            CoreEvent::Unsupported { peer, frame_type } => Self::Unsupported { peer, frame_type },
            CoreEvent::DiscoveryDiagnostic {
                peer,
                short_id,
                path,
                failure,
            } => Self::DiscoveryDiagnostic {
                peer,
                short_id,
                path: path.into(),
                failure,
            },
            other => Self::Unknown {
                description: format!("{other:?}"),
            },
        }
    }
}
