//! The event stream as the bindings see it: one enum mirroring `playdrop_core::host::HostEvent`
//! with FFI-friendly fields. Secrets (SAS, clipboard contents) arrive as plain values; the
//! foreign side must show them and never log them (SR-32).

use playdrop_core::host::{HostEvent as CoreEvent, InputEndReason as CoreEndReason};

use crate::types::{
    DeliveredFile, DiscoveryPath, FileRoot, InputEvent, MediaAction, MediaState,
    MirroredNotification, OfferedFile, PairingPrompt, TransferDirection, TransferFailure,
};

/// Why an input session we control ended or never started (protocol.md 5.12; ffi 0.6.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum InputEndReason {
    /// The device has remote input turned off for us (its `input` grant). Also what a
    /// device from 0.10.0 or older reports this way.
    GrantOff,
    /// The device is locked.
    Locked,
    /// The device does not accept remote input at all (no `input.*` capability: no input
    /// backend, or the feature is hidden there).
    NotAccepted,
    /// The device's user ended it (Stop, the kill switch).
    StoppedByUser,
    /// No input for 10 minutes.
    IdleTimeout,
    /// The connection to the device ended, or there was none.
    ConnectionLost,
    /// Anything else, such as an older device with an unknown message.
    Other,
}

impl From<CoreEndReason> for InputEndReason {
    fn from(r: CoreEndReason) -> Self {
        match r {
            CoreEndReason::GrantOff => Self::GrantOff,
            CoreEndReason::Locked => Self::Locked,
            CoreEndReason::NotAccepted => Self::NotAccepted,
            CoreEndReason::StoppedByUser => Self::StoppedByUser,
            CoreEndReason::IdleTimeout => Self::IdleTimeout,
            CoreEndReason::ConnectionLost => Self::ConnectionLost,
            CoreEndReason::Other => Self::Other,
        }
    }
}

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
        /// Files finished so far (sent whole, or verified; bundled files when placed).
        files_done: u64,
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
        /// Reason for the user, a plain sentence that reads after "Sending to <device>
        /// failed: " on the send side.
        reason: String,
        /// Why, for the UI to act on (ffi 0.6.0).
        code: TransferFailure,
    },
    /// This device refused a transfer from `peer` because it would cross the free-space
    /// reserve (the larger of 2 GiB and 5% of the volume): at most one per peer per minute
    /// (ffi 0.6.0). Show "Refused a file from <label>: not enough free space".
    ReceiveRefusedNoSpace {
        /// Hex Device ID.
        peer: String,
        /// Bytes the transfer needs.
        offered_bytes: u64,
        /// Bytes free when it was refused.
        free_bytes: u64,
        /// Bytes kept free.
        reserve_bytes: u64,
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
    /// The peer mirrored a notification (protocol.md 5.16; ffi 0.5.0), with our
    /// `notificationsIn` grant for it. A later one with the same `key` replaces the copy.
    /// Never log it (SR-43).
    NotificationPosted {
        /// Hex Device ID.
        peer: String,
        /// The notification.
        notification: MirroredNotification,
    },
    /// A notification the peer mirrored went away there: close the copy (ffi 0.5.0).
    NotificationRemoved {
        /// Hex Device ID.
        peer: String,
        /// Opaque key from `NotificationPosted`. Never log it (SR-43).
        key: String,
    },
    /// The peer closed its copy of a notification we mirrored: dismiss the original. Only
    /// with our `notificationsOut` grant for the peer (ffi 0.5.0).
    NotificationDismiss {
        /// Hex Device ID.
        peer: String,
        /// Opaque key we sent. Never log it (SR-43).
        key: String,
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
    /// Remote input from a peer with the `input` grant, inside a session it opened, while
    /// this host is unlocked (protocol.md 5.12, SR-25). Only hosts started with
    /// `acceptsInput` see it. `Session { start: false }` always comes last (peer stop,
    /// connection drop, 10 idle minutes, lock, `stopInputSessions`): release every key
    /// and button on it.
    Input {
        /// Hex Device ID.
        peer: String,
        /// The frame.
        input: InputEvent,
    },
    /// The peer answered one of our frames with an error, usually `forbidden`: it has no
    /// grant for that frame type from us (remote input off for this phone on the
    /// desktop). `frame_type` is the frame we sent when still known, such as `input`.
    PeerRefused {
        /// Hex Device ID.
        peer: String,
        /// Our frame type, if known.
        frame_type: Option<String>,
        /// Error code (`forbidden`, `not_paired`, ...).
        code: String,
        /// The peer's message, if any.
        message: Option<String>,
    },
    /// An input session we control ended, or its start was refused (protocol.md 5.12):
    /// exactly one per `Session { start: true }` that went out or failed, unless we sent
    /// `start: false` first. Leave the input screen and say why from `code`; `reason` is the
    /// device's own English text (or ours when the end was seen here). `code` is new in ffi
    /// 0.6.0.
    InputSessionEnded {
        /// Hex Device ID.
        peer: String,
        /// Why, in words.
        reason: String,
        /// Why, for the UI to act on.
        code: InputEndReason,
    },
    /// A frame this build parses but does not act on. Unused since Phase 5.
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
                files_done,
            } => Self::TransferProgress {
                id,
                peer,
                direction: direction.into(),
                bytes,
                total,
                bytes_per_sec,
                files_done: files_done as u64,
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
                code,
            } => Self::TransferFailed {
                id,
                peer,
                direction: direction.into(),
                reason,
                code: code.into(),
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
            CoreEvent::NotificationPosted { peer, notification } => Self::NotificationPosted {
                peer,
                notification: notification.into(),
            },
            CoreEvent::NotificationRemoved { peer, key } => Self::NotificationRemoved { peer, key },
            CoreEvent::NotificationDismiss { peer, key } => Self::NotificationDismiss { peer, key },
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
            CoreEvent::Input { peer, input } => match InputEvent::from_wire(input) {
                Some(input) => Self::Input { peer, input },
                None => Self::Unsupported {
                    peer,
                    frame_type: "input".to_owned(),
                },
            },
            CoreEvent::PeerRefused {
                peer,
                frame_type,
                code,
                message,
            } => Self::PeerRefused {
                peer,
                frame_type,
                code,
                message,
            },
            CoreEvent::ReceiveRefusedNoSpace {
                peer,
                offered_bytes,
                free_bytes,
                reserve_bytes,
            } => Self::ReceiveRefusedNoSpace {
                peer,
                offered_bytes,
                free_bytes,
                reserve_bytes,
            },
            CoreEvent::InputSessionEnded { peer, reason, code } => Self::InputSessionEnded {
                peer,
                reason,
                code: code.into(),
            },
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The failure code and the no-space refusal cross the FFI (ffi 0.6.0).
    #[test]
    fn transfer_failure_and_no_space_refusal_cross() {
        let ev = HostEvent::from(CoreEvent::TransferFailed {
            id: "t1".into(),
            peer: "ab".repeat(32),
            direction: playdrop_core::host::TransferDirection::Send,
            reason: "the receiving device does not have enough free space".into(),
            code: playdrop_core::host::TransferFailure::NoSpace,
        });
        assert!(matches!(
            ev,
            HostEvent::TransferFailed {
                code: TransferFailure::NoSpace,
                ..
            }
        ));
        let ev = HostEvent::from(CoreEvent::ReceiveRefusedNoSpace {
            peer: "ab".repeat(32),
            offered_bytes: 3,
            free_bytes: 2,
            reserve_bytes: 1,
        });
        assert_eq!(
            ev,
            HostEvent::ReceiveRefusedNoSpace {
                peer: "ab".repeat(32),
                offered_bytes: 3,
                free_bytes: 2,
                reserve_bytes: 1,
            }
        );
    }

    /// The end reason crosses the FFI next to the text (ffi 0.6.0).
    #[test]
    fn input_session_ended_carries_its_reason() {
        let ev = HostEvent::from(CoreEvent::InputSessionEnded {
            peer: "ab".repeat(32),
            reason: "the desktop is locked".into(),
            code: CoreEndReason::Locked,
        });
        assert_eq!(
            ev,
            HostEvent::InputSessionEnded {
                peer: "ab".repeat(32),
                reason: "the desktop is locked".into(),
                code: InputEndReason::Locked,
            }
        );
        for (core, ffi) in [
            (CoreEndReason::GrantOff, InputEndReason::GrantOff),
            (CoreEndReason::NotAccepted, InputEndReason::NotAccepted),
            (CoreEndReason::StoppedByUser, InputEndReason::StoppedByUser),
            (CoreEndReason::IdleTimeout, InputEndReason::IdleTimeout),
            (
                CoreEndReason::ConnectionLost,
                InputEndReason::ConnectionLost,
            ),
            (CoreEndReason::Other, InputEndReason::Other),
        ] {
            assert_eq!(InputEndReason::from(core), ffi);
        }
    }
}
