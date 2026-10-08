//! Session frame types, protocol.md 5.3 to 5.14 and 6.2 to 6.4.
//!
//! [`Frame`] is the body of a frame, internally tagged on `t`. The envelope fields `v`, `seq`
//! and `re` live in [`Envelope`](super::frame::Envelope) and are added or stripped by the codec,
//! so a `Frame` parses from the spec's examples even though they omit `seq`.
//!
//! Every closed vocabulary (capabilities, actions, states, reasons, error codes) carries an
//! `Unknown(String)` variant: unknown values are kept and ignored, never fatal (protocol.md
//! section 4).

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::util::{b64_decode, b64_encode, hex_encode};

/// Frame version this crate writes and the highest it understands for every v1 frame type
/// (protocol.md 5.2).
pub const FRAME_VERSION: u32 = 1;

/// Maximum `ping.message` length in bytes (protocol.md 5.6).
pub const PING_MESSAGE_MAX: usize = 256;

/// Maximum inline clipboard text in bytes; longer text goes as a blob (protocol.md 5.7).
pub const CLIPBOARD_INLINE_MAX: usize = 32 * 1024;

/// A closed wire vocabulary with an `Unknown` passthrough.
trait WireStr: Sized {
    fn from_wire(s: &str) -> Self;
    fn as_wire(&self) -> &str;
}

struct WireStrVisitor<T>(PhantomData<T>);

impl<T: WireStr> serde::de::Visitor<'_> for WireStrVisitor<T> {
    type Value = T;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a string")
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<T, E> {
        Ok(T::from_wire(v))
    }
}

/// Declares a string enum with exact wire spellings and an `Unknown(String)` variant.
macro_rules! wire_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this version does not know. Kept verbatim and ignored (protocol.md
            /// section 4).
            Unknown(String),
        }

        impl $name {
            /// The exact wire spelling.
            pub fn as_str(&self) -> &str {
                match self {
                    $( Self::$variant => $wire, )+
                    Self::Unknown(s) => s,
                }
            }

            /// Parses a wire spelling; unrecognised values become `Unknown`.
            pub fn from_wire(s: &str) -> Self {
                match s {
                    $( $wire => Self::$variant, )+
                    other => Self::Unknown(other.to_owned()),
                }
            }

            /// False for the `Unknown` passthrough.
            pub fn is_known(&self) -> bool {
                !matches!(self, Self::Unknown(_))
            }
        }

        impl WireStr for $name {
            fn from_wire(s: &str) -> Self {
                $name::from_wire(s)
            }
            fn as_wire(&self) -> &str {
                self.as_str()
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_wire())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                d.deserialize_str(WireStrVisitor::<Self>(PhantomData))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

wire_enum! {
    /// A capability string from protocol.md section 4. Hosts list what they accept (`in`) and
    /// what they may send (`out`).
    pub enum Capability {
        /// `ping`: ping and pong frames.
        Ping => "ping",
        /// `find`: ring the device.
        Find => "find",
        /// `clipboard`: text clipboard.
        Clipboard => "clipboard",
        /// `clipboard.image`: image clipboard via blobs.
        ClipboardImage => "clipboard.image",
        /// `battery`: battery reports.
        Battery => "battery",
        /// `media.state`: media player state.
        MediaState => "media.state",
        /// `media.control`: media player commands.
        MediaControl => "media.control",
        /// `presence`: active, idle, locked, asleep.
        Presence => "presence",
        /// `input.pointer`: remote pointer.
        InputPointer => "input.pointer",
        /// `input.key`: remote keyboard.
        InputKey => "input.key",
        /// `input.presenter`: presenter remote.
        InputPresenter => "input.presenter",
        /// `transfer`: file transfer.
        Transfer => "transfer",
        /// `transfer.bundle`: tar bundle for small files.
        TransferBundle => "transfer.bundle",
        /// `outbox`: queued items the peer pulls.
        Outbox => "outbox",
        /// `blobs`: `GET /v1/blobs/{id}`.
        Blobs => "blobs",
    }
}

wire_enum! {
    /// `hello.type`, protocol.md 5.4.
    pub enum DeviceType {
        /// `desktop`
        Desktop => "desktop",
        /// `laptop`
        Laptop => "laptop",
        /// `phone`
        Phone => "phone",
        /// `tablet`
        Tablet => "tablet",
        /// `tv`
        Tv => "tv",
    }
}

wire_enum! {
    /// `hello.trust`, protocol.md 5.4. `unpaired` is the value for "sender has no pin for the
    /// peer"; the spec names only `paired`.
    pub enum Trust {
        /// The sender has the peer pinned.
        Paired => "paired",
        /// The sender has no pin for the peer.
        Unpaired => "unpaired",
    }
}

wire_enum! {
    /// `find.action`, protocol.md 5.6.
    pub enum FindAction {
        /// Start ringing.
        Start => "start",
        /// Stop ringing.
        Stop => "stop",
    }
}

wire_enum! {
    /// `clipboard.kind`, protocol.md 5.7.
    pub enum ClipboardKind {
        /// Text, inline or as a blob.
        Text => "text",
        /// Image, always a blob.
        Image => "image",
    }
}

wire_enum! {
    /// Media actions for `media.can` and `media` commands, protocol.md 5.9.
    pub enum MediaAction {
        /// `play`
        Play => "play",
        /// `pause`
        Pause => "pause",
        /// `playPause`
        PlayPause => "playPause",
        /// `stop`
        Stop => "stop",
        /// `next`
        Next => "next",
        /// `previous`
        Previous => "previous",
        /// `seek`, `value` is the absolute position in ms.
        Seek => "seek",
        /// `volume`, `value` is 0 to 100.
        Volume => "volume",
    }
}

wire_enum! {
    /// `presence.state`, protocol.md 5.10.
    pub enum PresenceState {
        /// `active`
        Active => "active",
        /// `idle`
        Idle => "idle",
        /// `locked`
        Locked => "locked",
        /// `asleep`: the peer expects a drop.
        Asleep => "asleep",
    }
}

wire_enum! {
    /// Input session ops, protocol.md 5.12.
    pub enum SessionOp {
        /// `start`
        Start => "start",
        /// `stop`: release every key and button.
        Stop => "stop",
    }
}

wire_enum! {
    /// Pointer buttons, protocol.md 5.12.
    pub enum PointerButton {
        /// `left`
        Left => "left",
        /// `right`
        Right => "right",
        /// `middle`
        Middle => "middle",
    }
}

wire_enum! {
    /// Pointer button actions, protocol.md 5.12.
    pub enum ButtonAction {
        /// `down`
        Down => "down",
        /// `up`
        Up => "up",
        /// `click`
        Click => "click",
        /// `double`
        Double => "double",
    }
}

wire_enum! {
    /// Scroll units, protocol.md 5.12.
    pub enum ScrollUnit {
        /// `line`
        Line => "line",
        /// `pixel`
        Pixel => "pixel",
    }
}

wire_enum! {
    /// Key actions, protocol.md 5.12. The spec shows `press`; `down` and `up` cover held keys.
    pub enum KeyAction {
        /// `press`: down then up.
        Press => "press",
        /// `down`
        Down => "down",
        /// `up`
        Up => "up",
    }
}

wire_enum! {
    /// Key modifiers, protocol.md 5.12.
    pub enum Modifier {
        /// `ctrl`
        Ctrl => "ctrl",
        /// `alt`
        Alt => "alt",
        /// `shift`
        Shift => "shift",
        /// `meta`
        Meta => "meta",
    }
}

wire_enum! {
    /// Presenter actions, protocol.md 5.12.
    pub enum PresenterAction {
        /// `next`
        Next => "next",
        /// `previous`
        Previous => "previous",
        /// `pointerOn`
        PointerOn => "pointerOn",
        /// `pointerOff`
        PointerOff => "pointerOff",
        /// `blackout`
        Blackout => "blackout",
    }
}

wire_enum! {
    /// `transfer.state`, protocol.md 5.13.
    pub enum TransferState {
        /// The offer was accepted.
        Accepted => "accepted",
        /// The offer was declined.
        Declined => "declined",
        /// The receiver cancelled the transfer.
        Cancelled => "cancelled",
        /// The transfer failed at the receiver.
        Failed => "failed",
    }
}

wire_enum! {
    /// `pair_request.method`, protocol.md 6.2 and 6.3.
    pub enum PairMethod {
        /// Commit-then-reveal SAS comparison.
        Sas => "sas",
        /// QR one-time secret.
        Qr => "qr",
    }
}

wire_enum! {
    /// `pair_reject.reason`, exactly the list in protocol.md 6.2.
    pub enum RejectReason {
        /// The local user declined.
        User => "user",
        /// The attempt passed 30 s.
        Timeout => "timeout",
        /// `pair_reveal` did not match the commit.
        CommitMismatch => "commit_mismatch",
        /// Another pairing is in progress.
        Busy => "busy",
        /// Too many requests from this source.
        RateLimited => "rate_limited",
        /// A QR proof failed.
        QrProofInvalid => "qr_proof_invalid",
        /// The QR secret expired or was used.
        QrExpired => "qr_expired",
    }
}

wire_enum! {
    /// Error codes, protocol.md section 9.
    pub enum ErrorCode {
        /// Malformed JSON or missing field.
        BadRequest => "bad_request",
        /// Manifest path breaks 7.2.
        PathInvalid => "path_invalid",
        /// Missing or wrong `pd-token`.
        BadToken => "bad_token",
        /// Needs a paired connection.
        NotPaired => "not_paired",
        /// Grant missing for this device.
        Forbidden => "forbidden",
        /// Offer declined.
        Declined => "declined",
        /// Unknown ID, or an ID bound to another device.
        NotFound => "not_found",
        /// Second session stream, or changed manifest under a known ID.
        Conflict => "conflict",
        /// Range start is neither the verified offset nor 0.
        OffsetMismatch => "offset_mismatch",
        /// Transfer was cancelled.
        Cancelled => "cancelled",
        /// Offer, blob or outbox item expired.
        Expired => "expired",
        /// Manifest over limits.
        TooLarge => "too_large",
        /// Range not chunk-aligned or past end.
        RangeInvalid => "range_invalid",
        /// A chunk failed its BLAKE3 check.
        ChunkHashMismatch => "chunk_hash_mismatch",
        /// Whole-file root differs.
        RootMismatch => "root_mismatch",
        /// Tar member differs from manifest.
        BundleMismatch => "bundle_mismatch",
        /// Rate or concurrency limit exceeded.
        RateLimited => "rate_limited",
        /// Unexpected failure.
        Internal => "internal",
        /// Would cross the free-space reserve.
        NoSpace => "no_space",
        /// Cancel reason: source size changed.
        SourceChanged => "source_changed",
        /// `hello.protocolVersion` differs from ALPN; close.
        UnsupportedVersion => "unsupported_version",
        /// Frame `v` too new.
        UnsupportedFrameVersion => "unsupported_frame_version",
        /// `hello.deviceId` differs from the certificate; close.
        IdentityMismatch => "identity_mismatch",
        /// GOAWAY debug data (3.3).
        DuplicateConnection => "duplicate_connection",
    }
}

/// Per-device grants, protocol.md 6.4 (SR-14). Grants change only from the local UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrantKind {
    /// `files`: auto-accept offers.
    Files,
    /// `clipboardIn`: apply clipboards from the peer.
    ClipboardIn,
    /// `clipboardOut`: send local clipboards to the peer.
    ClipboardOut,
    /// `input`: apply remote input. Off by default.
    Input,
    /// `media`: media state and control.
    Media,
    /// `find`: ring this device.
    Find,
}

/// Capability lists in `hello` and `capabilities` (protocol.md section 4).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caps {
    /// `in`: what this host accepts.
    #[serde(rename = "in", default)]
    pub incoming: Vec<Capability>,
    /// `out`: what this host may send.
    #[serde(rename = "out", default)]
    pub outgoing: Vec<Capability>,
}

/// `hello`, protocol.md 5.4. Sent first by both sides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Hello {
    /// Must equal the ALPN-selected version (protocol.md section 4).
    pub protocol_version: u32,
    /// Sender's Device ID; must match the TLS certificate.
    pub device_id: String,
    /// Display name. Never feeds a trust decision; sanitise before display (SR-8).
    pub name: String,
    /// Device form factor.
    #[serde(rename = "type")]
    pub device_type: DeviceType,
    /// Sender's listening port.
    pub port: u16,
    /// Application and version, for diagnostics.
    pub app: String,
    /// Keep-alive interval in seconds (protocol.md 3.4).
    pub keep_alive: u32,
    /// Whether the sender has the peer pinned.
    pub trust: Trust,
    /// Capability lists.
    pub caps: Caps,
}

/// `capabilities`, protocol.md 5.5. Full replacement of the sender's lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilitiesFrame {
    /// The new lists.
    pub caps: Caps,
}

/// `announce_key`, protocol.md 8.2 (SR-5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnnounceKey {
    /// Key ID, rising by 1 per rotation.
    pub kid: u64,
    /// The 256-bit announce key, base64url without padding.
    pub key: String,
}

impl AnnounceKey {
    /// Builds the frame from a raw key.
    pub fn new(kid: u64, key: &[u8; 32]) -> Self {
        Self {
            kid,
            key: b64_encode(key),
        }
    }

    /// Decodes `key`; `None` if it is not 32 bytes of base64url.
    pub fn key_bytes(&self) -> Option<[u8; 32]> {
        b64_decode::<32>(&self.key)
    }
}

/// `ping`, protocol.md 5.6. Always answered with `pong`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ping {
    /// Optional notification text, at most [`PING_MESSAGE_MAX`] bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Ping {
    /// Builds a ping, cutting `message` to [`PING_MESSAGE_MAX`] bytes at a character boundary.
    pub fn new(message: Option<&str>) -> Self {
        Self {
            message: message.map(|m| truncate_bytes(m, PING_MESSAGE_MAX).to_owned()),
        }
    }
}

/// `find`, protocol.md 5.6.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Find {
    /// Start or stop ringing.
    pub action: FindAction,
}

/// Reference to a blob, protocol.md 5.15.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobRef {
    /// Lowercase hex BLAKE3 of the bytes.
    pub id: String,
    /// Size in bytes.
    pub size: u64,
}

/// `clipboard`, protocol.md 5.7. Exactly one of `text` and `blob` is set; images are always
/// blobs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clipboard {
    /// Text or image.
    pub kind: ClipboardKind,
    /// MIME type.
    pub mime: String,
    /// Lowercase hex BLAKE3 of the content; see [`clipboard_text_hash`].
    pub hash: String,
    /// Inline text, at most [`CLIPBOARD_INLINE_MAX`] bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Blob for long text and images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob: Option<BlobRef>,
    /// A password-manager clip sent by hand (SR-24).
    #[serde(default)]
    pub sensitive: bool,
}

/// Clipboard text hash, protocol.md 5.7: lowercase hex BLAKE3 of the UTF-8 text after
/// converting CRLF and lone CR to LF.
pub fn clipboard_text_hash(text: &str) -> String {
    let mut hasher = blake3::Hasher::new();
    let mut rest = text.as_bytes();
    while let Some(pos) = rest.iter().position(|&b| b == b'\r') {
        hasher.update(&rest[..pos]);
        hasher.update(b"\n");
        rest = &rest[pos + 1..];
        if rest.first() == Some(&b'\n') {
            rest = &rest[1..];
        }
    }
    hasher.update(rest);
    hex_encode(hasher.finalize().as_bytes())
}

/// `battery`, protocol.md 5.8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Battery {
    /// Charge level, 0 to 100.
    pub level: u8,
    /// Charging now.
    pub charging: bool,
    /// Below the platform's low threshold.
    pub low: bool,
}

/// `media`, protocol.md 5.9, tagged on `op`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Media {
    /// `op: state`, player state from the device that has the player.
    State(MediaStateFrame),
    /// `op: command`, a control request to the device that has the player.
    Command(MediaCommand),
}

/// `media` `op: state`, protocol.md 5.9. Coalesced to 2 per second per player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStateFrame {
    /// Player ID.
    pub player: String,
    /// Player display name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Playing now.
    pub playing: bool,
    /// Track title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Track artist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Track album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Track duration in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Position in ms at `position_at`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_ms: Option<u64>,
    /// Unix ms at which `position_ms` was sampled; receivers extrapolate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_at: Option<u64>,
    /// Volume, 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<u8>,
    /// Actions the player supports.
    #[serde(default)]
    pub can: Vec<MediaAction>,
    /// Album art blob.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub art: Option<BlobRef>,
}

/// `media` `op: command`, protocol.md 5.9.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaCommand {
    /// Target player ID.
    pub player: String,
    /// What to do.
    pub action: MediaAction,
    /// Seek position in ms or volume 0 to 100.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<u64>,
}

/// `presence`, protocol.md 5.10.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Presence {
    /// New state.
    pub state: PresenceState,
    /// Free-form platform reason, for diagnostics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Expected time until the device is back, in ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_back_ms: Option<u64>,
}

/// `outbox_ready`, protocol.md 5.11.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxReady {
    /// Items waiting.
    pub count: u64,
    /// Total bytes waiting.
    pub bytes: u64,
    /// Item IDs.
    #[serde(default)]
    pub ids: Vec<String>,
}

/// `input`, protocol.md 5.12, tagged on `kind`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Input {
    /// Start or stop an input session.
    Session {
        /// `start` or `stop`.
        op: SessionOp,
    },
    /// Pointer events.
    Pointer(PointerInput),
    /// A key or literal text.
    Key(KeyInput),
    /// Presenter remote.
    Presenter {
        /// What to do.
        action: PresenterAction,
    },
}

/// Pointer input, tagged on `op`, protocol.md 5.12.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PointerInput {
    /// Relative move.
    Move {
        /// Horizontal delta.
        dx: f64,
        /// Vertical delta.
        dy: f64,
    },
    /// Button event.
    Button {
        /// Which button.
        button: PointerButton,
        /// What happened.
        action: ButtonAction,
    },
    /// Scroll.
    Scroll {
        /// Horizontal amount.
        #[serde(default)]
        dx: f64,
        /// Vertical amount.
        #[serde(default)]
        dy: f64,
        /// `line` or `pixel`; absent means `line`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<ScrollUnit>,
    },
}

/// Key input, protocol.md 5.12. Exactly one of `key` and `text` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInput {
    /// W3C UI Events `key` value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// A literal string to type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Held modifiers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mods: Vec<Modifier>,
    /// What happened to the key; absent means `press`.
    #[serde(default = "default_key_action")]
    pub action: KeyAction,
}

fn default_key_action() -> KeyAction {
    KeyAction::Press
}

/// `transfer`, protocol.md 5.13. Pushed by the receiver.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferFrame {
    /// Transfer ID.
    pub id: String,
    /// New state.
    pub state: TransferState,
    /// Why, for cancel and fail (an error code or `user`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// `pair_request`, protocol.md 6.2 (`commit`) and 6.3 (`proof`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    /// `sas` or `qr`.
    pub method: PairMethod,
    /// SAS: hex BLAKE3 of `nonce_i`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// QR: hex `proof_b`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

/// `pair_response`, protocol.md 6.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    /// `nonce_r`, base64url without padding.
    pub nonce: String,
}

/// `pair_reveal`, protocol.md 6.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairReveal {
    /// `nonce_i`, base64url without padding.
    pub nonce: String,
}

/// `pair_confirm`, protocol.md 6.2 (no fields) and 6.3 (`proof_a`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairConfirm {
    /// QR: hex `proof_a`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proof: Option<String>,
}

/// `pair_reject`, protocol.md 6.2.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairReject {
    /// Why.
    pub reason: RejectReason,
}

/// `error`, protocol.md 5.14. `re` lives in the envelope and is absent for unsolicited errors.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorFrame {
    /// Section 9 code.
    pub code: ErrorCode,
    /// Human-readable detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// A session frame body, tagged on `t` (protocol.md 5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Frame {
    /// 5.4
    Hello(Hello),
    /// 5.5
    Capabilities(CapabilitiesFrame),
    /// 8.2
    AnnounceKey(AnnounceKey),
    /// 5.6
    Ping(Ping),
    /// 5.6, `re` is the ping's `seq`.
    Pong,
    /// 5.6
    Find(Find),
    /// 5.7
    Clipboard(Clipboard),
    /// 5.8
    Battery(Battery),
    /// 5.9
    Media(Media),
    /// 5.10
    Presence(Presence),
    /// 5.11
    OutboxReady(OutboxReady),
    /// 5.12
    Input(Input),
    /// 5.13
    Transfer(TransferFrame),
    /// 6.2, 6.3
    PairRequest(PairRequest),
    /// 6.2
    PairResponse(PairResponse),
    /// 6.2
    PairReveal(PairReveal),
    /// 6.2, 6.3
    PairConfirm(PairConfirm),
    /// 6.2
    PairReject(PairReject),
    /// 6.4
    Unpair,
    /// 5.14
    Error(ErrorFrame),
    /// A `t` this version does not know. Ignored by the receiver (protocol.md section 4);
    /// never encoded.
    #[serde(skip)]
    Unknown {
        /// The unknown type string.
        t: String,
    },
}

/// Which capabilities a frame needs in the peer's `in` list and the sender's `out` list
/// (protocol.md section 4 and 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapRequirement {
    /// No capability needed.
    None,
    /// Every listed capability is needed.
    AllOf(&'static [Capability]),
    /// Any one of the listed capabilities is enough (the `input` session frame).
    AnyOf(&'static [Capability]),
}

const CAP_PING: &[Capability] = &[Capability::Ping];
const CAP_FIND: &[Capability] = &[Capability::Find];
const CAP_CLIPBOARD: &[Capability] = &[Capability::Clipboard];
const CAP_CLIPBOARD_IMAGE: &[Capability] = &[Capability::Clipboard, Capability::ClipboardImage];
const CAP_BATTERY: &[Capability] = &[Capability::Battery];
const CAP_MEDIA_STATE: &[Capability] = &[Capability::MediaState];
const CAP_MEDIA_CONTROL: &[Capability] = &[Capability::MediaControl];
const CAP_PRESENCE: &[Capability] = &[Capability::Presence];
const CAP_OUTBOX: &[Capability] = &[Capability::Outbox];
const CAP_POINTER: &[Capability] = &[Capability::InputPointer];
const CAP_KEY: &[Capability] = &[Capability::InputKey];
const CAP_PRESENTER: &[Capability] = &[Capability::InputPresenter];
const CAP_INPUT_ANY: &[Capability] = &[
    Capability::InputPointer,
    Capability::InputKey,
    Capability::InputPresenter,
];
const CAP_TRANSFER: &[Capability] = &[Capability::Transfer];

/// Every `t` known to this version, protocol.md 5.3.
pub const KNOWN_TYPES: &[&str] = &[
    "hello",
    "capabilities",
    "announce_key",
    "ping",
    "pong",
    "find",
    "clipboard",
    "battery",
    "media",
    "presence",
    "outbox_ready",
    "input",
    "transfer",
    "pair_request",
    "pair_response",
    "pair_reveal",
    "pair_confirm",
    "pair_reject",
    "unpair",
    "error",
];

impl Frame {
    /// The wire `t` of this frame.
    pub fn type_name(&self) -> &str {
        match self {
            Frame::Hello(_) => "hello",
            Frame::Capabilities(_) => "capabilities",
            Frame::AnnounceKey(_) => "announce_key",
            Frame::Ping(_) => "ping",
            Frame::Pong => "pong",
            Frame::Find(_) => "find",
            Frame::Clipboard(_) => "clipboard",
            Frame::Battery(_) => "battery",
            Frame::Media(_) => "media",
            Frame::Presence(_) => "presence",
            Frame::OutboxReady(_) => "outbox_ready",
            Frame::Input(_) => "input",
            Frame::Transfer(_) => "transfer",
            Frame::PairRequest(_) => "pair_request",
            Frame::PairResponse(_) => "pair_response",
            Frame::PairReveal(_) => "pair_reveal",
            Frame::PairConfirm(_) => "pair_confirm",
            Frame::PairReject(_) => "pair_reject",
            Frame::Unpair => "unpair",
            Frame::Error(_) => "error",
            Frame::Unknown { t } => t,
        }
    }

    /// Highest frame version understood for type `t`, or `None` if `t` is unknown
    /// (protocol.md 5.2).
    pub fn supported_version(t: &str) -> Option<u32> {
        KNOWN_TYPES.contains(&t).then_some(FRAME_VERSION)
    }

    /// Builds an `error` frame body (protocol.md 5.14).
    pub fn error(code: ErrorCode, message: Option<&str>) -> Self {
        Frame::Error(ErrorFrame {
            code,
            message: message.map(str::to_owned),
        })
    }

    /// Structural checks serde cannot express. A failure makes the frame malformed, which
    /// closes the connection (protocol.md 5.2, SR-16).
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Frame::Clipboard(c) => match (&c.kind, &c.text, &c.blob) {
                (_, Some(_), Some(_)) => Err("clipboard has both text and blob"),
                (_, None, None) => Err("clipboard has neither text nor blob"),
                (ClipboardKind::Image, Some(_), None) => Err("image clipboard must be a blob"),
                (_, Some(t), None) if t.len() > CLIPBOARD_INLINE_MAX => {
                    Err("inline clipboard text over 32 KiB")
                }
                _ => Ok(()),
            },
            Frame::Input(Input::Key(k)) if k.key.is_some() == k.text.is_some() => {
                Err("key input needs exactly one of key and text")
            }
            Frame::PairRequest(r) => match r.method {
                PairMethod::Sas if r.commit.is_none() => Err("sas pair_request without commit"),
                PairMethod::Qr if r.proof.is_none() => Err("qr pair_request without proof"),
                _ => Ok(()),
            },
            _ => Ok(()),
        }
    }

    /// Grant the receiver must hold for the authenticated sender before applying this frame,
    /// exactly the "Grant at receiver" column of protocol.md 5.3 (SR-13, SR-14).
    pub fn required_grant(&self) -> Option<GrantKind> {
        match self {
            Frame::Find(_) => Some(GrantKind::Find),
            Frame::Clipboard(_) => Some(GrantKind::ClipboardIn),
            Frame::Media(_) => Some(GrantKind::Media),
            Frame::Input(_) => Some(GrantKind::Input),
            _ => None,
        }
    }

    /// Grant the sender must hold for the peer before sending this frame: `clipboardOut` for
    /// `clipboard` (protocol.md 5.7).
    pub fn required_send_grant(&self) -> Option<GrantKind> {
        match self {
            Frame::Clipboard(_) => Some(GrantKind::ClipboardOut),
            _ => None,
        }
    }

    /// Whether the frame is allowed on an unpaired connection, the "Unpaired" column of
    /// protocol.md 5.3 (SR-6). Others get `not_paired`. Unknown types return false; the
    /// receiver ignores them before this check.
    pub fn allowed_unpaired(&self) -> bool {
        matches!(
            self,
            Frame::Hello(_)
                | Frame::Ping(_)
                | Frame::Pong
                | Frame::PairRequest(_)
                | Frame::PairResponse(_)
                | Frame::PairReveal(_)
                | Frame::PairConfirm(_)
                | Frame::PairReject(_)
                | Frame::Error(_)
        )
    }

    /// Capabilities this frame needs, the "Capability" column of protocol.md 5.3 refined by
    /// the section 4 list: image clipboards need `clipboard.image`, media state and commands
    /// need `media.state` and `media.control`, input needs the matching `input.*`.
    pub fn required_capabilities(&self) -> CapRequirement {
        match self {
            Frame::Ping(_) | Frame::Pong => CapRequirement::AllOf(CAP_PING),
            Frame::Find(_) => CapRequirement::AllOf(CAP_FIND),
            Frame::Clipboard(c) if c.kind == ClipboardKind::Image => {
                CapRequirement::AllOf(CAP_CLIPBOARD_IMAGE)
            }
            Frame::Clipboard(_) => CapRequirement::AllOf(CAP_CLIPBOARD),
            Frame::Battery(_) => CapRequirement::AllOf(CAP_BATTERY),
            Frame::Media(Media::State(_)) => CapRequirement::AllOf(CAP_MEDIA_STATE),
            Frame::Media(Media::Command(_)) => CapRequirement::AllOf(CAP_MEDIA_CONTROL),
            Frame::Presence(_) => CapRequirement::AllOf(CAP_PRESENCE),
            Frame::OutboxReady(_) => CapRequirement::AllOf(CAP_OUTBOX),
            Frame::Input(Input::Session { .. }) => CapRequirement::AnyOf(CAP_INPUT_ANY),
            Frame::Input(Input::Pointer(_)) => CapRequirement::AllOf(CAP_POINTER),
            Frame::Input(Input::Key(_)) => CapRequirement::AllOf(CAP_KEY),
            Frame::Input(Input::Presenter { .. }) => CapRequirement::AllOf(CAP_PRESENTER),
            Frame::Transfer(_) => CapRequirement::AllOf(CAP_TRANSFER),
            _ => CapRequirement::None,
        }
    }
}

/// Cuts `s` to at most `max` bytes at a character boundary.
pub(crate) fn truncate_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Every JSON example from protocol.md 5.4 to 5.14, 6.2, 6.3, 6.4 and 8.2, verbatim.
    pub(crate) const SPEC_EXAMPLES: &[&str] = &[
        r#"{"t":"hello","v":1,"seq":1,"protocolVersion":1,
 "deviceId":"3f9ac21e7b04...","name":"Pixel 9","type":"phone","port":42187,
 "app":"playdrop-android/0.1.0","keepAlive":300,"trust":"paired",
 "caps":{"in":["ping","find","clipboard","transfer","outbox"],"out":["ping","clipboard","input.key","transfer"]}}"#,
        r#"{"t":"capabilities","v":1,"caps":{"in":["ping","transfer"],"out":["ping","transfer"]}}"#,
        r#"{"t":"ping","v":1,"seq":41,"message":"Hello from the desktop"}"#,
        r#"{"t":"pong","v":1,"re":41}"#,
        r#"{"t":"find","v":1,"action":"start"}"#,
        r#"{"t":"clipboard","v":1,"kind":"text","mime":"text/plain;charset=utf-8",
 "hash":"9b1d...c0","text":"https://example.org/a","sensitive":false}"#,
        r#"{"t":"clipboard","v":1,"kind":"image","mime":"image/png",
 "hash":"77e2...4a","blob":{"id":"77e2...4a","size":482113}}"#,
        r#"{"t":"battery","v":1,"level":64,"charging":true,"low":false}"#,
        r#"{"t":"media","v":1,"op":"state","player":"spotify","name":"Spotify","playing":true,
 "title":"Song","artist":"Artist","album":"Album","durationMs":215000,
 "positionMs":61200,"positionAt":1791460800123,"volume":70,
 "can":["play","pause","next","previous","seek","volume"],"art":{"id":"5c0e...19","size":40211}}"#,
        r#"{"t":"media","v":1,"op":"command","player":"spotify","action":"seek","value":90000}"#,
        r#"{"t":"presence","v":1,"state":"asleep","reason":"doze","expectBackMs":900000}"#,
        r#"{"t":"outbox_ready","v":1,"count":3,"bytes":734003200,"ids":["c1d2...f0"]}"#,
        r#"{"t":"input","v":1,"kind":"session","op":"start"}"#,
        r#"{"t":"input","v":1,"kind":"pointer","op":"move","dx":12.5,"dy":-3}"#,
        r#"{"t":"input","v":1,"kind":"pointer","op":"button","button":"left","action":"click"}"#,
        r#"{"t":"input","v":1,"kind":"key","key":"Enter","mods":["ctrl"],"action":"press"}"#,
        r#"{"t":"input","v":1,"kind":"presenter","action":"next"}"#,
        r#"{"t":"transfer","v":1,"id":"c1d2...f0","state":"accepted"}"#,
        r#"{"t":"transfer","v":1,"id":"c1d2...f0","state":"cancelled","reason":"user"}"#,
        r#"{"t":"error","v":1,"re":45,"code":"forbidden","message":"media control is off for this device"}"#,
        r#"{"t":"pair_request","v":1,"seq":2,"method":"sas","commit":"5f0c...a2"}"#,
        r#"{"t":"pair_response","v":1,"re":2,"nonce":"q8pZ2mVt4kQ1xR7bN0cWfA"}"#,
        r#"{"t":"pair_reveal","v":1,"nonce":"Hk3sL9eT0pYw2uZq6vMd1g"}"#,
        r#"{"t":"pair_confirm","v":1}"#,
        r#"{"t":"pair_reject","v":1,"reason":"user"}"#,
        r#"{"t":"pair_request","v":1,"seq":2,"method":"qr","proof":"0e4b...77"}"#,
        r#"{"t":"pair_confirm","v":1,"re":2,"proof":"d91a...3c"}"#,
        r#"{"t":"unpair","v":1}"#,
        r#"{"t":"announce_key","v":1,"kid":7,"key":"m3V9...Qw"}"#,
    ];

    #[test]
    fn every_spec_example_parses_and_round_trips() {
        for ex in SPEC_EXAMPLES {
            let frame: Frame = serde_json::from_str(ex).unwrap_or_else(|e| panic!("{ex}: {e}"));
            frame.validate().unwrap_or_else(|e| panic!("{ex}: {e}"));
            let t: serde_json::Value = serde_json::from_str(ex).unwrap();
            assert_eq!(frame.type_name(), t["t"].as_str().unwrap());
            let again: Frame =
                serde_json::from_slice(&serde_json::to_vec(&frame).unwrap()).unwrap();
            assert_eq!(again, frame, "{ex}");
        }
        // Every known type is covered by at least one example.
        for t in KNOWN_TYPES {
            assert!(
                SPEC_EXAMPLES
                    .iter()
                    .any(|e| e.contains(&format!(r#""t":"{t}""#))),
                "no example for {t}"
            );
        }
    }

    #[test]
    fn hello_fields() {
        let Frame::Hello(h) = serde_json::from_str(SPEC_EXAMPLES[0]).unwrap() else {
            panic!("not hello");
        };
        assert_eq!(h.protocol_version, 1);
        assert_eq!(h.device_type, DeviceType::Phone);
        assert_eq!(h.keep_alive, 300);
        assert_eq!(h.trust, Trust::Paired);
        assert_eq!(h.caps.incoming.len(), 5);
        assert_eq!(h.caps.outgoing[2], Capability::InputKey);
    }

    #[test]
    fn unknown_values_pass_through() {
        let f: Frame = serde_json::from_str(
            r#"{"t":"capabilities","v":1,"caps":{"in":["ping","hologram"],"out":[]},"extra":1}"#,
        )
        .unwrap();
        let Frame::Capabilities(c) = f else { panic!() };
        assert_eq!(c.caps.incoming[1], Capability::Unknown("hologram".into()));
        assert_eq!(
            serde_json::to_string(&c.caps.incoming[1]).unwrap(),
            "\"hologram\""
        );
        let f: Frame = serde_json::from_str(r#"{"t":"find","v":1,"action":"vibrate"}"#).unwrap();
        assert_eq!(
            f,
            Frame::Find(Find {
                action: FindAction::Unknown("vibrate".into())
            })
        );
    }

    #[test]
    fn capability_strings_exact() {
        let all = [
            "ping",
            "find",
            "clipboard",
            "clipboard.image",
            "battery",
            "media.state",
            "media.control",
            "presence",
            "input.pointer",
            "input.key",
            "input.presenter",
            "transfer",
            "transfer.bundle",
            "outbox",
            "blobs",
        ];
        for s in all {
            let c = Capability::from_wire(s);
            assert!(c.is_known(), "{s}");
            assert_eq!(c.as_str(), s);
        }
    }

    #[test]
    fn grant_and_unpaired_tables() {
        let parse = |s: &str| serde_json::from_str::<Frame>(s).unwrap();
        let by_t = |t: &str| {
            SPEC_EXAMPLES
                .iter()
                .find(|e| e.contains(&format!(r#""t":"{t}""#)))
                .map(|e| parse(e))
                .unwrap()
        };
        let expect = [
            ("hello", None, true),
            ("capabilities", None, false),
            ("announce_key", None, false),
            ("ping", None, true),
            ("pong", None, true),
            ("find", Some(GrantKind::Find), false),
            ("clipboard", Some(GrantKind::ClipboardIn), false),
            ("battery", None, false),
            ("media", Some(GrantKind::Media), false),
            ("presence", None, false),
            ("outbox_ready", None, false),
            ("input", Some(GrantKind::Input), false),
            ("transfer", None, false),
            ("pair_request", None, true),
            ("pair_response", None, true),
            ("pair_reveal", None, true),
            ("pair_confirm", None, true),
            ("pair_reject", None, true),
            ("unpair", None, false),
            ("error", None, true),
        ];
        assert_eq!(expect.len(), KNOWN_TYPES.len());
        for (t, grant, unpaired) in expect {
            let f = by_t(t);
            assert_eq!(f.required_grant(), grant, "{t}");
            assert_eq!(f.allowed_unpaired(), unpaired, "{t}");
        }
        assert!(!Frame::Unknown { t: "x".into() }.allowed_unpaired());
    }

    #[test]
    fn validation_rules() {
        let bad = [
            r#"{"t":"clipboard","v":1,"kind":"text","mime":"m","hash":"h"}"#,
            r#"{"t":"clipboard","v":1,"kind":"image","mime":"m","hash":"h","text":"x"}"#,
            r#"{"t":"input","v":1,"kind":"key","action":"press"}"#,
            r#"{"t":"pair_request","v":1,"method":"sas"}"#,
            r#"{"t":"pair_request","v":1,"method":"qr","commit":"aa"}"#,
        ];
        for b in bad {
            let f: Frame = serde_json::from_str(b).unwrap();
            assert!(f.validate().is_err(), "{b}");
        }
    }

    #[test]
    fn clipboard_hash_normalises_line_endings() {
        let lf = clipboard_text_hash("a\nb\nc\n");
        assert_eq!(clipboard_text_hash("a\r\nb\rc\r\n"), lf);
        assert_eq!(lf, hex_encode(blake3::hash(b"a\nb\nc\n").as_bytes()));
        assert_eq!(clipboard_text_hash("\r"), clipboard_text_hash("\n"));
    }

    #[test]
    fn ping_message_truncates_on_char_boundary() {
        let long = "é".repeat(200);
        let p = Ping::new(Some(&long));
        let m = p.message.unwrap();
        assert!(m.len() <= PING_MESSAGE_MAX);
        assert_eq!(m.len(), 256);
    }

    #[test]
    fn announce_key_round_trip() {
        let k = [9u8; 32];
        let f = AnnounceKey::new(3, &k);
        assert_eq!(f.key_bytes(), Some(k));
    }
}
