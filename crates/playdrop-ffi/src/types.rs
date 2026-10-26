//! Records and enums that mirror the core's public types with FFI-friendly fields: paths
//! and socket addresses become strings, `usize` becomes `u32` or `u64`. Each conversion
//! lives next to its type so the mapping is in one place.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;

use playdrop_core::host;
use playdrop_core::session::types as wire;

use crate::error::PlayDropError;
use crate::traits::{
    ForeignInterfaces, ForeignSecretStore, ForeignSink, ForeignSource, NetworkInterfaces,
    ReceiveSink, SecretStore, SourceOpener,
};

// ---------------------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------------------

/// `hello.type` (protocol.md 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DeviceType {
    /// A desktop computer.
    Desktop,
    /// A laptop.
    Laptop,
    /// A phone.
    Phone,
    /// A tablet.
    Tablet,
    /// A TV.
    Tv,
}

impl From<DeviceType> for wire::DeviceType {
    fn from(d: DeviceType) -> Self {
        match d {
            DeviceType::Desktop => Self::Desktop,
            DeviceType::Laptop => Self::Laptop,
            DeviceType::Phone => Self::Phone,
            DeviceType::Tablet => Self::Tablet,
            DeviceType::Tv => Self::Tv,
        }
    }
}

/// Keep-alive interval (protocol.md 3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum KeepAliveProfile {
    /// PING every 60 s; beacon always on.
    Desktop,
    /// PING every 300 s; beacon only during discovery windows (anti-pattern 10).
    Mobile,
}

impl From<KeepAliveProfile> for host::KeepAliveProfile {
    fn from(k: KeepAliveProfile) -> Self {
        match k {
            KeepAliveProfile::Desktop => Self::Desktop,
            KeepAliveProfile::Mobile => Self::Mobile,
        }
    }
}

/// Where received files go (docs/core-api.md section 2).
#[derive(Clone, uniffi::Enum)]
pub enum ReceiveTarget {
    /// Stage and finalize under one directory the host owns (desktop, iOS Documents).
    Directory {
        /// Absolute path.
        path: String,
    },
    /// Stage under `staging_dir`, then hand each verified file to `sink` (Android).
    Staged {
        /// Absolute path of a private directory on the same volume as the final store.
        staging_dir: String,
        /// The host's sink.
        sink: Arc<dyn ReceiveSink>,
    },
}

/// How to start a host. Mirrors `playdrop_core::host::HostConfig`.
#[derive(Clone, uniffi::Record)]
pub struct HostConfig {
    /// Private directory for identity, pins and the transfer index (0700, SR-28).
    pub data_dir: String,
    /// Where received files go.
    pub receive: ReceiveTarget,
    /// Name shown to peers (sanitised, SR-8).
    pub device_name: String,
    /// Form factor.
    pub device_type: DeviceType,
    /// Keep-alive profile.
    pub keep_alive: KeepAliveProfile,
    /// TCP port; 42187 unless the user overrode it. 0 is for tests only.
    pub port: u16,
    /// Address to bind; `None` is dual-stack `::`.
    pub bind_ip: Option<String>,
    /// Run mDNS and the beacon.
    pub discovery: bool,
    /// Interface list supplied by the host; `None` reads the system list (getifaddrs).
    pub interfaces: Option<Arc<dyn NetworkInterfaces>>,
    /// Seals the key and pin files at rest (SR-28); `None` keeps plain 0600 files.
    pub secret_store: Option<Arc<dyn SecretStore>>,
    /// Advertise that this host applies remote input (`input.*` in `caps.in`,
    /// protocol.md 5.12). Phones leave it off: only a desktop with an input backend
    /// accepts input. Any host may send input regardless.
    #[uniffi(default = false)]
    pub accepts_input: bool,
}

impl HostConfig {
    /// Builds the core config. Fails on a bad bind address.
    pub(crate) fn into_core(self) -> Result<host::HostConfig, PlayDropError> {
        let data_dir = PathBuf::from(self.data_dir);
        let mut cfg = match self.receive {
            ReceiveTarget::Directory { path } => {
                host::HostConfig::new(data_dir, PathBuf::from(path), self.device_name)
            }
            ReceiveTarget::Staged { staging_dir, sink } => host::HostConfig::with_sink(
                data_dir,
                PathBuf::from(staging_dir),
                Arc::new(ForeignSink(sink)),
                self.device_name,
            ),
        };
        cfg.device_type = self.device_type.into();
        cfg.keep_alive_profile = self.keep_alive.into();
        cfg.port = self.port;
        cfg.discovery = self.discovery;
        if let Some(ip) = self.bind_ip {
            cfg.bind_ip = ip
                .parse::<IpAddr>()
                .map_err(|e| PlayDropError::other(format!("bind address {ip}: {e}")))?;
        }
        if let Some(src) = self.interfaces {
            cfg.interfaces = Box::new(ForeignInterfaces(src));
        }
        if let Some(store) = self.secret_store {
            cfg.secret_store = Some(Arc::new(ForeignSecretStore(store)));
        }
        cfg.accepts_input = self.accepts_input;
        Ok(cfg)
    }
}

/// One IPv4 interface as the host reports it (docs/core-api.md section 7).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Ipv4Interface {
    /// OS interface index.
    pub index: u32,
    /// Dotted address.
    pub addr: String,
    /// Dotted netmask.
    pub netmask: String,
}

// ---------------------------------------------------------------------------------------
// Peers and pairing
// ---------------------------------------------------------------------------------------

/// Per-device grants (protocol.md 6.4, SR-14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "mirrors the core grants record"
)]
pub struct Grants {
    /// Auto-accept files.
    pub files: bool,
    /// Apply clipboard pushes from the peer.
    pub clipboard_in: bool,
    /// Push our clipboard to the peer.
    pub clipboard_out: bool,
    /// Remote input (Phase 5).
    pub input: bool,
    /// Media control.
    pub media: bool,
    /// Ring on `find`.
    pub find: bool,
}

impl From<playdrop_core::pin_store::Grants> for Grants {
    fn from(g: playdrop_core::pin_store::Grants) -> Self {
        Self {
            files: g.files,
            clipboard_in: g.clipboard_in,
            clipboard_out: g.clipboard_out,
            input: g.input,
            media: g.media,
            find: g.find,
        }
    }
}

impl From<Grants> for playdrop_core::pin_store::Grants {
    fn from(g: Grants) -> Self {
        Self {
            files: g.files,
            clipboard_in: g.clipboard_in,
            clipboard_out: g.clipboard_out,
            input: g.input,
            media: g.media,
            find: g.find,
        }
    }
}

/// A paired peer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PeerSummary {
    /// Hex Device ID.
    pub device_id: String,
    /// Short ID (SR-8).
    pub short_id: String,
    /// Local label.
    pub label: String,
    /// A session is up.
    pub online: bool,
    /// Grants.
    pub grants: Grants,
    /// Last-known addresses, newest first, as `ip:port`.
    pub last_known: Vec<String>,
    /// Pairing time, Unix seconds.
    pub paired_at: u64,
    /// Last frame from this peer, Unix seconds.
    pub last_seen: Option<u64>,
    /// The peer told us it no longer has us pinned (protocol.md 5.4).
    pub removed_by_peer: bool,
    /// What the peer accepts (its `hello.caps.in`, protocol.md section 4) while online,
    /// empty offline: `input.pointer`, `input.key`, `input.presenter`, `media.control`,
    /// `clipboard.image` and so on. Show a feature only when its capability is listed.
    pub capabilities: Vec<String>,
}

impl From<host::PeerSummary> for PeerSummary {
    fn from(p: host::PeerSummary) -> Self {
        Self {
            device_id: p.device_id,
            short_id: p.short_id,
            label: p.label,
            online: p.online,
            grants: p.grants.into(),
            last_known: p.last_known.iter().map(ToString::to_string).collect(),
            paired_at: p.paired_at,
            last_seen: p.last_seen,
            removed_by_peer: p.removed_by_peer,
            capabilities: p.capabilities,
        }
    }
}

/// A device in its own "Add device" window (protocol.md 8.3).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AddDeviceCandidate {
    /// Sanitised advertised name.
    pub name: String,
    /// Device type token.
    pub device_type: String,
    /// `ip:port` to pair with.
    pub addr: String,
}

impl From<host::AddDeviceCandidate> for AddDeviceCandidate {
    fn from(c: host::AddDeviceCandidate) -> Self {
        Self {
            name: c.name,
            device_type: c.device_type,
            addr: c.addr.to_string(),
        }
    }
}

/// What a pairing prompt asks (protocol.md 6.2, 6.3).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum PairingPrompt {
    /// Compare the six digits with the other screen (never log them).
    Sas {
        /// The SAS.
        sas: String,
    },
    /// QR displayer: "Add <name> (<Short ID>)?".
    ScannerConfirm,
}

impl From<host::PairingPromptKind> for PairingPrompt {
    fn from(k: host::PairingPromptKind) -> Self {
        match k {
            host::PairingPromptKind::Sas { sas } => Self::Sas { sas: sas.0 },
            host::PairingPromptKind::ScannerConfirm => Self::ScannerConfirm,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Transfers
// ---------------------------------------------------------------------------------------

/// Which side of a transfer this host is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TransferDirection {
    /// We send.
    Send,
    /// We receive.
    Receive,
}

impl From<host::TransferDirection> for TransferDirection {
    fn from(d: host::TransferDirection) -> Self {
        match d {
            host::TransferDirection::Send => Self::Send,
            host::TransferDirection::Receive => Self::Receive,
        }
    }
}

/// Transfer state as the UI shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum TransferStatus {
    /// Offered, waiting for the receiver.
    Offered,
    /// Bytes moving.
    Active,
    /// Every file verified or skipped.
    Complete,
    /// Failed.
    Failed,
    /// Cancelled or declined.
    Cancelled,
}

impl From<host::TransferStatus> for TransferStatus {
    fn from(s: host::TransferStatus) -> Self {
        match s {
            host::TransferStatus::Offered => Self::Offered,
            host::TransferStatus::Active => Self::Active,
            host::TransferStatus::Complete => Self::Complete,
            host::TransferStatus::Failed => Self::Failed,
            host::TransferStatus::Cancelled => Self::Cancelled,
        }
    }
}

/// A delivered file: manifest path and where it ended up (a `content://` URI on Android).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DeliveredFile {
    /// Manifest path, components joined with `/`.
    pub path: String,
    /// Final location string.
    pub location: String,
}

impl From<host::DeliveredFile> for DeliveredFile {
    fn from(d: host::DeliveredFile) -> Self {
        Self {
            path: d.path,
            location: d.location,
        }
    }
}

/// A live or recently finished transfer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct TransferSnapshot {
    /// Transfer ID.
    pub id: String,
    /// Hex Device ID of the peer.
    pub peer: String,
    /// Short ID of the peer.
    pub short_id: String,
    /// Our side.
    pub direction: TransferDirection,
    /// State.
    pub state: TransferStatus,
    /// Bytes moved or verified.
    pub bytes: u64,
    /// Total bytes.
    pub total: u64,
    /// Recent rate.
    pub bytes_per_sec: u64,
    /// Files finished so far (sent whole, or verified; bundled files when placed).
    pub files_done: u64,
    /// Start time, Unix seconds.
    pub started_at: u64,
    /// Files in the manifest.
    pub file_count: u64,
    /// Note or failure reason.
    pub note: Option<String>,
    /// Delivered files (receive side, once complete).
    pub delivered: Vec<DeliveredFile>,
    /// A failed send whose record is kept: it resumes by itself or when the same files
    /// are sent again, and `Host::cancel_transfer` forgets it. False otherwise.
    pub resumable: bool,
}

impl From<host::TransferSnapshot> for TransferSnapshot {
    fn from(t: host::TransferSnapshot) -> Self {
        Self {
            id: t.id,
            peer: t.peer,
            short_id: t.short_id,
            direction: t.direction.into(),
            state: t.state.into(),
            bytes: t.bytes,
            total: t.total,
            bytes_per_sec: t.bytes_per_sec,
            files_done: t.files_done as u64,
            started_at: t.started_at,
            file_count: t.file_count as u64,
            note: t.note,
            delivered: t.delivered.into_iter().map(Into::into).collect(),
            resumable: t.resumable,
        }
    }
}

/// One file of an offer (for the accept prompt).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OfferedFile {
    /// Manifest path for display only.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
}

impl From<host::OfferedFile> for OfferedFile {
    fn from(f: host::OfferedFile) -> Self {
        Self {
            path: f.path,
            size: f.size,
        }
    }
}

/// One file's verified root.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FileRoot {
    /// Manifest path.
    pub path: String,
    /// Size.
    pub size: u64,
    /// Hex root, or `None` for bundled and empty files.
    pub root: Option<String>,
}

impl From<host::FileRoot> for FileRoot {
    fn from(f: host::FileRoot) -> Self {
        Self {
            path: f.path,
            size: f.size,
            root: f.root,
        }
    }
}

/// Something to send (docs/core-api.md section 3).
#[derive(Clone, uniffi::Enum)]
pub enum SendSource {
    /// A file or folder by absolute path (desktop hosts, or an app-private file).
    Path {
        /// Absolute path.
        path: String,
    },
    /// One file behind a reopenable handle (a `content://` item on Android).
    Stream {
        /// File name; one path component (SR-17).
        name: String,
        /// Size in bytes.
        size: u64,
        /// Modification time, Unix ms; 0 if unknown.
        mtime_ms: i64,
        /// MIME type hint.
        mime: Option<String>,
        /// Opens the item read-only; called more than once.
        opener: Arc<dyn SourceOpener>,
    },
}

impl From<SendSource> for host::SendSource {
    fn from(s: SendSource) -> Self {
        match s {
            SendSource::Path { path } => Self::Path(PathBuf::from(path)),
            SendSource::Stream {
                name,
                size,
                mtime_ms,
                mime,
                opener,
            } => Self::Stream(host::StreamSource {
                name,
                size,
                mtime_ms,
                mime,
                opener: Arc::new(ForeignSource(opener)),
            }),
        }
    }
}

/// What a clipboard push did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ClipboardPush {
    /// Hex Device IDs the clip went to.
    pub sent_to: Vec<String>,
    /// Skipped by the echo rule.
    pub skipped_echo: bool,
    /// Peers with the grant but offline.
    pub skipped_offline: u32,
    /// Peers without the `clipboardOut` grant.
    pub skipped_no_grant: u32,
    /// Peers whose build lacks `blobs` or `clipboard.image`.
    pub skipped_unsupported: u32,
}

impl From<host::ClipboardPush> for ClipboardPush {
    fn from(p: host::ClipboardPush) -> Self {
        Self {
            sent_to: p
                .sent_to
                .iter()
                .map(playdrop_core::identity::DeviceId::to_hex)
                .collect(),
            skipped_echo: p.skipped_echo,
            skipped_offline: u32::try_from(p.skipped_offline).unwrap_or(u32::MAX),
            skipped_no_grant: u32::try_from(p.skipped_no_grant).unwrap_or(u32::MAX),
            skipped_unsupported: u32::try_from(p.skipped_unsupported).unwrap_or(u32::MAX),
        }
    }
}

/// An outbox item this host holds for a peer (protocol.md 7.8).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutboxItem {
    /// Item ID.
    pub id: String,
    /// Hex Device ID of the peer it is queued for.
    pub peer: String,
    /// Files.
    pub file_count: u64,
    /// Total bytes.
    pub total_bytes: u64,
    /// Note.
    pub note: Option<String>,
    /// Creation time, Unix ms.
    pub created_at_ms: u64,
}

impl From<host::OutboxItem> for OutboxItem {
    fn from(o: host::OutboxItem) -> Self {
        Self {
            id: o.id,
            peer: o.peer,
            file_count: o.file_count as u64,
            total_bytes: o.total_bytes,
            note: o.note,
            created_at_ms: o.created_at_ms,
        }
    }
}

// ---------------------------------------------------------------------------------------
// Session plane values
// ---------------------------------------------------------------------------------------

/// `presence.state` (protocol.md 5.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PresenceState {
    /// In use.
    Active,
    /// Idle.
    Idle,
    /// Screen locked.
    Locked,
    /// About to sleep; the peer expects a drop.
    Asleep,
}

impl From<PresenceState> for wire::PresenceState {
    fn from(p: PresenceState) -> Self {
        match p {
            PresenceState::Active => Self::Active,
            PresenceState::Idle => Self::Idle,
            PresenceState::Locked => Self::Locked,
            PresenceState::Asleep => Self::Asleep,
        }
    }
}

/// Media actions (protocol.md 5.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MediaAction {
    /// Play.
    Play,
    /// Pause.
    Pause,
    /// Toggle.
    PlayPause,
    /// Stop.
    Stop,
    /// Next track.
    Next,
    /// Previous track.
    Previous,
    /// Seek to `value` ms.
    Seek,
    /// Set volume to `value` (0 to 100).
    Volume,
    /// An action this build does not know (the wire keeps unknown values, protocol.md 4).
    Other,
}

impl From<MediaAction> for wire::MediaAction {
    fn from(a: MediaAction) -> Self {
        match a {
            MediaAction::Play => Self::Play,
            MediaAction::Pause => Self::Pause,
            MediaAction::PlayPause => Self::PlayPause,
            MediaAction::Stop => Self::Stop,
            MediaAction::Next => Self::Next,
            MediaAction::Previous => Self::Previous,
            MediaAction::Seek => Self::Seek,
            MediaAction::Volume => Self::Volume,
            MediaAction::Other => Self::Unknown("other".to_owned()),
        }
    }
}

impl From<wire::MediaAction> for MediaAction {
    fn from(a: wire::MediaAction) -> Self {
        match a {
            wire::MediaAction::Play => Self::Play,
            wire::MediaAction::Pause => Self::Pause,
            wire::MediaAction::PlayPause => Self::PlayPause,
            wire::MediaAction::Stop => Self::Stop,
            wire::MediaAction::Next => Self::Next,
            wire::MediaAction::Previous => Self::Previous,
            wire::MediaAction::Seek => Self::Seek,
            wire::MediaAction::Volume => Self::Volume,
            wire::MediaAction::Unknown(_) => Self::Other,
        }
    }
}

/// A media player's state (protocol.md 5.9).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MediaState {
    /// Player ID.
    pub player: String,
    /// Display name.
    pub name: Option<String>,
    /// Playing now.
    pub playing: bool,
    /// Title.
    pub title: Option<String>,
    /// Artist.
    pub artist: Option<String>,
    /// Album.
    pub album: Option<String>,
    /// Duration in ms.
    pub duration_ms: Option<u64>,
    /// Position in ms at `position_at_ms`.
    pub position_ms: Option<u64>,
    /// Unix ms when `position_ms` was read.
    pub position_at_ms: Option<u64>,
    /// Volume 0 to 100.
    pub volume: Option<u8>,
    /// Supported actions.
    pub can: Vec<MediaAction>,
}

impl From<MediaState> for host::MediaState {
    fn from(m: MediaState) -> Self {
        Self {
            player: m.player,
            name: m.name,
            playing: m.playing,
            title: m.title,
            artist: m.artist,
            album: m.album,
            duration_ms: m.duration_ms,
            position_ms: m.position_ms,
            position_at_ms: m.position_at_ms,
            volume: m.volume,
            can: m.can.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<host::MediaState> for MediaState {
    fn from(m: host::MediaState) -> Self {
        Self {
            player: m.player,
            name: m.name,
            playing: m.playing,
            title: m.title,
            artist: m.artist,
            album: m.album,
            duration_ms: m.duration_ms,
            position_ms: m.position_ms,
            position_at_ms: m.position_at_ms,
            volume: m.volume,
            can: m.can.into_iter().map(Into::into).collect(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------------------

/// A discovery path (PLAN.md: name the layer that failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DiscoveryPath {
    /// mDNS.
    Mdns,
    /// Multicast beacon.
    Beacon,
    /// Last-known dial.
    LastKnown,
}

impl From<host::DiscoveryPath> for DiscoveryPath {
    fn from(p: host::DiscoveryPath) -> Self {
        match p {
            host::DiscoveryPath::Mdns => Self::Mdns,
            host::DiscoveryPath::Beacon => Self::Beacon,
            host::DiscoveryPath::LastKnown => Self::LastKnown,
        }
    }
}

/// Multicast carrier status.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MulticastStatus {
    /// Beacon enabled.
    pub enabled: bool,
    /// Interfaces whose group join succeeded.
    pub joined: Vec<String>,
    /// Interfaces whose join failed, as `addr: error`.
    pub join_failed: Vec<String>,
    /// A beacon was heard in the last 60 s.
    pub beacon_recently: bool,
    /// Seconds since the last beacon.
    pub last_beacon_secs_ago: Option<u64>,
}

/// mDNS carrier status.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct MdnsStatus {
    /// mDNS enabled.
    pub enabled: bool,
    /// Our record is registered.
    pub registered: bool,
    /// Last error.
    pub error: Option<String>,
    /// Any peer record seen.
    pub peer_seen: bool,
    /// Seconds since the last peer record.
    pub last_seen_secs_ago: Option<u64>,
}

/// The most recent failed dial to a peer.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DialFailure {
    /// Path that produced the address.
    pub path: DiscoveryPath,
    /// What went wrong.
    pub failure: String,
    /// Unix seconds.
    pub at: u64,
}

/// Per-peer reachability.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct PeerDiagnostic {
    /// Hex Device ID.
    pub peer: String,
    /// Short ID.
    pub short_id: String,
    /// Label.
    pub label: String,
    /// Online.
    pub online: bool,
    /// Last-known addresses.
    pub last_known: Vec<String>,
    /// Seen over mDNS.
    pub mdns_seen: bool,
    /// Seen over the beacon.
    pub beacon_seen: bool,
    /// Most recent failed dial.
    pub last_result: Option<DialFailure>,
}

/// The on-demand diagnostic snapshot.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Diagnostics {
    /// Bound address.
    pub addr: String,
    /// Our hex Device ID.
    pub device_id: String,
    /// Our Short ID.
    pub short_id: String,
    /// Port.
    pub port: u16,
    /// Listener is up.
    pub bind_ok: bool,
    /// Interfaces.
    pub interfaces: Vec<Ipv4Interface>,
    /// Beacon status.
    pub multicast: MulticastStatus,
    /// mDNS status.
    pub mdns: MdnsStatus,
    /// Paired peers.
    pub peers: Vec<PeerDiagnostic>,
}

impl From<host::Diagnostics> for Diagnostics {
    fn from(d: host::Diagnostics) -> Self {
        Self {
            addr: d.addr.to_string(),
            device_id: d.device_id,
            short_id: d.short_id,
            port: d.port,
            bind_ok: d.bind_ok,
            interfaces: d
                .interfaces
                .into_iter()
                .map(|i| Ipv4Interface {
                    index: i.index,
                    addr: i.addr.to_string(),
                    netmask: i.netmask.to_string(),
                })
                .collect(),
            multicast: MulticastStatus {
                enabled: d.multicast.enabled,
                joined: d.multicast.joined.iter().map(ToString::to_string).collect(),
                join_failed: d
                    .multicast
                    .join_failed
                    .iter()
                    .map(|j| format!("{}: {}", j.addr, j.error))
                    .collect(),
                beacon_recently: d.multicast.beacon_recently,
                last_beacon_secs_ago: d.multicast.last_beacon_secs_ago,
            },
            mdns: MdnsStatus {
                enabled: d.mdns.enabled,
                registered: d.mdns.registered,
                error: d.mdns.error,
                peer_seen: d.mdns.peer_seen,
                last_seen_secs_ago: d.mdns.last_seen_secs_ago,
            },
            peers: d
                .peers
                .into_iter()
                .map(|p| PeerDiagnostic {
                    peer: p.peer,
                    short_id: p.short_id,
                    label: p.label,
                    online: p.online,
                    last_known: p.last_known.iter().map(ToString::to_string).collect(),
                    mdns_seen: p.mdns_seen,
                    beacon_seen: p.beacon_seen,
                    last_result: p.last_result.map(|r| DialFailure {
                        path: r.path.into(),
                        failure: r.failure,
                        at: r.at,
                    }),
                })
                .collect(),
        }
    }
}

/// Parses `ip:port` for calls that take an address.
pub(crate) fn parse_addr(addr: &str) -> Result<SocketAddr, PlayDropError> {
    addr.parse()
        .map_err(|e| PlayDropError::other(format!("address {addr}: {e}")))
}

/// Parses a hex Device ID.
pub(crate) fn parse_peer(peer: &str) -> Result<playdrop_core::identity::DeviceId, PlayDropError> {
    peer.parse().map_err(|_| PlayDropError::NotFound {
        detail: format!("not a device id: {peer}"),
    })
}

// ---------------------------------------------------------------------------------------
// Remote input (protocol.md 5.12, Phase 5)
// ---------------------------------------------------------------------------------------

/// Pointer buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PointerButton {
    /// Left.
    Left,
    /// Right.
    Right,
    /// Middle.
    Middle,
}

impl From<PointerButton> for wire::PointerButton {
    fn from(b: PointerButton) -> Self {
        match b {
            PointerButton::Left => Self::Left,
            PointerButton::Right => Self::Right,
            PointerButton::Middle => Self::Middle,
        }
    }
}

impl From<wire::PointerButton> for PointerButton {
    fn from(b: wire::PointerButton) -> Self {
        match b {
            wire::PointerButton::Right => Self::Right,
            wire::PointerButton::Middle => Self::Middle,
            wire::PointerButton::Left | wire::PointerButton::Unknown(_) => Self::Left,
        }
    }
}

/// What happened to a pointer button.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ButtonAction {
    /// Pressed and held.
    Down,
    /// Released.
    Up,
    /// Down then up.
    Click,
    /// Two clicks.
    Double,
}

impl From<ButtonAction> for wire::ButtonAction {
    fn from(a: ButtonAction) -> Self {
        match a {
            ButtonAction::Down => Self::Down,
            ButtonAction::Up => Self::Up,
            ButtonAction::Click => Self::Click,
            ButtonAction::Double => Self::Double,
        }
    }
}

impl From<wire::ButtonAction> for ButtonAction {
    fn from(a: wire::ButtonAction) -> Self {
        match a {
            wire::ButtonAction::Down => Self::Down,
            wire::ButtonAction::Up => Self::Up,
            wire::ButtonAction::Double => Self::Double,
            wire::ButtonAction::Click | wire::ButtonAction::Unknown(_) => Self::Click,
        }
    }
}

/// What happened to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum KeyAction {
    /// Down then up.
    Press,
    /// Pressed and held.
    Down,
    /// Released.
    Up,
}

impl From<KeyAction> for wire::KeyAction {
    fn from(a: KeyAction) -> Self {
        match a {
            KeyAction::Press => Self::Press,
            KeyAction::Down => Self::Down,
            KeyAction::Up => Self::Up,
        }
    }
}

impl From<wire::KeyAction> for KeyAction {
    fn from(a: wire::KeyAction) -> Self {
        match a {
            wire::KeyAction::Down => Self::Down,
            wire::KeyAction::Up => Self::Up,
            wire::KeyAction::Press | wire::KeyAction::Unknown(_) => Self::Press,
        }
    }
}

/// A held modifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum Modifier {
    /// Control.
    Ctrl,
    /// Alt (Option).
    Alt,
    /// Shift.
    Shift,
    /// Meta (Super, Windows, Command).
    Meta,
}

impl From<Modifier> for wire::Modifier {
    fn from(m: Modifier) -> Self {
        match m {
            Modifier::Ctrl => Self::Ctrl,
            Modifier::Alt => Self::Alt,
            Modifier::Shift => Self::Shift,
            Modifier::Meta => Self::Meta,
        }
    }
}

impl TryFrom<wire::Modifier> for Modifier {
    type Error = ();

    fn try_from(m: wire::Modifier) -> Result<Self, ()> {
        match m {
            wire::Modifier::Ctrl => Ok(Self::Ctrl),
            wire::Modifier::Alt => Ok(Self::Alt),
            wire::Modifier::Shift => Ok(Self::Shift),
            wire::Modifier::Meta => Ok(Self::Meta),
            wire::Modifier::Unknown(_) => Err(()),
        }
    }
}

/// Presenter remote actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum PresenterAction {
    /// Next slide.
    Next,
    /// Previous slide.
    Previous,
    /// Show the laser pointer.
    PointerOn,
    /// Hide the laser pointer.
    PointerOff,
    /// Black the screen.
    Blackout,
}

impl From<PresenterAction> for wire::PresenterAction {
    fn from(a: PresenterAction) -> Self {
        match a {
            PresenterAction::Next => Self::Next,
            PresenterAction::Previous => Self::Previous,
            PresenterAction::PointerOn => Self::PointerOn,
            PresenterAction::PointerOff => Self::PointerOff,
            PresenterAction::Blackout => Self::Blackout,
        }
    }
}

impl TryFrom<wire::PresenterAction> for PresenterAction {
    type Error = ();

    fn try_from(a: wire::PresenterAction) -> Result<Self, ()> {
        match a {
            wire::PresenterAction::Next => Ok(Self::Next),
            wire::PresenterAction::Previous => Ok(Self::Previous),
            wire::PresenterAction::PointerOn => Ok(Self::PointerOn),
            wire::PresenterAction::PointerOff => Ok(Self::PointerOff),
            wire::PresenterAction::Blackout => Ok(Self::Blackout),
            wire::PresenterAction::Unknown(_) => Err(()),
        }
    }
}

/// One remote input frame (protocol.md 5.12). A controller opens with `Session { start:
/// true }`, sends pointer, key and presenter frames, and closes with `Session { start:
/// false }`; the target applies nothing outside a session.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum InputEvent {
    /// Open (`true`) or close (`false`) the input session.
    Session {
        /// Start or stop.
        start: bool,
    },
    /// Relative pointer move, in pointer units.
    PointerMove {
        /// Horizontal delta.
        dx: f64,
        /// Vertical delta.
        dy: f64,
    },
    /// Pointer button.
    PointerButton {
        /// Which.
        button: PointerButton,
        /// What happened.
        action: ButtonAction,
    },
    /// Scroll; `pixels` false means lines.
    Scroll {
        /// Horizontal amount.
        dx: f64,
        /// Vertical amount.
        dy: f64,
        /// Pixel units rather than lines.
        pixels: bool,
    },
    /// A named key (W3C UI Events `key` value, such as `Enter` or `ArrowLeft`) with
    /// modifiers.
    Key {
        /// The key name.
        key: String,
        /// Held modifiers.
        mods: Vec<Modifier>,
        /// What happened.
        action: KeyAction,
    },
    /// A literal string to type.
    Text {
        /// The text.
        text: String,
    },
    /// Presenter remote.
    Presenter {
        /// What to do.
        action: PresenterAction,
    },
}

impl From<InputEvent> for wire::Input {
    fn from(i: InputEvent) -> Self {
        match i {
            InputEvent::Session { start } => Self::Session {
                op: if start {
                    wire::SessionOp::Start
                } else {
                    wire::SessionOp::Stop
                },
            },
            InputEvent::PointerMove { dx, dy } => {
                Self::Pointer(wire::PointerInput::Move { dx, dy })
            }
            InputEvent::PointerButton { button, action } => {
                Self::Pointer(wire::PointerInput::Button {
                    button: button.into(),
                    action: action.into(),
                })
            }
            InputEvent::Scroll { dx, dy, pixels } => Self::Pointer(wire::PointerInput::Scroll {
                dx,
                dy,
                unit: pixels.then_some(wire::ScrollUnit::Pixel),
            }),
            InputEvent::Key { key, mods, action } => Self::Key(wire::KeyInput {
                key: Some(key),
                text: None,
                mods: mods.into_iter().map(Into::into).collect(),
                action: action.into(),
            }),
            InputEvent::Text { text } => Self::Key(wire::KeyInput {
                key: None,
                text: Some(text),
                mods: Vec::new(),
                action: wire::KeyAction::Press,
            }),
            InputEvent::Presenter { action } => Self::Presenter {
                action: action.into(),
            },
        }
    }
}

impl InputEvent {
    /// Maps a frame from the wire; `None` for a value this binding does not know (an
    /// unknown presenter action, say), which the host ignores (protocol.md section 4).
    pub(crate) fn from_wire(i: wire::Input) -> Option<Self> {
        Some(match i {
            wire::Input::Session { op } => Self::Session {
                start: match op {
                    wire::SessionOp::Start => true,
                    wire::SessionOp::Stop => false,
                    wire::SessionOp::Unknown(_) => return None,
                },
            },
            wire::Input::Pointer(wire::PointerInput::Move { dx, dy }) => {
                Self::PointerMove { dx, dy }
            }
            wire::Input::Pointer(wire::PointerInput::Button { button, action }) => {
                if !button.is_known() || !action.is_known() {
                    return None;
                }
                Self::PointerButton {
                    button: button.into(),
                    action: action.into(),
                }
            }
            wire::Input::Pointer(wire::PointerInput::Scroll { dx, dy, unit }) => Self::Scroll {
                dx,
                dy,
                pixels: unit == Some(wire::ScrollUnit::Pixel),
            },
            wire::Input::Key(k) => match (k.key, k.text) {
                (Some(key), None) => {
                    if !k.action.is_known() {
                        return None;
                    }
                    let mods = k
                        .mods
                        .into_iter()
                        .map(Modifier::try_from)
                        .collect::<Result<Vec<_>, ()>>()
                        .ok()?;
                    Self::Key {
                        key,
                        mods,
                        action: k.action.into(),
                    }
                }
                (None, Some(text)) => Self::Text { text },
                _ => return None,
            },
            wire::Input::Presenter { action } => Self::Presenter {
                action: action.try_into().ok()?,
            },
        })
    }
}

#[cfg(test)]
mod input_tests {
    use super::*;

    /// Every binding-side frame survives the trip to the wire type and back.
    #[test]
    fn input_round_trips() {
        let events = vec![
            InputEvent::Session { start: true },
            InputEvent::PointerMove { dx: 1.5, dy: -2.0 },
            InputEvent::PointerButton {
                button: PointerButton::Right,
                action: ButtonAction::Double,
            },
            InputEvent::Scroll {
                dx: 0.0,
                dy: 3.0,
                pixels: true,
            },
            InputEvent::Key {
                key: "Enter".into(),
                mods: vec![Modifier::Ctrl, Modifier::Shift],
                action: KeyAction::Down,
            },
            InputEvent::Text {
                text: "hello".into(),
            },
            InputEvent::Presenter {
                action: PresenterAction::Blackout,
            },
        ];
        for ev in events {
            let wire: wire::Input = ev.clone().into();
            assert_eq!(InputEvent::from_wire(wire), Some(ev));
        }
        let unknown = wire::Input::Presenter {
            action: wire::PresenterAction::Unknown("spin".into()),
        };
        assert_eq!(InputEvent::from_wire(unknown), None);
    }
}
