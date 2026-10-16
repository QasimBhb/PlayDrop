//! The window's view state and its conversion into Slint rows.
//!
//! [`Store`] holds what the daemon told us (status, peers, transfers, add-device
//! candidates, the open pairing prompt) and applies [`Event`]s to it, reporting which parts
//! changed so the controller refreshes only those models. Everything here is plain data
//! and runs without a display, so it is unit tested.

use std::fmt::Write as _;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use playdrop_core::host::{
    AddDeviceCandidate, Diagnostics, DiscoveryPath, HostEvent, MdnsStatus, MediaState,
    MulticastStatus, PairingPromptKind, PeerDiagnostic, PeerSummary, TransferDirection,
    TransferSnapshot, TransferStatus,
};
use playdrop_core::pin_store::Grants;
use playdrop_core::session::types::MediaAction;
use playdrop_ipc::{
    ClipSkip, DaemonEvent, DiagnosticsReport, Event, FirewallHint, PortProbe, Settings, Status,
};
use slint::{ModelRc, SharedString, VecModel};

use crate::{CandidateRow, DeviceRow, DiagSection, HelpFocus, MediaRow, TransferRow};

/// The daemon's default port, used when a typed address has none (protocol.md section 1).
pub const DEFAULT_PORT: u16 = 42187;

/// How many offered file names a transfer row lists before "and N more".
const OFFER_NAMES_SHOWN: usize = 3;

/// Current Unix time in seconds.
pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// A paired device as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerView {
    /// Hex Device ID.
    pub device_id: String,
    /// Short ID (SR-8).
    pub short_id: String,
    /// Local label.
    pub label: String,
    /// Session up.
    pub online: bool,
    /// Last `presence` state reported while online. Kept as `asleep` after the device
    /// drops, so a phone in Doze reads "Asleep" rather than "Offline" (protocol.md 5.10:
    /// after `asleep` the peer expects a drop).
    pub presence: Option<String>,
    /// Last battery report: level, charging.
    pub battery: Option<(u8, bool)>,
    /// When pairing completed, Unix seconds.
    pub paired_at: u64,
    /// When the device last sent anything, Unix seconds: the daemon's persisted value
    /// (SR-8), moved forward by online and offline events seen here. `None` = never.
    pub last_seen: Option<u64>,
    /// Grants.
    pub grants: Grants,
    /// Last discovery diagnostic, already worded.
    pub note: Option<String>,
    /// What the peer accepts (`caps.in`) while online; empty offline.
    pub capabilities: Vec<String>,
    /// The peer's media players (protocol.md 5.9), most recently updated first. Only
    /// arrives with the `media` grant; dropped when the peer goes offline.
    pub media: Vec<MediaState>,
}

impl PeerView {
    fn from_summary(p: PeerSummary, now: u64) -> Self {
        Self {
            last_seen: p.last_seen.or(p.online.then_some(now)),
            device_id: p.device_id,
            short_id: p.short_id,
            label: p.label,
            online: p.online,
            presence: None,
            battery: None,
            paired_at: p.paired_at,
            grants: p.grants,
            note: None,
            capabilities: p.capabilities,
            media: Vec::new(),
        }
    }
}

/// A transfer as the window shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferView {
    /// Transfer ID.
    pub id: String,
    /// Other device's hex ID.
    pub peer: String,
    /// Other device's Short ID.
    pub short_id: String,
    /// Our side.
    pub direction: TransferDirection,
    /// Where it stands.
    pub state: TransferStatus,
    /// Bytes moved or verified.
    pub bytes: u64,
    /// Total bytes.
    pub total: u64,
    /// Recent rate.
    pub bytes_per_sec: u64,
    /// Files in the manifest.
    pub file_count: usize,
    /// Files finished so far; 0 from an older daemon.
    pub files_done: usize,
    /// First offered names, for the offer prompt.
    pub names: Vec<String>,
    /// Sender's note, or the failure reason once failed.
    pub note: Option<String>,
    /// Receive side: final paths, from `TransferComplete` or, for a window opened after
    /// the transfer finished, from the daemon's snapshot.
    pub saved: Vec<PathBuf>,
    /// Wall time once complete.
    pub elapsed_ms: Option<u64>,
}

impl TransferView {
    fn from_snapshot(s: TransferSnapshot) -> Self {
        Self {
            id: s.id,
            peer: s.peer,
            short_id: s.short_id,
            direction: s.direction,
            state: s.state,
            bytes: s.bytes,
            total: s.total,
            bytes_per_sec: s.bytes_per_sec,
            file_count: s.file_count,
            files_done: s.files_done,
            names: Vec::new(),
            note: s.note,
            saved: s.saved,
            elapsed_ms: None,
        }
    }

    fn blank(id: &str, peer: &str, direction: TransferDirection) -> Self {
        Self {
            id: id.to_owned(),
            peer: peer.to_owned(),
            short_id: short_from_hex(peer),
            direction,
            state: TransferStatus::Active,
            bytes: 0,
            total: 0,
            bytes_per_sec: 0,
            file_count: 0,
            files_done: 0,
            names: Vec::new(),
            note: None,
            saved: Vec::new(),
            elapsed_ms: None,
        }
    }
}

/// The open pairing prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    /// The SAS, or `None` for a scanner confirmation.
    pub sas: Option<String>,
    /// Sanitised peer name.
    pub name: String,
    /// Short ID.
    pub short_id: String,
}

/// Something to re-request from the daemon after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refetch {
    /// Peers only (pairing changed).
    Peers,
    /// Status, peers and transfers (host restarted).
    All,
}

/// What an applied event changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// The peer list changed shape.
    Peers,
    /// One peer row changed.
    Peer(usize),
    /// The transfer list changed shape.
    Transfers,
    /// One transfer row changed.
    Transfer(usize),
    /// Add-device candidates changed.
    Candidates,
    /// The pairing prompt opened or closed.
    Prompt,
    /// The add-device window was closed by the daemon.
    WindowClosed,
    /// A pairing outcome to show on the Add device screen; `true` for an error.
    PairingStatus(String, bool),
    /// A one-line notice; `true` for an error.
    Notice(String, bool),
    /// Settings changed.
    Settings,
    /// Lock state changed (SR-6).
    Lock,
    /// The set of remote input sessions changed (SR-25 banner).
    InputSessions,
    /// Re-request something from the daemon.
    Refetch(Refetch),
    /// The daemon is going away.
    ShuttingDown,
}

/// Everything the window knows about the daemon.
#[derive(Debug, Default)]
pub struct Store {
    /// Last status answer.
    pub status: Option<Status>,
    /// Paired devices, in daemon order.
    pub peers: Vec<PeerView>,
    /// Transfers, oldest first.
    pub transfers: Vec<TransferView>,
    /// Devices in their own add-device window.
    pub candidates: Vec<AddDeviceCandidate>,
    /// The open pairing prompt.
    pub prompt: Option<PromptView>,
    /// Peers with a remote input session open on this desktop (SR-25), as hex Device ID
    /// and label, in the order they started.
    pub input_sessions: Vec<(String, String)>,
}

impl Store {
    /// Replaces the status, and seeds the remote input sessions from its `input_from`
    /// labels (the daemon's current truth), so a window opened mid-session shows its
    /// banner at once. A label is matched to a listed peer when it can be; otherwise the
    /// entry waits for that peer's next `input_session` event to learn its ID.
    pub fn set_status(&mut self, status: Status) {
        self.input_sessions = status
            .input_from
            .iter()
            .map(|label| {
                let id = self
                    .peers
                    .iter()
                    .find(|p| p.label == *label)
                    .map(|p| p.device_id.clone())
                    .unwrap_or_default();
                (id, label.clone())
            })
            .collect();
        self.status = Some(status);
    }

    /// Replaces the settings inside the status.
    pub fn set_settings(&mut self, settings: Settings) {
        if let Some(s) = self.status.as_mut() {
            s.settings = settings;
        }
    }

    /// Whether the daemon says the desktop is locked (SR-6).
    pub fn locked(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.locked)
    }

    /// Replaces the peer list, keeping what only events tell us (battery, presence, last
    /// seen, diagnostics) for devices that stay.
    pub fn set_peers(&mut self, peers: Vec<PeerSummary>, now: u64) {
        let old = std::mem::take(&mut self.peers);
        self.peers = peers
            .into_iter()
            .map(|p| {
                let mut v = PeerView::from_summary(p, now);
                if let Some(o) = old.iter().find(|o| o.device_id == v.device_id) {
                    v.battery = o.battery;
                    if v.online && v.grants.media {
                        v.media.clone_from(&o.media);
                    }
                    v.note.clone_from(&o.note);
                    if v.online || o.presence.as_deref() == Some(ASLEEP) {
                        v.presence.clone_from(&o.presence);
                    }
                    if !v.online {
                        v.last_seen = v.last_seen.max(o.last_seen);
                    }
                }
                v
            })
            .collect();
        // Sessions seeded from `Status` before the peers were listed learn their IDs.
        for (id, label) in &mut self.input_sessions {
            if id.is_empty() {
                if let Some(p) = self.peers.iter().find(|p| p.label == *label) {
                    id.clone_from(&p.device_id);
                }
            }
        }
    }

    /// Replaces the transfer list, keeping saved paths and offered names already known.
    pub fn set_transfers(&mut self, transfers: Vec<TransferSnapshot>) {
        let old = std::mem::take(&mut self.transfers);
        self.transfers = transfers
            .into_iter()
            .map(|s| {
                let mut v = TransferView::from_snapshot(s);
                if let Some(o) = old
                    .iter()
                    .find(|o| o.id == v.id && o.direction == v.direction)
                {
                    v.names.clone_from(&o.names);
                    if v.saved.is_empty() {
                        v.saved.clone_from(&o.saved);
                    }
                    v.elapsed_ms = o.elapsed_ms;
                }
                v
            })
            .collect();
    }

    /// Drops finished transfers from the list (local only; the daemon keeps its own).
    pub fn clear_finished(&mut self) {
        self.transfers.retain(|t| !t.state.is_finished());
    }

    /// Stores new grants for peer `i`. Turning `media` off drops the media state shown for
    /// it, since none arrives without the grant.
    pub fn set_peer_grants(&mut self, i: usize, grants: Grants) {
        if let Some(p) = self.peers.get_mut(i) {
            p.grants = grants;
            if !grants.media {
                p.media.clear();
            }
        }
    }

    /// Index of a peer by hex Device ID.
    pub fn peer_index(&self, device_id: &str) -> Option<usize> {
        self.peers.iter().position(|p| p.device_id == device_id)
    }

    /// Index of a transfer by ID and side.
    pub fn transfer_index(&self, id: &str, direction: TransferDirection) -> Option<usize> {
        self.transfers
            .iter()
            .position(|t| t.id == id && t.direction == direction)
    }

    /// The display name of a peer: its label, else its Short ID.
    pub fn peer_name(&self, device_id: &str) -> String {
        self.peers
            .iter()
            .find(|p| p.device_id == device_id)
            .map_or_else(|| short_from_hex(device_id), |p| p.label.clone())
    }

    /// Applies one daemon event.
    pub fn apply(&mut self, event: Event, now: u64) -> Vec<Change> {
        match event {
            Event::Host(e) => self.apply_host(e, now),
            Event::Daemon(e) => self.apply_daemon(e),
            _ => Vec::new(),
        }
    }

    fn apply_daemon(&mut self, event: DaemonEvent) -> Vec<Change> {
        match event {
            DaemonEvent::SettingsChanged { settings } => {
                // The daemon says why "Start at login" is off; a reason that is new
                // (not the one already on screen) is also announced once.
                let before = self
                    .status
                    .as_ref()
                    .and_then(|s| s.settings.autostart_refused.clone());
                let reason = settings.autostart_refused.clone();
                self.set_settings(settings);
                match reason {
                    Some(text) if before.as_ref() != Some(&text) => {
                        vec![Change::Settings, Change::Notice(text, true)]
                    }
                    _ => vec![Change::Settings],
                }
            }
            DaemonEvent::ClipboardPushed { sent_to } => {
                vec![Change::Notice(clipboard_sent_text(sent_to.len()), false)]
            }
            DaemonEvent::ClipboardApplied { peer, .. } => vec![Change::Notice(
                format!("Clipboard received from {}", self.peer_name(&peer)),
                false,
            )],
            DaemonEvent::LockChanged { locked } => {
                if let Some(s) = self.status.as_mut() {
                    s.locked = locked;
                }
                let mut changes = vec![Change::Lock];
                // Locking ends every input session (SR-25); do not wait for the events.
                if locked {
                    changes.extend(self.clear_input_sessions());
                }
                changes
            }
            DaemonEvent::HostRestarted => {
                let mut changes = self.clear_input_sessions();
                changes.push(Change::Refetch(Refetch::All));
                changes
            }
            DaemonEvent::InputSession {
                peer,
                label,
                active,
            } => self.set_input_session(peer, label, active),
            DaemonEvent::ShuttingDown => {
                let mut changes = self.clear_input_sessions();
                changes.push(Change::ShuttingDown);
                changes
            }
            _ => Vec::new(),
        }
    }

    /// Records a remote input session starting or ending. Returns
    /// [`Change::InputSessions`] when the set changed.
    fn set_input_session(&mut self, peer: String, label: String, active: bool) -> Vec<Change> {
        // An entry seeded from `Status` without a known ID matches by label.
        let at = self
            .input_sessions
            .iter()
            .position(|(p, _)| *p == peer)
            .or_else(|| {
                self.input_sessions
                    .iter()
                    .position(|(p, l)| p.is_empty() && !label.is_empty() && *l == label)
            });
        match (active, at) {
            (true, Some(i)) => {
                let entry = &mut self.input_sessions[i];
                if entry.0 == peer && entry.1 == label {
                    return Vec::new();
                }
                let renamed = entry.1 != label;
                entry.0 = peer;
                entry.1 = label;
                if !renamed {
                    return Vec::new();
                }
            }
            (true, None) => self.input_sessions.push((peer, label)),
            (false, Some(i)) => {
                self.input_sessions.remove(i);
            }
            (false, None) => return Vec::new(),
        }
        vec![Change::InputSessions]
    }

    /// Forgets every input session (lock, host restart, daemon gone). Returns
    /// [`Change::InputSessions`] when there were any.
    pub fn clear_input_sessions(&mut self) -> Vec<Change> {
        if self.input_sessions.is_empty() {
            Vec::new()
        } else {
            self.input_sessions.clear();
            vec![Change::InputSessions]
        }
    }

    // One arm per event keeps the mapping readable; splitting it would only scatter it.
    #[allow(clippy::too_many_lines)]
    fn apply_host(&mut self, event: HostEvent, now: u64) -> Vec<Change> {
        match event {
            HostEvent::PeerOnline { peer, .. } => self.with_peer(&peer, |p| {
                p.online = true;
                p.presence = None;
                p.note = None;
                p.last_seen = Some(now);
            }),
            HostEvent::PeerOffline { peer, .. } => {
                let mut changes = self.with_peer(&peer, |p| {
                    p.online = false;
                    if p.presence.as_deref() != Some(ASLEEP) {
                        p.presence = None;
                    }
                    p.last_seen = Some(now);
                    p.capabilities.clear();
                    p.media.clear();
                });
                // A dropped connection ends its input session (protocol.md 5.12).
                changes.extend(self.set_input_session(peer, String::new(), false));
                changes
            }
            HostEvent::MediaState { peer, state } => {
                self.with_peer(&peer, |p| merge_media(&mut p.media, state))
            }
            HostEvent::PeerRefused {
                peer,
                frame_type,
                code,
                ..
            } => {
                if code == "forbidden" {
                    vec![Change::Notice(
                        refusal_text(&self.peer_name(&peer), frame_type.as_deref()),
                        true,
                    )]
                } else {
                    let feature = refused_feature(frame_type.as_deref());
                    tracing::debug!(code, feature, "peer refused a frame");
                    Vec::new()
                }
            }
            HostEvent::Battery {
                peer,
                level,
                charging,
                ..
            } => self.with_peer(&peer, |p| p.battery = Some((level, charging))),
            HostEvent::Presence { peer, state } => {
                self.with_peer(&peer, |p| p.presence = Some(state))
            }
            HostEvent::DiscoveryDiagnostic {
                peer,
                path,
                failure,
                ..
            } => self.with_peer(&peer, |p| {
                p.note = Some(format!("Not reached via {}: {failure}", path_name(path)));
            }),
            HostEvent::PairingPrompt {
                short_id,
                name,
                prompt,
                ..
            } => {
                self.prompt = Some(PromptView {
                    sas: match prompt {
                        PairingPromptKind::Sas { sas } => Some(sas.0),
                        PairingPromptKind::ScannerConfirm => None,
                    },
                    name,
                    short_id,
                });
                vec![Change::Prompt]
            }
            HostEvent::Paired { short_id, name, .. } => {
                self.prompt = None;
                self.candidates.clear();
                vec![
                    Change::Prompt,
                    Change::Candidates,
                    Change::WindowClosed,
                    Change::PairingStatus(format!("Paired with {name} ({short_id})."), false),
                    Change::Refetch(Refetch::Peers),
                ]
            }
            HostEvent::PairingFailed { reason } => {
                self.prompt = None;
                vec![
                    Change::Prompt,
                    Change::PairingStatus(format!("Pairing failed: {reason}"), true),
                ]
            }
            HostEvent::RemovedByPeer { peer, .. } => vec![
                Change::Notice(
                    format!(
                        "{} no longer trusts this computer. Pair again to use it.",
                        self.peer_name(&peer)
                    ),
                    true,
                ),
                Change::Refetch(Refetch::Peers),
            ],
            HostEvent::Unpaired { peer, .. } => match self.peer_index(&peer) {
                Some(i) => {
                    self.peers.remove(i);
                    vec![Change::Peers]
                }
                None => Vec::new(),
            },
            HostEvent::AddDeviceSeen {
                name,
                device_type,
                addr,
            } => {
                let c = AddDeviceCandidate {
                    name,
                    device_type,
                    addr,
                };
                if merge_candidate(&mut self.candidates, c) {
                    vec![Change::Candidates]
                } else {
                    Vec::new()
                }
            }
            HostEvent::TransferOffer {
                id,
                peer,
                short_id,
                files,
                file_count,
                total_bytes,
                note,
            } => {
                let names = files
                    .into_iter()
                    .take(OFFER_NAMES_SHOWN)
                    .map(|f| f.path)
                    .collect();
                let who = self.peer_name(&peer);
                let mut changes =
                    self.upsert_transfer(&id, &peer, TransferDirection::Receive, |t| {
                        t.short_id = short_id;
                        t.state = TransferStatus::Offered;
                        t.file_count = file_count;
                        t.total = total_bytes;
                        t.names = names;
                        t.note = note;
                    });
                changes.push(Change::Notice(
                    format!(
                        "{who} wants to send {} ({}). Answer in Transfers.",
                        count_files(file_count),
                        format_bytes(total_bytes)
                    ),
                    false,
                ));
                changes
            }
            HostEvent::TransferProgress {
                id,
                peer,
                direction,
                bytes,
                total,
                bytes_per_sec,
                files_done,
                ..
            } => self.upsert_transfer(&id, &peer, direction, |t| {
                t.state = TransferStatus::Active;
                t.bytes = bytes;
                t.total = total;
                t.bytes_per_sec = bytes_per_sec;
                t.files_done = files_done;
            }),
            HostEvent::TransferComplete {
                id,
                peer,
                direction,
                bytes,
                elapsed_ms,
                roots,
                saved,
                ..
            } => self.upsert_transfer(&id, &peer, direction, |t| {
                t.state = TransferStatus::Complete;
                t.bytes = bytes;
                t.total = bytes;
                t.bytes_per_sec = 0;
                if t.file_count == 0 {
                    t.file_count = roots.len();
                }
                t.saved = saved;
                t.elapsed_ms = Some(elapsed_ms);
            }),
            HostEvent::TransferFailed {
                id,
                peer,
                direction,
                reason,
            } => self.upsert_transfer(&id, &peer, direction, |t| {
                t.state = TransferStatus::Failed;
                t.bytes_per_sec = 0;
                t.note = Some(reason);
            }),
            HostEvent::Clipboard { peer, .. } => vec![Change::Notice(
                format!("Clipboard received from {}", self.peer_name(&peer)),
                false,
            )],
            HostEvent::ClipboardImage { peer, .. } => vec![Change::Notice(
                format!("Clipboard image received from {}", self.peer_name(&peer)),
                false,
            )],
            HostEvent::Ping { peer, message } => {
                let who = self.peer_name(&peer);
                let text = match message {
                    Some(m) if !m.is_empty() => format!("Ping from {who}: {m}"),
                    _ => format!("Ping from {who}"),
                };
                vec![Change::Notice(text, false)]
            }
            HostEvent::Find { peer, start } => {
                let who = self.peer_name(&peer);
                let text = if start {
                    format!("{who} is looking for this computer")
                } else {
                    format!("{who} stopped looking for this computer")
                };
                vec![Change::Notice(text, false)]
            }
            _ => Vec::new(),
        }
    }

    fn with_peer(&mut self, device_id: &str, f: impl FnOnce(&mut PeerView)) -> Vec<Change> {
        match self.peer_index(device_id) {
            Some(i) => {
                f(&mut self.peers[i]);
                vec![Change::Peer(i)]
            }
            // A device we have not listed yet (paired from another client): reload.
            None => vec![Change::Refetch(Refetch::Peers)],
        }
    }

    fn upsert_transfer(
        &mut self,
        id: &str,
        peer: &str,
        direction: TransferDirection,
        f: impl FnOnce(&mut TransferView),
    ) -> Vec<Change> {
        if let Some(i) = self.transfer_index(id, direction) {
            f(&mut self.transfers[i]);
            vec![Change::Transfer(i)]
        } else {
            let mut t = TransferView::blank(id, peer, direction);
            f(&mut t);
            self.transfers.push(t);
            vec![Change::Transfers]
        }
    }
}

/// The first 16 base32 characters of a Device ID are its Short ID; without the decoder at
/// hand, a hex prefix is a readable stand-in for a peer we have not listed yet.
fn short_from_hex(device_id: &str) -> String {
    device_id.chars().take(8).collect()
}

/// Applies a peer's media state to its player list (protocol.md 5.9). A state that is
/// not playing and supports no action means the player is gone, so it is dropped;
/// anything else replaces that player's entry and moves it to the front.
pub fn merge_media(players: &mut Vec<MediaState>, state: MediaState) {
    players.retain(|m| m.player != state.player);
    if state.playing || !state.can.is_empty() {
        players.insert(0, state);
    }
}

/// The feature a peer refused, from the type of the frame we sent, for "<peer> does not
/// allow <feature>".
pub fn refused_feature(frame_type: Option<&str>) -> String {
    match frame_type {
        Some("input") => "remote input".to_owned(),
        Some("clipboard") => "clipboard".to_owned(),
        Some("find") => "find".to_owned(),
        Some("media") => "media control".to_owned(),
        Some(other) => other.to_owned(),
        None => "that request".to_owned(),
    }
}

/// The notice for a `forbidden` refusal. Media control says what to do about it, since
/// the grant lives on the other device and only its user can turn it on.
pub fn refusal_text(peer: &str, frame_type: Option<&str>) -> String {
    if frame_type == Some("media") {
        return format!(
            "{peer} has not allowed this computer to control its media. Turn on Media for this computer on {peer}."
        );
    }
    format!("{peer} does not allow {}", refused_feature(frame_type))
}

/// Whether a peer accepts remote input (any `input.*` capability). The desktop does not
/// control other devices in v1; the row carries this for a later phase.
pub fn can_be_controlled(capabilities: &[String]) -> bool {
    capabilities.iter().any(|c| c.starts_with("input."))
}

/// The banner text while remote input sessions are open: "Remote input from Phone",
/// "... from Phone and Tablet", "... from A, B and C". Empty when there are none.
pub fn input_banner_text(sessions: &[(String, String)]) -> String {
    let names: Vec<&str> = sessions
        .iter()
        .map(|(id, label)| {
            if label.is_empty() {
                id.get(..8).unwrap_or(id)
            } else {
                label.as_str()
            }
        })
        .collect();
    let list = match names.as_slice() {
        [] => return String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    format!("Remote input from {list}")
}

/// The "Remote input" line on the Settings screen.
pub fn remote_input_line(settings: &Settings) -> String {
    if settings.remote_input_supported {
        "Remote input: available (devices you allow can move the pointer and type here)".to_owned()
    } else {
        format!(
            "Remote input: unavailable: {}",
            remote_input_reason(settings)
        )
    }
}

/// Why remote input cannot be offered, for the greyed-out grant toggle.
pub fn remote_input_reason(settings: &Settings) -> String {
    settings
        .remote_input_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("this desktop gives PlayDrop no way to apply it")
        .to_owned()
}

/// The grants after the user flips one toggle, or `None` when that toggle is not offered:
/// an unknown name, or `input` while this desktop cannot apply remote input (SR-14,
/// PLAN.md anti-pattern 18). Turning `input` off is always allowed.
pub fn apply_grant(grants: Grants, grant: &str, on: bool, input_supported: bool) -> Option<Grants> {
    let mut g = grants;
    match grant {
        "files" => g.files = on,
        "clipboard_in" => g.clipboard_in = on,
        "clipboard_out" => g.clipboard_out = on,
        "find" => g.find = on,
        "media" => g.media = on,
        "input" if input_supported || !on => g.input = on,
        _ => return None,
    }
    Some(g)
}

/// User-facing name of a discovery path.
pub fn path_name(path: DiscoveryPath) -> &'static str {
    match path {
        DiscoveryPath::Mdns => "mDNS",
        DiscoveryPath::Beacon => "the multicast beacon",
        DiscoveryPath::LastKnown => "the last-known address",
    }
}

fn count_files(n: usize) -> String {
    if n == 1 {
        "1 file".to_owned()
    } else {
        format!("{n} files")
    }
}

/// "Clipboard not sent: <reason>", with the daemon's own wording of the reason.
pub fn clipboard_skipped_text(skip: ClipSkip) -> String {
    format!("Clipboard not sent: {}", skip.reason())
}

/// "Clipboard sent to 2 devices" or the reason nothing was sent.
pub fn clipboard_sent_text(n: usize) -> String {
    match n {
        0 => clipboard_skipped_text(ClipSkip::NoPeers),
        1 => "Clipboard sent to 1 device".to_owned(),
        n => format!("Clipboard sent to {n} devices"),
    }
}

// Display precision is all these conversions need.
#[allow(clippy::cast_precision_loss)]
/// Bytes in binary units with one decimal: "512 B", "1.5 KiB", "12.0 MiB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A rate: "30.0 MiB/s".
pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_bytes(bytes_per_sec))
}

/// Time left at the current rate, or `None` when it cannot be estimated.
pub fn format_eta(remaining: u64, bytes_per_sec: u64) -> Option<String> {
    if bytes_per_sec == 0 {
        return None;
    }
    let secs = remaining.div_ceil(bytes_per_sec);
    Some(format_duration(secs))
}

/// "less than 1 s", "45 s", "2 min 5 s", "1 h 3 min".
pub fn format_duration(secs: u64) -> String {
    match secs {
        0 => "less than 1 s".to_owned(),
        1..=59 => format!("{secs} s"),
        60..=3599 => {
            let (m, s) = (secs / 60, secs % 60);
            if s == 0 {
                format!("{m} min")
            } else {
                format!("{m} min {s} s")
            }
        }
        _ => format!("{} h {} min", secs / 3600, (secs % 3600) / 60),
    }
}

/// How long ago: "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn format_ago(then: u64, now: u64) -> String {
    let d = now.saturating_sub(then);
    match d {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        86_400..=172_799 => "1 day ago".to_owned(),
        _ => format!("{} days ago", d / 86_400),
    }
}

/// A Unix time as a UTC calendar date, "2026-10-08". UTC because the workspace carries no
/// time-zone database; a date is off by at most one day.
pub fn format_date(unix: u64) -> String {
    // Howard Hinnant's days-to-civil algorithm, valid for every date after 1970.
    let days = i64::try_from(unix / 86_400).unwrap_or(i64::MAX / 2);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Splits a six-digit SAS in two groups for reading aloud: "123 456".
pub fn format_sas(sas: &str) -> String {
    if sas.len() == 6 && sas.is_ascii() {
        format!("{} {}", &sas[..3], &sas[3..])
    } else {
        sas.to_owned()
    }
}

/// Parses a typed pairing address. A bare IP gets the default port.
pub fn parse_pair_addr(text: &str) -> Option<SocketAddr> {
    let t = text.trim();
    if let Ok(a) = t.parse::<SocketAddr>() {
        return Some(a);
    }
    let ip = t.trim_start_matches('[').trim_end_matches(']');
    ip.parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| SocketAddr::new(ip, DEFAULT_PORT))
}

/// The `presence` state a phone sends before Doze or the background drops it.
const ASLEEP: &str = "asleep";

fn state_text(p: &PeerView) -> &'static str {
    if !p.online {
        // A device that said it was going to sleep is asleep, not gone (PLAN.md Phase 3:
        // "honestly shown as asleep").
        return if p.presence.as_deref() == Some(ASLEEP) {
            "Asleep"
        } else {
            "Offline"
        };
    }
    match p.presence.as_deref() {
        Some(ASLEEP) => "Asleep",
        Some("idle") => "Idle",
        Some("locked") => "Locked",
        _ => "Online",
    }
}

/// The device a drag-and-drop goes to: the one selected row (`selected` is the Slint
/// index, -1 for none). `None` when nothing or a stale index is selected.
pub fn drop_target(selected: i32, peers: &[PeerView]) -> Option<String> {
    let i = usize::try_from(selected).ok()?;
    peers.get(i).map(|p| p.device_id.clone())
}

/// A peer as a Slint row.
pub fn device_row(p: &PeerView, now: u64) -> DeviceRow {
    let battery = p.battery.map_or_else(String::new, |(level, charging)| {
        if charging {
            format!("{level}%, charging")
        } else {
            format!("{level}%")
        }
    });
    let last_seen = if p.online {
        "now".to_owned()
    } else {
        p.last_seen
            .map_or_else(|| "never".to_owned(), |t| format_ago(t, now))
    };
    let can_control_media = p.capabilities.iter().any(|c| c == MEDIA_CONTROL);
    let players: Vec<MediaRow> = ordered_players(&p.media)
        .into_iter()
        .map(|m| media_row(m, can_control_media))
        .collect();
    DeviceRow {
        id: p.device_id.as_str().into(),
        label: p.label.as_str().into(),
        short_id: p.short_id.as_str().into(),
        state: state_text(p).into(),
        online: p.online,
        battery: battery.into(),
        paired: format_date(p.paired_at).into(),
        last_seen: last_seen.into(),
        note: p.note.clone().unwrap_or_default().into(),
        grant_files: p.grants.files,
        grant_clipboard_in: p.grants.clipboard_in,
        grant_clipboard_out: p.grants.clipboard_out,
        grant_find: p.grants.find,
        grant_input: p.grants.input,
        grant_media: p.grants.media,
        can_be_controlled: can_be_controlled(&p.capabilities),
        can_control_media,
        players: ModelRc::new(VecModel::from(players)),
    }
}

/// The capability a peer lists when it takes media commands (protocol.md 5.9).
pub const MEDIA_CONTROL: &str = "media.control";

/// The players a device row lists: the playing ones first, then the rest, each group most
/// recently updated first.
pub fn ordered_players(players: &[MediaState]) -> Vec<&MediaState> {
    let (mut playing, idle): (Vec<_>, Vec<_>) = players.iter().partition(|m| m.playing);
    playing.extend(idle);
    playing
}

/// One player as a Slint row. Every control is offered only when the player lists the
/// action and the device takes commands at all (anti-pattern 18: no button that cannot
/// work).
pub fn media_row(m: &MediaState, controllable: bool) -> MediaRow {
    let can = |a: MediaAction| controllable && m.can.contains(&a);
    let toggle = toggle_action(m).filter(|_| controllable);
    MediaRow {
        player: m.player.as_str().into(),
        title: media_title(m).into(),
        detail: media_detail(m).into(),
        playing: m.playing,
        can_previous: can(MediaAction::Previous),
        can_next: can(MediaAction::Next),
        toggle_action: toggle
            .map(|a| a.as_str().to_owned())
            .unwrap_or_default()
            .into(),
        has_volume: can(MediaAction::Volume) && m.volume.is_some(),
        volume: i32::from(m.volume.unwrap_or(0).min(100)),
    }
}

/// What the play/pause button sends: `playPause` when the player has it, else `pause`
/// while playing or `play` while paused, when that one is listed.
pub fn toggle_action(m: &MediaState) -> Option<MediaAction> {
    if m.can.contains(&MediaAction::PlayPause) {
        return Some(MediaAction::PlayPause);
    }
    let want = if m.playing {
        MediaAction::Pause
    } else {
        MediaAction::Play
    };
    m.can.contains(&want).then_some(want)
}

/// A media command from the UI: the action in wire spelling and, for `volume`, the level.
/// `None` for an action this window never sends.
pub fn media_command(action: &str, volume: i32) -> Option<(MediaAction, Option<u64>)> {
    let action = MediaAction::from_wire(action);
    match action {
        MediaAction::Play
        | MediaAction::Pause
        | MediaAction::PlayPause
        | MediaAction::Next
        | MediaAction::Previous => Some((action, None)),
        MediaAction::Volume => Some((action, u64::try_from(volume.clamp(0, 100)).ok())),
        _ => None,
    }
}

/// The title line of a now-playing block: the track title, else the player's name.
fn media_title(m: &MediaState) -> String {
    m.title
        .clone()
        .filter(|t| !t.is_empty())
        .or_else(|| m.name.clone())
        .unwrap_or_else(|| m.player.clone())
}

/// The second line: "Artist · Player · Paused" with the parts that are known.
fn media_detail(m: &MediaState) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if let Some(a) = m.artist.as_deref().filter(|a| !a.is_empty()) {
        parts.push(a);
    }
    if let Some(n) = m.name.as_deref().filter(|n| !n.is_empty()) {
        parts.push(n);
    }
    parts.push(if m.playing { "Playing" } else { "Paused" });
    parts.join(" · ")
}

/// Adds one advert to the candidate list, one row per device: the core reports every
/// address it hears a device on (a laptop with Docker bridges and IPv6 shows up five
/// times), and before pairing the name plus type is the only identity there is. The row
/// keeps the address most likely to work: an IPv4 address beats an IPv6 one, and a
/// link-local IPv6 address loses to anything else. Returns whether the list changed.
pub fn merge_candidate(list: &mut Vec<AddDeviceCandidate>, c: AddDeviceCandidate) -> bool {
    let Some(existing) = list
        .iter_mut()
        .find(|o| o.name == c.name && o.device_type == c.device_type)
    else {
        list.push(c);
        return true;
    };
    if existing.addr == c.addr {
        return false;
    }
    if addr_rank(c.addr) < addr_rank(existing.addr) {
        existing.addr = c.addr;
        return true;
    }
    false
}

/// Folds a full candidate list (the daemon's `AddDeviceList`) through [`merge_candidate`].
pub fn dedup_candidates(list: Vec<AddDeviceCandidate>) -> Vec<AddDeviceCandidate> {
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        merge_candidate(&mut out, c);
    }
    out
}

/// Lower is better for dialling by hand: routable IPv4, then other IPv4, then global
/// IPv6, then link-local IPv6.
fn addr_rank(addr: SocketAddr) -> u8 {
    match addr.ip() {
        std::net::IpAddr::V4(v4) if v4.is_private() => 0,
        std::net::IpAddr::V4(_) => 1,
        std::net::IpAddr::V6(v6) if v6.to_ipv4_mapped().is_some() => 1,
        std::net::IpAddr::V6(v6) if (v6.segments()[0] & 0xffc0) == 0xfe80 => 3,
        std::net::IpAddr::V6(_) => 2,
    }
}

/// A candidate as a Slint row.
pub fn candidate_row(c: &AddDeviceCandidate) -> CandidateRow {
    CandidateRow {
        name: c.name.as_str().into(),
        device_type: device_type_text(&c.device_type).into(),
        addr: c.addr.to_string().into(),
    }
}

fn device_type_text(token: &str) -> String {
    match token {
        "desktop" => "Computer".to_owned(),
        "laptop" => "Laptop".to_owned(),
        "phone" | "mobile" => "Phone".to_owned(),
        "tablet" => "Tablet".to_owned(),
        other => other.to_owned(),
    }
}

// Display precision is all the progress fraction needs.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
fn fraction(bytes: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (bytes as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

/// A transfer as a Slint row. `peer_name` is the label of the other device.
pub fn transfer_row(t: &TransferView, peer_name: &str) -> TransferRow {
    let receive = t.direction == TransferDirection::Receive;
    let title = if receive {
        format!("From {peer_name}")
    } else {
        format!("To {peer_name}")
    };
    let mut files = if t.file_count > 0 {
        format!("{}, {}", count_files(t.file_count), format_bytes(t.total))
    } else {
        format_bytes(t.total)
    };
    if !t.names.is_empty() {
        files.push_str(": ");
        files.push_str(&t.names.join(", "));
        let more = t.file_count.saturating_sub(t.names.len());
        if more > 0 {
            let _ = write!(files, " and {more} more");
        }
    }
    let (state, status) = match t.state {
        TransferStatus::Offered if receive => ("offered", "Waiting for you".to_owned()),
        TransferStatus::Offered => ("offered", "Waiting for the other device".to_owned()),
        TransferStatus::Active => (
            "active",
            format!("{:.0}%", f64::from(fraction(t.bytes, t.total)) * 100.0),
        ),
        TransferStatus::Complete => ("complete", "Done".to_owned()),
        TransferStatus::Failed => ("failed", "Failed".to_owned()),
        TransferStatus::Cancelled => ("cancelled", "Cancelled".to_owned()),
    };
    let detail = match t.state {
        TransferStatus::Active => {
            let mut d = format!("{} of {}", format_bytes(t.bytes), format_bytes(t.total));
            // A single file has nothing to count, and 0 means the host sent no count.
            if t.file_count > 1 && t.files_done > 0 {
                let _ = write!(d, " · {} of {} files", t.files_done, t.file_count);
            }
            if t.bytes_per_sec > 0 {
                d.push_str(" · ");
                d.push_str(&format_rate(t.bytes_per_sec));
            }
            if let Some(eta) = format_eta(t.total.saturating_sub(t.bytes), t.bytes_per_sec) {
                d.push_str(" · ");
                d.push_str(&eta);
                d.push_str(" left");
            }
            d
        }
        TransferStatus::Complete => t
            .elapsed_ms
            .map(|ms| format!("Took {}", format_duration(ms / 1000)))
            .unwrap_or_default(),
        TransferStatus::Failed | TransferStatus::Cancelled | TransferStatus::Offered => {
            t.note.clone().unwrap_or_default()
        }
    };
    let finished = t.state.is_finished();
    TransferRow {
        id: t.id.as_str().into(),
        receive,
        title: title.into(),
        files: files.into(),
        state: state.into(),
        status_text: status.into(),
        progress: fraction(t.bytes, t.total),
        show_progress: t.state == TransferStatus::Active,
        detail: detail.into(),
        can_respond: receive && t.state == TransferStatus::Offered,
        can_cancel: !(finished || receive && t.state == TransferStatus::Offered),
        can_open: receive && t.state == TransferStatus::Complete && t.saved.len() == 1,
        can_show: receive && t.state == TransferStatus::Complete && !t.saved.is_empty(),
    }
}

/// The Diagnostics screen: summary lines and detail sections.
pub fn diagnostics_view(report: &DiagnosticsReport) -> (Vec<String>, Vec<DiagSection>) {
    let core = &report.core;
    let mut sections = vec![
        interfaces_section(core),
        multicast_section(&core.multicast),
        mdns_section(&core.mdns),
    ];
    if !core.peers.is_empty() {
        sections.push(peers_section(&core.peers));
    }
    sections.push(port_section(&report.port));
    if let Some(fw) = &report.port.firewall {
        sections.push(firewall_section(fw, report.port.port));
    }
    (report.summary.clone(), sections)
}

/// The daemon's summary line when nothing is wrong (`playdropd` `probe::ALL_FINE`).
const ALL_FINE: &str = "all paths fine";

/// Summary lines that are not a discovery problem: information such as KDE Connect being
/// installed alongside, and the start-at-login refusal. They never open the help panel.
const NOT_NETWORK: [&str; 2] = ["note: ", "start at login"];

/// Whether the Diagnose summary reports a failed network layer, which shows the "Can't
/// find your device?" panel on the Diagnostics page.
pub fn network_failed(summary: &[String]) -> bool {
    summary
        .iter()
        .any(|l| l != ALL_FINE && !NOT_NETWORK.iter().any(|p| l.starts_with(p)))
}

/// Which way out the help panel puts first, from the Diagnose summary, and the firewall
/// command when that is the fix. Only unambiguous lines pick one: a firewall that blocks
/// the port (its own command), multicast blocked (pair by QR or address, then dial
/// directly), no interface on the peer's network (hotspot or USB tether). Anything else
/// leaves the panel in its usual order.
pub fn help_focus(summary: &[String]) -> (HelpFocus, String) {
    if let Some(line) = summary
        .iter()
        .find(|l| l.contains("blocked by the host firewall"))
    {
        let fix = line.split('`').nth(1).unwrap_or_default().to_owned();
        if !fix.is_empty() {
            return (HelpFocus::Firewall, fix);
        }
    }
    if summary
        .iter()
        .any(|l| l.starts_with("no interface on the same network as"))
    {
        return (HelpFocus::HotspotOrUsb, String::new());
    }
    if summary.iter().any(|l| l.starts_with("multicast blocked")) {
        return (HelpFocus::QrOrAddress, String::new());
    }
    (HelpFocus::None, String::new())
}

fn interfaces_section(core: &Diagnostics) -> DiagSection {
    let lines = if core.interfaces.is_empty() {
        vec!["No usable network interface".to_owned()]
    } else {
        core.interfaces
            .iter()
            .map(|i| format!("{} mask {} (interface {})", i.addr, i.netmask, i.index))
            .collect()
    };
    section("Network interfaces", lines, !core.interfaces.is_empty())
}

fn multicast_section(m: &MulticastStatus) -> DiagSection {
    let mut lines = Vec::new();
    if m.enabled {
        lines.extend(m.joined.iter().map(|a| format!("Joined on {a}")));
        lines.extend(
            m.join_failed
                .iter()
                .map(|f| format!("Join failed on {}: {}", f.addr, f.error)),
        );
        lines.push(match m.last_beacon_secs_ago {
            Some(secs) => format!("Beacon from another device {} ago", format_duration(secs)),
            None => "No beacon from another device since start".to_owned(),
        });
    } else {
        lines.push("Turned off".to_owned());
    }
    let ok = m.enabled && m.join_failed.is_empty() && m.beacon_recently;
    section("Multicast beacon", lines, ok)
}

fn mdns_section(m: &MdnsStatus) -> DiagSection {
    let mut lines = Vec::new();
    if m.enabled {
        lines.push(if m.registered {
            "This computer is registered".to_owned()
        } else {
            "This computer is not registered".to_owned()
        });
        if let Some(e) = &m.error {
            lines.push(format!("Error: {e}"));
        }
        lines.push(match m.last_seen_secs_ago {
            Some(secs) => format!("Advert from another device {} ago", format_duration(secs)),
            None => "No advert from another device since start".to_owned(),
        });
    } else {
        lines.push("Turned off".to_owned());
    }
    let ok = m.enabled && m.registered && m.error.is_none();
    section("mDNS", lines, ok)
}

fn peers_section(peers: &[PeerDiagnostic]) -> DiagSection {
    let mut ok = true;
    let mut lines = Vec::new();
    for p in peers {
        lines.push(format!(
            "{} ({}): {}; seen via mDNS {}, beacon {}",
            p.label,
            p.short_id,
            if p.online { "online" } else { "offline" },
            yes_no(p.mdns_seen),
            yes_no(p.beacon_seen)
        ));
        if let Some(f) = &p.last_result {
            ok &= p.online;
            lines.push(format!(
                "    Last failure via {}: {}",
                path_name(f.path),
                f.failure
            ));
        }
    }
    section("Paired devices", lines, ok)
}

fn port_section(port: &PortProbe) -> DiagSection {
    let loopback = if port.loopback_ok {
        "reachable"
    } else {
        "not reachable"
    };
    let mut lines = vec![format!("Loopback: {loopback}")];
    lines.extend(port.lan.iter().map(|a| {
        if a.ok {
            format!("{}: reachable", a.addr)
        } else {
            let why = a.error.as_deref().unwrap_or("no answer");
            format!("{}: blocked ({why})", a.addr)
        }
    }));
    let ok = port.loopback_ok && port.lan.iter().all(|a| a.ok);
    section(&format!("Port {}", port.port), lines, ok)
}

fn firewall_section(fw: &FirewallHint, port: u16) -> DiagSection {
    let active = if fw.active { "active" } else { "inactive" };
    let mut lines = vec![format!("{}: {active}", fw.tool)];
    match fw.port_allowed {
        Some(true) => lines.push(format!("Port {port} allowed")),
        Some(false) => lines.push(format!("Port {port} not allowed")),
        None => {}
    }
    if !fw.fix.is_empty() {
        lines.push(format!("Fix: {}", fw.fix));
    }
    let ok = !fw.active || fw.port_allowed != Some(false);
    section("Firewall", lines, ok)
}

fn yes_no(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn section(title: &str, lines: Vec<String>, ok: bool) -> DiagSection {
    let lines: Vec<SharedString> = lines.into_iter().map(SharedString::from).collect();
    DiagSection {
        title: title.into(),
        lines: ModelRc::new(VecModel::from(lines)),
        ok,
    }
}

#[cfg(test)]
// `assert!(x.is_empty())` reads clearer than comparing to `vec![]`. The lint is new in
// clippy 1.99; `unknown_lints` keeps older toolchains (CI) from rejecting the allow.
#[allow(unknown_lints, clippy::assert_is_empty)]
mod tests {
    use super::*;
    use playdrop_core::host::{OfferedFile, Secret};
    use playdrop_core::session::types::MediaAction;
    use slint::Model;

    fn peer(id: &str, online: bool) -> PeerSummary {
        PeerSummary {
            device_id: id.to_owned(),
            short_id: "K3QD-7F2M-XA9P-41BC".to_owned(),
            label: "Phone".to_owned(),
            online,
            grants: Grants::default(),
            last_known: Vec::new(),
            paired_at: 1_791_417_600, // 2026-10-08
            last_seen: None,
            removed_by_peer: false,
            capabilities: Vec::new(),
        }
    }

    fn settings(supported: bool, reason: Option<&str>) -> Settings {
        Settings {
            receive_dir: PathBuf::from("/tmp/in"),
            device_name: "desk".into(),
            clipboard_sync: true,
            clipboard_images: true,
            clipboard_watch_supported: true,
            remote_input_supported: supported,
            remote_input_reason: reason.map(str::to_owned),
            autostart: false,
            autostart_refused: None,
            notifications: true,
        }
    }

    fn media(player: &str, playing: bool, can: Vec<MediaAction>) -> MediaState {
        MediaState {
            player: player.into(),
            name: Some("Spotify".into()),
            playing,
            title: Some("Song".into()),
            artist: Some("Band".into()),
            album: None,
            duration_ms: None,
            position_ms: None,
            position_at_ms: None,
            volume: None,
            can,
        }
    }

    fn session(peer: &str, label: &str, active: bool) -> Event {
        Event::Daemon(DaemonEvent::InputSession {
            peer: peer.into(),
            label: label.into(),
            active,
        })
    }

    #[test]
    fn grant_toggles_offer_media_and_input_only_when_supported() {
        let g = Grants::default();
        assert!(!g.input && g.media);
        let off = apply_grant(g, "media", false, false).unwrap();
        assert!(!off.media);
        assert_eq!(apply_grant(g, "input", true, false), None);
        assert!(apply_grant(g, "input", true, true).unwrap().input);
        // Turning input off always works, even where it cannot be applied.
        let on = Grants { input: true, ..g };
        assert!(!apply_grant(on, "input", false, false).unwrap().input);
        assert!(!apply_grant(g, "files", false, false).unwrap().files);
        assert_eq!(apply_grant(g, "bogus", true, true), None);
    }

    #[test]
    fn remote_input_settings_line_and_reason() {
        assert!(remote_input_line(&settings(true, None)).starts_with("Remote input: available ("));
        let s = settings(false, Some("the RemoteDesktop portal is missing"));
        assert_eq!(
            remote_input_line(&s),
            "Remote input: unavailable: the RemoteDesktop portal is missing"
        );
        assert_eq!(
            remote_input_reason(&s),
            "the RemoteDesktop portal is missing"
        );
        // No reason from the daemon still explains instead of showing nothing.
        assert!(!remote_input_reason(&settings(false, Some("  "))).is_empty());
        assert!(!remote_input_reason(&settings(false, None)).is_empty());
        // An older daemon's settings without the new fields read as unsupported.
        let mut v = serde_json::to_value(settings(true, None)).unwrap();
        v.as_object_mut().unwrap().remove("remote_input_supported");
        let old: Settings = serde_json::from_value(v).unwrap();
        assert!(!old.remote_input_supported);
    }

    #[test]
    fn input_sessions_drive_the_banner() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true), peer("bb", true)], 1000);
        assert_eq!(input_banner_text(&s.input_sessions), "");
        assert_eq!(
            s.apply(session("aa", "Phone", true), 1),
            vec![Change::InputSessions]
        );
        // A repeat start changes nothing.
        assert!(s.apply(session("aa", "Phone", true), 1).is_empty());
        assert_eq!(
            input_banner_text(&s.input_sessions),
            "Remote input from Phone"
        );
        s.apply(session("bb", "Tablet", true), 2);
        assert_eq!(
            input_banner_text(&s.input_sessions),
            "Remote input from Phone and Tablet"
        );
        s.apply(session("cc", "", true), 3);
        assert_eq!(
            input_banner_text(&s.input_sessions),
            "Remote input from Phone, Tablet and cc"
        );
        assert_eq!(
            s.apply(session("aa", "Phone", false), 4),
            vec![Change::InputSessions]
        );
        assert!(s.apply(session("aa", "Phone", false), 4).is_empty());
        assert_eq!(
            input_banner_text(&s.input_sessions),
            "Remote input from Tablet and cc"
        );
        // A dropped connection ends that peer's session.
        let ch = s.apply(
            Event::Host(HostEvent::PeerOffline {
                peer: "bb".into(),
                short_id: "K3QD".into(),
            }),
            5,
        );
        assert_eq!(ch, vec![Change::Peer(1), Change::InputSessions]);
        // Locking ends the rest.
        let ch = s.apply(Event::Daemon(DaemonEvent::LockChanged { locked: true }), 6);
        assert_eq!(ch, vec![Change::Lock, Change::InputSessions]);
        assert!(s.input_sessions.is_empty());
        // The daemon's JSON decodes.
        let e: Event = serde_json::from_value(serde_json::json!({
            "source": "daemon", "event": "input_session",
            "peer": "aa", "label": "Phone", "active": true
        }))
        .unwrap();
        s.apply(e, 7);
        let ch = s.apply(Event::Daemon(DaemonEvent::HostRestarted), 8);
        assert_eq!(
            ch,
            vec![Change::InputSessions, Change::Refetch(Refetch::All)]
        );
    }

    #[test]
    fn status_seeds_the_input_banner() {
        let status = |input_from: Vec<String>| Status {
            device_id: "me".into(),
            short_id: "ME".into(),
            addr: "127.0.0.1:42187".parse().unwrap(),
            daemon_version: "0".into(),
            ipc_version: 1,
            settings: settings(true, None),
            add_device_window_open: false,
            locked: false,
            input_from,
        };
        let mut s = Store::default();
        // Status before the peer list: seeded by label, ID learnt from the list.
        s.set_status(status(vec!["Phone".into()]));
        assert_eq!(
            input_banner_text(&s.input_sessions),
            "Remote input from Phone"
        );
        s.set_peers(vec![peer("aa", true)], 1000);
        assert_eq!(s.input_sessions, vec![("aa".into(), "Phone".into())]);
        // Later events keep adjusting it; a repeat start is no change.
        assert!(s.apply(session("aa", "Phone", true), 1).is_empty());
        assert_eq!(
            s.apply(session("aa", "Phone", false), 2),
            vec![Change::InputSessions]
        );
        assert!(s.input_sessions.is_empty());

        // A seeded label with no listed peer still matches its stop event by label.
        s.set_status(status(vec!["Tablet".into()]));
        assert_eq!(s.input_sessions, vec![(String::new(), "Tablet".into())]);
        assert_eq!(
            s.apply(session("bb", "Tablet", false), 3),
            vec![Change::InputSessions]
        );
        assert!(s.input_sessions.is_empty());

        // A fresh Status is the daemon's truth: an empty list clears the banner.
        s.apply(session("aa", "Phone", true), 4);
        s.set_status(status(Vec::new()));
        assert_eq!(input_banner_text(&s.input_sessions), "");
        // An older daemon without the field reads as no sessions.
        let mut v = serde_json::to_value(status(vec!["Phone".into()])).unwrap();
        v.as_object_mut().unwrap().remove("input_from");
        let old: Status = serde_json::from_value(v).unwrap();
        assert!(old.input_from.is_empty());
    }

    #[test]
    fn refusals_name_the_feature() {
        assert_eq!(refused_feature(Some("input")), "remote input");
        assert_eq!(refused_feature(Some("clipboard")), "clipboard");
        assert_eq!(refused_feature(Some("find")), "find");
        assert_eq!(refused_feature(Some("media")), "media control");
        assert_eq!(refused_feature(Some("ping")), "ping");
        assert_eq!(refused_feature(None), "that request");

        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true)], 1000);
        let refused: Event = serde_json::from_value(serde_json::json!({
            "source": "host", "event": "peer_refused", "peer": "aa",
            "frame_type": "input", "code": "forbidden", "message": null
        }))
        .unwrap();
        assert_eq!(
            s.apply(refused, 1),
            vec![Change::Notice(
                "Phone does not allow remote input".into(),
                true
            )]
        );
        let media_refused = HostEvent::PeerRefused {
            peer: "aa".into(),
            frame_type: Some("media".into()),
            code: "forbidden".into(),
            message: None,
        };
        assert_eq!(
            s.apply(Event::Host(media_refused), 2),
            vec![Change::Notice(
                "Phone has not allowed this computer to control its media. Turn on Media for this computer on Phone.".into(),
                true
            )]
        );
        // Other codes are not the user's to act on here.
        let other = HostEvent::PeerRefused {
            peer: "aa".into(),
            frame_type: Some("media".into()),
            code: "bad_request".into(),
            message: None,
        };
        assert!(s.apply(Event::Host(other), 2).is_empty());
    }

    #[test]
    fn media_state_fills_the_row() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true)], 1000);
        let row = device_row(&s.peers[0], 1000);
        assert_eq!(row.players.row_count(), 0);
        let ch = s.apply(
            Event::Host(HostEvent::MediaState {
                peer: "aa".into(),
                state: media("spotify", true, vec![MediaAction::PlayPause]),
            }),
            1001,
        );
        assert_eq!(ch, vec![Change::Peer(0)]);
        let row = device_row(&s.peers[0], 1001);
        let m = row.players.row_data(0).unwrap();
        assert!(m.playing);
        assert_eq!(m.title, "Song");
        assert_eq!(m.detail, "Band · Spotify · Playing");
        // The device does not list `media.control`: state only, no buttons.
        assert!(!row.can_control_media && m.toggle_action.is_empty());

        // Paused but still controllable: kept, shown as paused.
        s.apply(
            Event::Host(HostEvent::MediaState {
                peer: "aa".into(),
                state: media("spotify", false, vec![MediaAction::Play]),
            }),
            1002,
        );
        let row = device_row(&s.peers[0], 1002);
        let m = row.players.row_data(0).unwrap();
        assert!(!m.playing);
        assert_eq!(m.detail, "Band · Spotify · Paused");

        // A reload keeps it while online with the media grant.
        s.set_peers(vec![peer("aa", true)], 1003);
        assert_eq!(s.peers[0].media.len(), 1);

        // Turning the grant off drops it.
        let no_media = Grants {
            media: false,
            ..Grants::default()
        };
        s.set_peer_grants(0, no_media);
        assert_eq!(device_row(&s.peers[0], 1003).players.row_count(), 0);

        // Going offline drops it too.
        s.set_peer_grants(0, Grants::default());
        merge_media(&mut s.peers[0].media, media("spotify", true, Vec::new()));
        s.apply(
            Event::Host(HostEvent::PeerOffline {
                peer: "aa".into(),
                short_id: "K3QD".into(),
            }),
            1004,
        );
        assert!(s.peers[0].media.is_empty());
    }

    #[test]
    fn gone_players_are_dropped_and_playing_ones_win() {
        let mut players = Vec::new();
        merge_media(&mut players, media("a", true, vec![MediaAction::Pause]));
        merge_media(&mut players, media("b", false, vec![MediaAction::Play]));
        assert_eq!(players.len(), 2);
        // Most recent first, but a playing player is the one shown.
        assert_eq!(players[0].player, "b");
        let order: Vec<_> = ordered_players(&players)
            .iter()
            .map(|m| m.player.as_str())
            .collect();
        assert_eq!(order, ["a", "b"]);
        // Not playing and nothing it can do: the player is gone.
        merge_media(&mut players, media("a", false, Vec::new()));
        assert_eq!(players.len(), 1);
        assert_eq!(players[0].player, "b");
        merge_media(&mut players, media("b", false, Vec::new()));
        assert!(players.is_empty() && ordered_players(&players).is_empty());
        // A gone state for a player never seen adds nothing.
        merge_media(&mut players, media("z", false, Vec::new()));
        assert!(players.is_empty());
        // Untitled track falls back to the player name.
        let mut untitled = media("c", true, Vec::new());
        untitled.title = None;
        untitled.artist = None;
        assert_eq!(media_title(&untitled), "Spotify");
        assert_eq!(media_detail(&untitled), "Spotify · Playing");
    }

    #[test]
    fn media_controls_follow_the_actions_the_player_lists() {
        use MediaAction as A;
        let all = vec![A::Previous, A::PlayPause, A::Next, A::Volume];
        let mut m = media("spotify", true, all);
        m.volume = Some(40);
        let row = media_row(&m, true);
        assert!(row.can_previous && row.can_next && row.has_volume);
        assert_eq!(row.toggle_action, "playPause");
        assert_eq!(row.volume, 40);
        // Without `media.control` on the device nothing is offered.
        let row = media_row(&m, false);
        assert!(!row.can_previous && !row.can_next && !row.has_volume);
        assert!(row.toggle_action.is_empty());
        // Volume needs both the action and a reported level.
        m.volume = None;
        assert!(!media_row(&m, true).has_volume);

        // No playPause: pause while playing, play while paused, nothing when not listed.
        let playing = media("a", true, vec![A::Play, A::Pause]);
        assert_eq!(toggle_action(&playing), Some(A::Pause));
        let paused = media("a", false, vec![A::Play, A::Pause]);
        assert_eq!(toggle_action(&paused), Some(A::Play));
        let stuck = media("a", true, vec![A::Play, A::Next]);
        assert_eq!(toggle_action(&stuck), None);
        let row = media_row(&stuck, true);
        assert!(row.toggle_action.is_empty() && row.can_next && !row.can_previous);

        // The row turns controls on once the device lists `media.control`.
        let mut s = Store::default();
        let mut p = peer("aa", true);
        p.capabilities = vec![MEDIA_CONTROL.into()];
        s.set_peers(vec![p], 1000);
        merge_media(
            &mut s.peers[0].media,
            media("spotify", false, vec![A::Play]),
        );
        let row = device_row(&s.peers[0], 1000);
        assert!(row.can_control_media);
        assert_eq!(row.players.row_data(0).unwrap().toggle_action, "play");

        assert_eq!(media_command("playPause", 0), Some((A::PlayPause, None)));
        assert_eq!(media_command("next", 0), Some((A::Next, None)));
        assert_eq!(media_command("volume", 140), Some((A::Volume, Some(100))));
        assert_eq!(media_command("volume", -3), Some((A::Volume, Some(0))));
        assert_eq!(media_command("seek", 5), None);
        assert_eq!(media_command("bogus", 0), None);
    }

    #[test]
    fn capabilities_decide_whether_a_peer_could_be_controlled() {
        assert!(!can_be_controlled(&[]));
        assert!(!can_be_controlled(&["ping".into(), "media.control".into()]));
        assert!(can_be_controlled(&["ping".into(), "input.pointer".into()]));
        assert!(can_be_controlled(&["input.key".into()]));

        let mut s = Store::default();
        let mut p = peer("aa", true);
        p.capabilities = vec!["input.key".into(), "transfer".into()];
        s.set_peers(vec![p], 1000);
        assert!(device_row(&s.peers[0], 1000).can_be_controlled);
        // Offline the peer advertises nothing.
        s.apply(
            Event::Host(HostEvent::PeerOffline {
                peer: "aa".into(),
                short_id: "K3QD".into(),
            }),
            1001,
        );
        assert!(!device_row(&s.peers[0], 1001).can_be_controlled);
    }

    #[test]
    fn bytes_rate_and_eta() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(12 * 1024 * 1024), "12.0 MiB");
        assert_eq!(format_bytes(5 * 1024 * 1024 * 1024), "5.0 GiB");
        assert_eq!(format_rate(30 * 1024 * 1024), "30.0 MiB/s");
        assert_eq!(format_eta(100, 0), None);
        assert_eq!(format_eta(0, 10).as_deref(), Some("less than 1 s"));
        assert_eq!(format_eta(101, 10).as_deref(), Some("11 s"));
        assert_eq!(format_eta(125, 1).as_deref(), Some("2 min 5 s"));
        assert_eq!(format_eta(120, 1).as_deref(), Some("2 min"));
        assert_eq!(format_eta(3780, 1).as_deref(), Some("1 h 3 min"));
    }

    #[test]
    fn dates_and_ages() {
        assert_eq!(format_date(0), "1970-01-01");
        assert_eq!(format_date(951_782_400), "2000-02-29");
        assert_eq!(format_date(1_791_417_600), "2026-10-08");
        assert_eq!(format_ago(100, 130), "just now");
        assert_eq!(format_ago(0, 300), "5 min ago");
        assert_eq!(format_ago(0, 7200), "2 h ago");
        assert_eq!(format_ago(0, 90_000), "1 day ago");
        assert_eq!(format_ago(0, 3 * 86_400), "3 days ago");
        assert_eq!(format_ago(500, 100), "just now");
    }

    #[test]
    fn sas_and_addresses() {
        assert_eq!(format_sas("123456"), "123 456");
        assert_eq!(format_sas("12345"), "12345");
        assert_eq!(
            parse_pair_addr(" 192.168.1.20 "),
            Some("192.168.1.20:42187".parse().unwrap())
        );
        assert_eq!(
            parse_pair_addr("10.0.0.2:5000"),
            Some("10.0.0.2:5000".parse().unwrap())
        );
        assert_eq!(
            parse_pair_addr("[::1]"),
            Some("[::1]:42187".parse().unwrap())
        );
        assert_eq!(parse_pair_addr("phone.local"), None);
    }

    #[test]
    fn peer_events_update_rows() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", false)], 1000);
        let row = device_row(&s.peers[0], 1000);
        assert_eq!(row.state, "Offline");
        assert_eq!(row.last_seen, "never");
        assert_eq!(row.paired, "2026-10-08");
        assert!(row.grant_files && !row.grant_input);

        let ch = s.apply(
            Event::Host(HostEvent::PeerOnline {
                peer: "aa".into(),
                short_id: "K3QD".into(),
                name: "Phone".into(),
                addr: "10.0.0.2:42187".parse().unwrap(),
            }),
            1010,
        );
        assert_eq!(ch, vec![Change::Peer(0)]);
        s.apply(
            Event::Host(HostEvent::Battery {
                peer: "aa".into(),
                level: 81,
                charging: true,
                low: false,
            }),
            1011,
        );
        s.apply(
            Event::Host(HostEvent::Presence {
                peer: "aa".into(),
                state: "asleep".into(),
            }),
            1012,
        );
        let row = device_row(&s.peers[0], 1012);
        assert_eq!(row.state, "Asleep");
        assert_eq!(row.battery, "81%, charging");
        assert_eq!(row.last_seen, "now");

        s.apply(
            Event::Host(HostEvent::PeerOffline {
                peer: "aa".into(),
                short_id: "K3QD".into(),
            }),
            1100,
        );
        // It said it was going to sleep before it dropped: asleep, not gone.
        let row = device_row(&s.peers[0], 1400);
        assert_eq!(row.state, "Asleep");
        assert_eq!(row.last_seen, "5 min ago");

        // A reload keeps what only events told us.
        s.set_peers(vec![peer("aa", false)], 1500);
        assert_eq!(s.peers[0].battery, Some((81, true)));
        assert_eq!(s.peers[0].last_seen, Some(1100));
        assert_eq!(device_row(&s.peers[0], 1500).state, "Asleep");

        // The daemon's persisted time wins over "never" and is shown for offline devices.
        let mut known = peer("cc", false);
        known.last_seen = Some(1200);
        s.set_peers(vec![peer("aa", false), known], 1500);
        assert_eq!(device_row(&s.peers[1], 1500).last_seen, "5 min ago");
        // A reload with an older daemon value never moves the time back.
        let mut stale = peer("aa", false);
        stale.last_seen = Some(900);
        s.set_peers(vec![stale], 1500);
        assert_eq!(s.peers[0].last_seen, Some(1100));

        // Back online clears it; a plain drop then reads "Offline".
        let online = HostEvent::PeerOnline {
            peer: "aa".into(),
            short_id: "K3QD".into(),
            name: "Phone".into(),
            addr: "10.0.0.2:42187".parse().unwrap(),
        };
        s.apply(Event::Host(online), 1501);
        assert_eq!(device_row(&s.peers[0], 1501).state, "Online");
        s.apply(
            Event::Host(HostEvent::PeerOffline {
                peer: "aa".into(),
                short_id: "K3QD".into(),
            }),
            1502,
        );
        assert_eq!(device_row(&s.peers[0], 1502).state, "Offline");

        // Unknown peer asks for a reload; unpair removes the row.
        let ch = s.apply(
            Event::Host(HostEvent::Battery {
                peer: "bb".into(),
                level: 1,
                charging: false,
                low: true,
            }),
            1500,
        );
        assert_eq!(ch, vec![Change::Refetch(Refetch::Peers)]);
        let ch = s.apply(
            Event::Host(HostEvent::Unpaired {
                peer: "aa".into(),
                short_id: "K3QD".into(),
            }),
            1500,
        );
        assert_eq!(ch, vec![Change::Peers]);
        assert!(s.peers.is_empty());
    }

    #[test]
    fn phase3_events_decode_and_unrendered_ones_change_nothing() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true)], 1000);
        // Exactly what the daemon puts on the bus: the image arrives as base64.
        let image: Event = serde_json::from_value(serde_json::json!({
            "source": "host", "event": "clipboard_image", "peer": "aa",
            "mime": "image/png", "data": "iVBORw0K", "sensitive": false
        }))
        .unwrap();
        let ch = s.apply(image, 1001);
        assert!(matches!(&ch[..], [Change::Notice(t, false)] if t.contains("image")));

        let outbox: Event = serde_json::from_value(serde_json::json!({
            "source": "host", "event": "outbox_ready", "peer": "aa",
            "count": 2, "bytes": 10, "ids": ["o1", "o2"]
        }))
        .unwrap();
        assert!(s.apply(outbox, 1002).is_empty());
        let command: Event = serde_json::from_value(serde_json::json!({
            "source": "host", "event": "media_command", "peer": "aa",
            "player": "spotify", "action": "playPause", "value": null
        }))
        .unwrap();
        assert!(s.apply(command, 1003).is_empty());
    }

    #[test]
    fn pairing_prompt_and_outcome() {
        let mut s = Store::default();
        let ch = s.apply(
            Event::Host(HostEvent::PairingPrompt {
                peer: "aa".into(),
                short_id: "K3QD".into(),
                name: "Phone".into(),
                prompt: PairingPromptKind::Sas {
                    sas: Secret("123456".into()),
                },
            }),
            0,
        );
        assert_eq!(ch, vec![Change::Prompt]);
        assert_eq!(s.prompt.as_ref().unwrap().sas.as_deref(), Some("123456"));
        let ch = s.apply(
            Event::Host(HostEvent::PairingFailed {
                reason: "rejected".into(),
            }),
            0,
        );
        assert!(s.prompt.is_none());
        assert!(ch.contains(&Change::PairingStatus(
            "Pairing failed: rejected".into(),
            true
        )));

        let seen = HostEvent::AddDeviceSeen {
            name: "Pixel".into(),
            device_type: "phone".into(),
            addr: "10.0.0.2:42187".parse().unwrap(),
        };
        assert_eq!(
            s.apply(Event::Host(seen.clone()), 0),
            vec![Change::Candidates]
        );
        assert!(s.apply(Event::Host(seen), 0).is_empty());
        assert_eq!(candidate_row(&s.candidates[0]).device_type, "Phone");

        // The same device on a link-local IPv6 address is the same row; a second
        // private IPv4 address does not replace the first.
        let v6 = HostEvent::AddDeviceSeen {
            name: "Pixel".into(),
            device_type: "phone".into(),
            addr: "[fe80::1]:42187".parse().unwrap(),
        };
        assert!(s.apply(Event::Host(v6), 0).is_empty());
        let other_v4 = HostEvent::AddDeviceSeen {
            name: "Pixel".into(),
            device_type: "phone".into(),
            addr: "172.17.0.1:42187".parse().unwrap(),
        };
        assert!(s.apply(Event::Host(other_v4), 0).is_empty());
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0].addr.to_string(), "10.0.0.2:42187");

        let ch = s.apply(
            Event::Host(HostEvent::Paired {
                peer: "aa".into(),
                short_id: "K3QD".into(),
                name: "Pixel".into(),
            }),
            0,
        );
        assert!(ch.contains(&Change::Refetch(Refetch::Peers)));
        assert!(ch.contains(&Change::WindowClosed));
        assert!(s.candidates.is_empty());
    }

    #[test]
    fn transfer_lifecycle_rows() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true)], 0);
        let ch = s.apply(
            Event::Host(HostEvent::TransferOffer {
                id: "t1".into(),
                peer: "aa".into(),
                short_id: "K3QD".into(),
                files: (0..5)
                    .map(|i| OfferedFile {
                        path: format!("f{i}.jpg"),
                        size: 10,
                    })
                    .collect(),
                file_count: 5,
                total_bytes: 2048,
                note: None,
            }),
            0,
        );
        assert_eq!(ch[0], Change::Transfers);
        assert!(matches!(ch[1], Change::Notice(..)));
        let row = transfer_row(&s.transfers[0], &s.peer_name("aa"));
        assert_eq!(row.title, "From Phone");
        assert_eq!(
            row.files,
            "5 files, 2.0 KiB: f0.jpg, f1.jpg, f2.jpg and 2 more"
        );
        assert!(row.can_respond && !row.can_cancel && !row.show_progress);

        let ch = s.apply(
            Event::Host(HostEvent::TransferProgress {
                id: "t1".into(),
                peer: "aa".into(),
                direction: TransferDirection::Receive,
                bytes: 1024,
                total: 2048,
                bytes_per_sec: 512,
                files_done: 0,
            }),
            0,
        );
        assert_eq!(ch, vec![Change::Transfer(0)]);
        let row = transfer_row(&s.transfers[0], "Phone");
        assert_eq!(row.status_text, "50%");
        assert!((row.progress - 0.5).abs() < f32::EPSILON);
        assert_eq!(row.detail, "1.0 KiB of 2.0 KiB · 512 B/s · 2 s left");
        assert!(row.can_cancel && !row.can_respond);

        s.apply(
            Event::Host(HostEvent::TransferComplete {
                id: "t1".into(),
                peer: "aa".into(),
                direction: TransferDirection::Receive,
                bytes: 2048,
                elapsed_ms: 4000,
                roots: Vec::new(),
                bundle_root: None,
                saved: vec![PathBuf::from("/d/f0.jpg"), PathBuf::from("/d/f1.jpg")],
                folder: Some(PathBuf::from("/d")),
                delivered: Vec::new(),
            }),
            0,
        );
        let row = transfer_row(&s.transfers[0], "Phone");
        assert_eq!(row.state, "complete");
        assert_eq!(row.detail, "Took 4 s");
        assert!(row.can_show && !row.can_open && !row.can_cancel);

        // A send started elsewhere shows up from its first progress event.
        let ch = s.apply(
            Event::Host(HostEvent::TransferFailed {
                id: "t2".into(),
                peer: "aa".into(),
                direction: TransferDirection::Send,
                reason: "peer went away".into(),
            }),
            0,
        );
        assert_eq!(ch, vec![Change::Transfers]);
        let row = transfer_row(&s.transfers[1], "Phone");
        assert_eq!(row.title, "To Phone");
        assert_eq!(row.detail, "peer went away");
        assert_eq!(row.state, "failed");

        s.clear_finished();
        assert!(s.transfers.is_empty());
    }

    #[test]
    fn active_row_counts_files_only_for_a_bundle_that_has_started() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true)], 0);
        let progress = |files_done| {
            Event::Host(HostEvent::TransferProgress {
                id: "t1".into(),
                peer: "aa".into(),
                direction: TransferDirection::Send,
                bytes: 1024,
                total: 2048,
                bytes_per_sec: 0,
                files_done,
            })
        };
        let tv = |s: &mut Store, files_done, file_count| {
            s.apply(progress(files_done), 0);
            s.transfers[0].file_count = file_count;
            transfer_row(&s.transfers[0], "Phone").detail.to_string()
        };
        // Nothing finished yet, or an older daemon that sends no count.
        assert_eq!(tv(&mut s, 0, 40), "1.0 KiB of 2.0 KiB");
        assert_eq!(tv(&mut s, 12, 40), "1.0 KiB of 2.0 KiB · 12 of 40 files");
        // One file has nothing to count.
        assert_eq!(tv(&mut s, 1, 1), "1.0 KiB of 2.0 KiB");
    }

    #[test]
    fn drops_go_to_the_selected_device_only() {
        let mut s = Store::default();
        s.set_peers(vec![peer("aa", true), peer("bb", false)], 0);
        assert_eq!(drop_target(1, &s.peers).as_deref(), Some("bb"));
        assert_eq!(drop_target(-1, &s.peers), None);
        assert_eq!(drop_target(2, &s.peers), None);
    }

    #[test]
    fn late_window_can_open_finished_receive() {
        let snap: TransferSnapshot = serde_json::from_value(serde_json::json!({
            "id": "t9", "peer": "aa", "short_id": "K3QD", "direction": "receive",
            "state": "complete", "bytes": 10, "total": 10, "bytes_per_sec": 0,
            "started_at": 1, "file_count": 1, "note": null,
            "saved": ["/d/report.pdf"], "folder": "/d"
        }))
        .unwrap();
        let mut s = Store::default();
        s.set_transfers(vec![snap.clone()]);
        assert_eq!(s.transfers[0].saved, vec![PathBuf::from("/d/report.pdf")]);
        let row = transfer_row(&s.transfers[0], "Phone");
        assert!(row.can_open && row.can_show);
        // A reload from a daemon that sent no paths keeps the ones already known.
        let mut bare = snap;
        bare.saved.clear();
        s.set_transfers(vec![bare]);
        assert_eq!(s.transfers[0].saved.len(), 1);
    }

    #[test]
    fn autostart_refusal_comes_from_the_daemon_field() {
        let mut s = Store::default();
        s.set_status(Status {
            device_id: "aa".into(),
            short_id: "K3QD".into(),
            addr: "0.0.0.0:42187".parse().unwrap(),
            daemon_version: "0".into(),
            ipc_version: 1,
            settings: settings(true, None),
            add_device_window_open: false,
            locked: false,
            input_from: Vec::new(),
        });
        let changed = |on: bool, refused: Option<&str>| {
            Event::Daemon(DaemonEvent::SettingsChanged {
                settings: Settings {
                    autostart: on,
                    autostart_refused: refused.map(str::to_owned),
                    ..settings(true, None)
                },
            })
        };
        // Off without a reason (the user's own choice, however long after): no notice.
        assert_eq!(s.apply(changed(false, None), 1), vec![Change::Settings]);
        // The daemon reports a refusal: announced once and kept for the settings screen.
        assert_eq!(
            s.apply(changed(false, Some("not allowed")), 2),
            vec![Change::Settings, Change::Notice("not allowed".into(), true)]
        );
        assert_eq!(
            s.status
                .as_ref()
                .unwrap()
                .settings
                .autostart_refused
                .as_deref(),
            Some("not allowed")
        );
        // The same reason again (another setting changed): no second notice.
        assert_eq!(
            s.apply(changed(false, Some("not allowed")), 3),
            vec![Change::Settings]
        );
        // Turned on again successfully: the reason is gone.
        assert_eq!(s.apply(changed(true, None), 4), vec![Change::Settings]);
        assert_eq!(s.status.as_ref().unwrap().settings.autostart_refused, None);
    }

    #[test]
    fn daemon_events() {
        let mut s = Store::default();
        assert_eq!(
            s.apply(Event::Daemon(DaemonEvent::HostRestarted), 0),
            vec![Change::Refetch(Refetch::All)]
        );
        assert_eq!(
            s.apply(Event::Daemon(DaemonEvent::LockChanged { locked: true }), 0),
            vec![Change::Lock]
        );
        assert_eq!(
            clipboard_sent_text(0),
            "Clipboard not sent: no connected device has clipboard sharing turned on"
        );
        assert_eq!(
            clipboard_skipped_text(ClipSkip::Unsupported),
            "Clipboard not sent: the connected devices cannot receive this kind of clip"
        );
        assert_eq!(clipboard_sent_text(2), "Clipboard sent to 2 devices");
    }

    #[test]
    fn diagnose_summary_picks_the_help_panel_focus() {
        let lines = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert!(!network_failed(&lines(&["all paths fine"])));
        assert!(!network_failed(&lines(&[
            "all paths fine",
            "note: port 1716 is in use by KDE Connect or GSConnect; that is fine",
            "start at login was not allowed: the request was denied",
        ])));
        assert!(network_failed(&lines(&["mDNS failed: no route"])));

        let fw = lines(&[
            "multicast blocked on 10.0.0.5: No such device",
            "port 42187 blocked by the host firewall (ufw): run `sudo ufw allow 42187`",
        ]);
        assert_eq!(
            help_focus(&fw),
            (HelpFocus::Firewall, "sudo ufw allow 42187".to_owned())
        );
        let net = lines(&[
            "multicast blocked on 10.0.0.5: No such device",
            "no interface on the same network as Phone",
        ]);
        assert_eq!(help_focus(&net), (HelpFocus::HotspotOrUsb, String::new()));
        let mc = lines(&["multicast blocked on 192.168.1.10: no beacon heard although Laptop is connected on this network"]);
        assert_eq!(help_focus(&mc), (HelpFocus::QrOrAddress, String::new()));
        // Ambiguous findings keep the usual order.
        let other = lines(&["Phone not reached: mDNS failed (connection refused)"]);
        assert_eq!(help_focus(&other), (HelpFocus::None, String::new()));
        assert_eq!(
            help_focus(&lines(&["all paths fine"])),
            (HelpFocus::None, String::new())
        );
    }

    #[test]
    fn diagnostics_sections() {
        use playdrop_core::host::{DialFailure, InterfaceInfo};
        use playdrop_ipc::AddrProbe;
        let report = DiagnosticsReport {
            core: Diagnostics {
                addr: "0.0.0.0:42187".parse().unwrap(),
                device_id: "aa".into(),
                short_id: "K3QD".into(),
                port: 42187,
                bind_ok: true,
                interfaces: vec![InterfaceInfo {
                    index: 2,
                    addr: "192.168.1.5".parse().unwrap(),
                    netmask: "255.255.255.0".parse().unwrap(),
                }],
                multicast: MulticastStatus {
                    enabled: true,
                    joined: vec!["192.168.1.5".parse().unwrap()],
                    join_failed: Vec::new(),
                    beacon_recently: false,
                    last_beacon_secs_ago: None,
                },
                mdns: MdnsStatus {
                    enabled: true,
                    registered: true,
                    error: None,
                    peer_seen: true,
                    last_seen_secs_ago: Some(5),
                },
                peers: vec![PeerDiagnostic {
                    peer: "bb".into(),
                    short_id: "ZZ".into(),
                    label: "Phone".into(),
                    online: false,
                    last_known: Vec::new(),
                    mdns_seen: true,
                    beacon_seen: false,
                    last_result: Some(DialFailure {
                        path: DiscoveryPath::Mdns,
                        failure: "connection refused".into(),
                        at: 0,
                    }),
                }],
            },
            port: PortProbe {
                port: 42187,
                loopback_ok: true,
                lan: vec![AddrProbe {
                    addr: "192.168.1.5:42187".parse().unwrap(),
                    ok: false,
                    error: Some("timed out".into()),
                }],
                firewall: Some(FirewallHint {
                    tool: "ufw".into(),
                    active: true,
                    port_allowed: Some(false),
                    fix: "sudo ufw allow 42187".into(),
                }),
            },
            summary: vec!["port 42187 blocked by the host firewall (ufw)".into()],
        };
        let (summary, sections) = diagnostics_view(&report);
        assert_eq!(summary.len(), 1);
        let titles: Vec<_> = sections.iter().map(|s| s.title.to_string()).collect();
        assert_eq!(
            titles,
            [
                "Network interfaces",
                "Multicast beacon",
                "mDNS",
                "Paired devices",
                "Port 42187",
                "Firewall"
            ]
        );
        assert!(sections[0].ok && !sections[1].ok && sections[2].ok);
        assert!(!sections[3].ok && !sections[4].ok && !sections[5].ok);
        let peer_lines: Vec<_> = sections[3].lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(
            peer_lines[1],
            "    Last failure via mDNS: connection refused"
        );
        let fw: Vec<_> = sections[5].lines.iter().map(|l| l.to_string()).collect();
        assert_eq!(fw.last().unwrap(), "Fix: sudo ufw allow 42187");
        assert_eq!(
            sections[4].lines.row_data(1).unwrap(),
            "192.168.1.5:42187: blocked (timed out)"
        );
    }
}
