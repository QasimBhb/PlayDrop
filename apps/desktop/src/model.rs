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
    AddDeviceCandidate, Diagnostics, DiscoveryPath, HostEvent, MdnsStatus, MulticastStatus,
    PairingPromptKind, PeerDiagnostic, PeerSummary, TransferDirection, TransferSnapshot,
    TransferStatus,
};
use playdrop_core::pin_store::Grants;
use playdrop_ipc::{
    DaemonEvent, DiagnosticsReport, Event, FirewallHint, PortProbe, Settings, Status,
};
use slint::{ModelRc, SharedString, VecModel};

use crate::{CandidateRow, DeviceRow, DiagSection, TransferRow};

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
    /// Last `presence` state reported while online.
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
}

impl Store {
    /// Replaces the status.
    pub fn set_status(&mut self, status: Status) {
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
                    v.note.clone_from(&o.note);
                    if v.online {
                        v.presence.clone_from(&o.presence);
                    } else {
                        v.last_seen = v.last_seen.max(o.last_seen);
                    }
                }
                v
            })
            .collect();
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
                self.set_settings(settings);
                vec![Change::Settings]
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
                vec![Change::Lock]
            }
            DaemonEvent::HostRestarted => vec![Change::Refetch(Refetch::All)],
            DaemonEvent::ShuttingDown => vec![Change::ShuttingDown],
            _ => Vec::new(),
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
            HostEvent::PeerOffline { peer, .. } => self.with_peer(&peer, |p| {
                p.online = false;
                p.presence = None;
                p.last_seen = Some(now);
            }),
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
            } => self.upsert_transfer(&id, &peer, direction, |t| {
                t.state = TransferStatus::Active;
                t.bytes = bytes;
                t.total = total;
                t.bytes_per_sec = bytes_per_sec;
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

/// "Clipboard sent to 2 devices" or the reason nothing was sent.
pub fn clipboard_sent_text(n: usize) -> String {
    match n {
        0 => "Clipboard not sent: no online device accepts it".to_owned(),
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

fn state_text(p: &PeerView) -> &'static str {
    if !p.online {
        return "Offline";
    }
    match p.presence.as_deref() {
        Some("asleep") => "Asleep",
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
    }
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
#[allow(clippy::assert_is_empty)] // `is_empty` reads clearer than comparing to `vec![]`.
mod tests {
    use super::*;
    use playdrop_core::host::{OfferedFile, Secret};
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
        }
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
        let row = device_row(&s.peers[0], 1400);
        assert_eq!(row.state, "Offline");
        assert_eq!(row.last_seen, "5 min ago");

        // A reload keeps what only events told us.
        s.set_peers(vec![peer("aa", false)], 1500);
        assert_eq!(s.peers[0].battery, Some((81, true)));
        assert_eq!(s.peers[0].last_seen, Some(1100));

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
            "Clipboard not sent: no online device accepts it"
        );
        assert_eq!(clipboard_sent_text(2), "Clipboard sent to 2 devices");
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
