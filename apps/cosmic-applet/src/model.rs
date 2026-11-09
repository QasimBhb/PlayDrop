//! The applet's state and what the popup shows, with no widget code.
//!
//! Daemon answers and events go in through [`Model::apply`] (and a few helpers for the
//! applet's own actions); [`Model::view`] turns the state into a [`View`] of plain strings and
//! flags that `app.rs` lays out. Keeping this free of libcosmic lets it be unit tested without
//! a Wayland session.
//!
//! Peer-supplied text (labels, file names) is shown here but never logged (SR-32).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Write as _;

use playdrop_core::host::TransferStatus;
use playdrop_core::host::{HostEvent, PeerSummary, TransferDirection, TransferSnapshot};
use playdrop_ipc::{ClipSkip, DaemonEvent, Event, Status};

/// Most finished transfers the popup lists.
pub const SHOW_FINISHED: usize = 4;
/// Most finished transfers the model keeps; older ones are dropped.
const KEEP_FINISHED: usize = 16;
/// Finished transfers older than this many seconds are dropped, like the daemon's own book
/// (10 minutes).
const FINISHED_FOR_SECS: u64 = 600;
/// How many pruned transfer keys are remembered, so a list refresh (the daemon keeps 64
/// finished transfers for 10 minutes) does not bring them back as if they just changed.
const DROPPED_MEMORY: usize = 256;

/// Transfers are keyed by ID and direction, like the daemon's book: a loopback test can
/// send and receive under the same ID.
type TransferKey = (String, TransferDirection);

/// Where the connection to the daemon stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonState {
    /// First attempt not answered yet.
    Connecting,
    /// Connected and verified.
    Running {
        /// The daemon's package version.
        version: String,
    },
    /// The user asked for it, or the daemon answered that its host is still coming up.
    Starting,
    /// The daemon owns its name but has said "starting" for too long: its host did not come
    /// up.
    NotReady,
    /// Nobody owns the D-Bus name.
    NotRunning,
    /// Connecting failed for another reason (unverified daemon, version mismatch, bus error).
    Failed(String),
}

/// Why a connect attempt failed, already sorted for the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectProblem {
    /// No daemon (`ConnectError::NotRunning`).
    NotRunning,
    /// The daemon is coming up: it answered "starting; try again", or the applet just ran it.
    Starting,
    /// It has answered "starting" for too long.
    NotReady,
    /// Anything else, worded for the user.
    Other(String),
}

impl ConnectProblem {
    /// Plain words for a notice.
    pub fn text(&self) -> String {
        match self {
            Self::NotRunning => "PlayDrop is not running".to_owned(),
            Self::Starting => "PlayDrop is still starting".to_owned(),
            Self::NotReady => "PlayDrop is running but not ready".to_owned(),
            Self::Other(m) => m.clone(),
        }
    }
}

/// One paired device.
// `device_id` is the IPC's own name for the field; renaming it would only obscure that.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// Hex Device ID: used to address requests, never shown or logged in full (SR-32).
    pub device_id: String,
    /// Short ID, shown when two labels collide (SR-8).
    pub short_id: String,
    /// Local label.
    pub label: String,
    /// Session up.
    pub online: bool,
    /// Last battery report while this applet ran: percent and charging.
    pub battery: Option<(u8, bool)>,
    /// Unix seconds of the last frame from the device, when known.
    pub last_seen: Option<u64>,
    /// Our `clipboardOut` grant for this device.
    pub clipboard_out: bool,
    /// What the device accepts while online; empty from an older daemon or offline.
    pub capabilities: Vec<String>,
    /// The device forgot this computer and must be paired again.
    pub removed_by_peer: bool,
}

impl Device {
    fn from_summary(p: &PeerSummary, battery: Option<(u8, bool)>) -> Self {
        Self {
            device_id: p.device_id.clone(),
            short_id: p.short_id.clone(),
            label: p.label.clone(),
            online: p.online,
            battery,
            last_seen: p.last_seen,
            clipboard_out: p.grants.clipboard_out,
            capabilities: p.capabilities.clone(),
            removed_by_peer: p.removed_by_peer,
        }
    }

    /// Whether the device lists `cap`. An online device with an empty list comes from an
    /// older daemon that did not report capabilities: assume yes rather than hide a working
    /// control.
    fn accepts(&self, cap: &str) -> bool {
        self.capabilities.is_empty() || self.capabilities.iter().any(|c| c == cap)
    }
}

/// One transfer, either direction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transfer {
    /// Transfer ID.
    pub id: String,
    /// Hex Device ID of the other side.
    pub peer: String,
    /// Short ID of the other side.
    pub short_id: String,
    /// Our side.
    pub direction: TransferDirection,
    /// Where it stands.
    pub state: TransferStatus,
    /// Bytes moved or verified.
    pub bytes: u64,
    /// Total bytes.
    pub total: u64,
    /// Number of files, 0 when not known yet.
    pub file_count: usize,
    /// Display name when one file is involved and its name is known.
    pub name: Option<String>,
    /// Failure or cancel reason.
    pub reason: Option<String>,
    /// Unix seconds the transfer started or was first seen.
    pub started_at: u64,
    /// Unix seconds of the last change, for ordering and expiry.
    pub updated_at: u64,
}

impl Transfer {
    fn from_snapshot(s: &TransferSnapshot, now: u64) -> Self {
        let finished = s.state.is_finished();
        Self {
            id: s.id.clone(),
            peer: s.peer.clone(),
            short_id: s.short_id.clone(),
            direction: s.direction,
            state: s.state,
            bytes: s.bytes,
            total: s.total,
            file_count: s.file_count,
            name: single_name(s.file_count, s.saved.first()),
            reason: if finished && s.state != TransferStatus::Complete {
                s.note.clone()
            } else {
                None
            },
            started_at: s.started_at,
            // A snapshot has no end time: count a finished one from when we first saw it
            // (the daemon itself keeps them at most 10 minutes).
            updated_at: now,
        }
    }
}

/// The file name of `path` when exactly one file is involved.
fn single_name(file_count: usize, path: Option<&std::path::PathBuf>) -> Option<String> {
    if file_count != 1 {
        return None;
    }
    path.and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
}

/// A stable order for directions (the core type has no `Ord`).
fn dir_rank(d: TransferDirection) -> u8 {
    match d {
        TransferDirection::Send => 0,
        TransferDirection::Receive => 1,
    }
}

/// The last name component of a manifest path (`a/b/c.txt` is `c.txt`).
fn manifest_basename(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_owned()
}

/// What the reducer asks the caller to fetch, because an event changed something it does
/// not carry (grants, capabilities, file counts).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Refresh {
    /// Ask for `peers` again.
    pub peers: bool,
    /// Ask for `transfers` again.
    pub transfers: bool,
    /// Ask for `status` again.
    pub status: bool,
}

impl Refresh {
    /// Nothing to fetch.
    pub const NONE: Self = Self {
        peers: false,
        transfers: false,
        status: false,
    };
    /// The device list.
    pub const PEERS: Self = Self {
        peers: true,
        transfers: false,
        status: false,
    };
    /// The transfer list.
    pub const TRANSFERS: Self = Self {
        peers: false,
        transfers: true,
        status: false,
    };
    /// Everything (after a lag or a host restart).
    pub const ALL: Self = Self {
        peers: true,
        transfers: true,
        status: true,
    };
}

/// One remote input session on this desktop (SR-25).
#[derive(Debug, Clone, PartialEq, Eq)]
struct InputSession {
    /// Hex Device ID when an event named it; `None` for entries from `Status.input_from`,
    /// which carries labels only.
    peer: Option<String>,
    /// The controlling peer's label.
    label: String,
}

/// The applet's whole state.
#[derive(Debug, Clone)]
pub struct Model {
    daemon: DaemonState,
    devices: HashMap<String, Device>,
    transfers: HashMap<TransferKey, Transfer>,
    /// Finished transfers pruned here, oldest first, at most [`DROPPED_MEMORY`].
    dropped: VecDeque<TransferKey>,
    /// Names the applet knows for transfers it started itself, by transfer ID.
    sent_names: HashMap<String, String>,
    inputs: Vec<InputSession>,
    locked: bool,
    notice: Option<String>,
    /// Device IDs with a clipboard send or file pick in flight, so their buttons show it.
    busy: HashSet<(String, Busy)>,
    /// The user clicked "Start" and the connection loop has not answered yet: an automatic
    /// attempt that reports "not running" meanwhile must not bring the button back.
    start_in_flight: bool,
    /// Device waiting for the user to confirm sending a password-manager clip (SR-24).
    confirm_sensitive: Option<String>,
}

/// An action in flight for one device. A device can have both at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Busy {
    /// Reading and sending the clipboard.
    Clipboard,
    /// The file chooser is open or the send request is in flight.
    Files,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// Inputs to [`Model::apply`].
#[derive(Debug, Clone)]
pub enum Input {
    /// Connected; the first full picture.
    Connected {
        /// `status` answer.
        status: Box<Status>,
        /// `peers` answer.
        peers: Vec<PeerSummary>,
        /// `transfers` answer.
        transfers: Vec<TransferSnapshot>,
    },
    /// A connect attempt failed.
    ConnectFailed(ConnectProblem),
    /// The daemon went away (event stream ended).
    Disconnected,
    /// A fresh `peers` answer.
    Peers(Vec<PeerSummary>),
    /// A fresh `transfers` answer.
    Transfers(Vec<TransferSnapshot>),
    /// A fresh `status` answer (after a lag).
    Status(Box<Status>),
    /// A daemon event.
    Event(Box<Event>),
    /// The user clicked "Start `PlayDrop`".
    StartRequested,
    /// The start did not bring the daemon up, with the reason.
    StartFailed(String),
}

impl Model {
    /// An empty model, connecting.
    pub fn new() -> Self {
        Self {
            daemon: DaemonState::Connecting,
            devices: HashMap::new(),
            transfers: HashMap::new(),
            dropped: VecDeque::new(),
            sent_names: HashMap::new(),
            inputs: Vec::new(),
            locked: false,
            notice: None,
            busy: HashSet::new(),
            start_in_flight: false,
            confirm_sensitive: None,
        }
    }

    /// Where the connection stands.
    #[cfg(test)]
    pub fn daemon(&self) -> &DaemonState {
        &self.daemon
    }

    /// Whether a verified connection is up.
    pub fn is_running(&self) -> bool {
        matches!(self.daemon, DaemonState::Running { .. })
    }

    /// Applies one input at Unix time `now` and says what to fetch next.
    pub fn apply(&mut self, input: Input, now: u64) -> Refresh {
        match input {
            Input::Connected {
                status,
                peers,
                transfers,
            } => {
                self.daemon = DaemonState::Running {
                    version: status.daemon_version.clone(),
                };
                // "Starting PlayDrop..." and older outcomes are moot now.
                self.notice = None;
                self.start_in_flight = false;
                self.apply_status(&status);
                self.replace_peers(&peers);
                self.replace_transfers(&transfers, now);
                Refresh::NONE
            }
            Input::ConnectFailed(problem) => {
                self.go_offline();
                // Mid-start, only `StartFailed` or `Connected` ends the "starting" state.
                if !self.start_in_flight {
                    self.daemon = match problem {
                        ConnectProblem::NotRunning => DaemonState::NotRunning,
                        ConnectProblem::Starting => DaemonState::Starting,
                        ConnectProblem::NotReady => DaemonState::NotReady,
                        ConnectProblem::Other(m) => DaemonState::Failed(m),
                    };
                }
                Refresh::NONE
            }
            Input::StartRequested => {
                if !self.is_running() {
                    self.daemon = DaemonState::Starting;
                    self.notice = None;
                    self.start_in_flight = true;
                }
                Refresh::NONE
            }
            Input::StartFailed(reason) => {
                self.start_in_flight = false;
                if !self.is_running() {
                    self.daemon = DaemonState::NotRunning;
                    self.notice = Some(format!("PlayDrop did not start: {reason}."));
                }
                Refresh::NONE
            }
            Input::Disconnected => {
                self.go_offline();
                self.daemon = DaemonState::NotRunning;
                Refresh::NONE
            }
            Input::Peers(peers) => {
                if self.is_running() {
                    self.replace_peers(&peers);
                }
                Refresh::NONE
            }
            Input::Transfers(list) => {
                if self.is_running() {
                    self.replace_transfers(&list, now);
                }
                Refresh::NONE
            }
            Input::Status(status) => {
                if self.is_running() {
                    self.apply_status(&status);
                }
                Refresh::NONE
            }
            Input::Event(event) => {
                if !self.is_running() {
                    return Refresh::NONE;
                }
                let refresh = match *event {
                    Event::Host(e) => self.apply_host(e, now),
                    Event::Daemon(e) => self.apply_daemon(e),
                    // `Event` is non-exhaustive: a newer source is ignored.
                    #[allow(unreachable_patterns)]
                    _ => Refresh::NONE,
                };
                self.prune(now);
                refresh
            }
        }
    }

    /// The daemon is gone: nothing it reported is current any more.
    fn go_offline(&mut self) {
        self.devices.clear();
        self.transfers.clear();
        self.sent_names.clear();
        self.inputs.clear();
        self.busy.clear();
        self.confirm_sensitive = None;
        self.locked = false;
    }

    fn apply_status(&mut self, status: &Status) {
        self.locked = status.locked;
        // Keep entries an event named (they carry the peer ID); rebuild the rest from the
        // labels the status lists.
        let mut inputs: Vec<InputSession> = self
            .inputs
            .iter()
            .filter(|s| s.peer.is_some() && status.input_from.contains(&s.label))
            .cloned()
            .collect();
        for label in &status.input_from {
            if !inputs.iter().any(|s| &s.label == label) {
                inputs.push(InputSession {
                    peer: None,
                    label: label.clone(),
                });
            }
        }
        self.inputs = inputs;
    }

    fn replace_peers(&mut self, peers: &[PeerSummary]) {
        let old = std::mem::take(&mut self.devices);
        for p in peers {
            // Battery only arrives as events; keep what we heard while the device stays
            // online.
            let battery = old
                .get(&p.device_id)
                .filter(|_| p.online)
                .and_then(|d| d.battery);
            self.devices
                .insert(p.device_id.clone(), Device::from_summary(p, battery));
        }
        self.busy.retain(|(id, _)| self.devices.contains_key(id));
        if let Some(d) = &self.confirm_sensitive {
            if !self
                .devices
                .get(d)
                .is_some_and(|d| d.online && !d.removed_by_peer)
            {
                self.confirm_sensitive = None;
            }
        }
    }

    fn replace_transfers(&mut self, list: &[TransferSnapshot], now: u64) {
        let mut old = std::mem::take(&mut self.transfers);
        for s in list {
            let key = (s.id.clone(), s.direction);
            let prev = old.remove(&key);
            if prev.is_none() && s.state.is_finished() && self.dropped.contains(&key) {
                // Pruned here already; the daemon just keeps more than the applet.
                continue;
            }
            let t = match prev {
                // A snapshot never moves a finished transfer back to unfinished: a list
                // fetched before the end event can arrive after it.
                Some(prev) if prev.state.is_finished() && !s.state.is_finished() => prev,
                Some(prev) => {
                    let mut t = Transfer::from_snapshot(s, now);
                    if s.state == prev.state {
                        t.updated_at = prev.updated_at;
                    }
                    t.name = t.name.or(prev.name);
                    t
                }
                None => Transfer::from_snapshot(s, now),
            };
            let mut t = t;
            if t.name.is_none() && t.direction == TransferDirection::Send {
                t.name = self.sent_names.get(&s.id).cloned();
            }
            self.transfers.insert(key, t);
        }
        self.prune(now);
    }

    fn apply_host(&mut self, e: HostEvent, now: u64) -> Refresh {
        match e {
            HostEvent::PeerOnline { .. }
            | HostEvent::PeerOffline { .. }
            | HostEvent::Paired { .. }
            | HostEvent::Unpaired { .. }
            | HostEvent::RemovedByPeer { .. }
            | HostEvent::Battery { .. } => self.apply_peer_event(e, now),
            HostEvent::TransferOffer { .. }
            | HostEvent::TransferProgress { .. }
            | HostEvent::TransferComplete { .. }
            | HostEvent::TransferFailed { .. } => self.apply_transfer_event(e, now),
            // Pairing prompts, clipboard, media, pings and the rest belong to the window and
            // to notifications; the applet ignores them.
            _ => Refresh::NONE,
        }
    }

    fn apply_peer_event(&mut self, e: HostEvent, now: u64) -> Refresh {
        match e {
            HostEvent::PeerOnline { peer, .. } => {
                if let Some(d) = self.devices.get_mut(&peer) {
                    d.online = true;
                    d.last_seen = Some(now);
                }
                // Capabilities change with every session; a device paired in another UI
                // is not listed yet.
                Refresh::PEERS
            }
            HostEvent::PeerOffline { peer, .. } => {
                if let Some(d) = self.devices.get_mut(&peer) {
                    d.online = false;
                    d.last_seen = Some(now);
                    d.battery = None;
                    d.capabilities.clear();
                }
                self.forget_actions(&peer);
                Refresh::NONE
            }
            HostEvent::Paired { .. } => Refresh::PEERS,
            HostEvent::Unpaired { peer, .. } => {
                self.devices.remove(&peer);
                self.forget_actions(&peer);
                Refresh::NONE
            }
            HostEvent::RemovedByPeer { peer, .. } => {
                if let Some(d) = self.devices.get_mut(&peer) {
                    d.removed_by_peer = true;
                }
                // Nothing can be sent to it any more, so a question about it ends here too.
                self.forget_actions(&peer);
                Refresh::NONE
            }
            HostEvent::Battery {
                peer,
                level,
                charging,
                ..
            } => {
                if let Some(d) = self.devices.get_mut(&peer) {
                    d.battery = Some((level.min(100), charging));
                    d.last_seen = Some(now);
                }
                Refresh::NONE
            }
            _ => Refresh::NONE,
        }
    }

    /// A transfer first heard of through an event, before any snapshot listed it.
    fn new_transfer(
        &self,
        id: String,
        peer: String,
        direction: TransferDirection,
        state: TransferStatus,
        now: u64,
    ) -> Transfer {
        let name = if direction == TransferDirection::Send {
            self.sent_names.get(&id).cloned()
        } else {
            None
        };
        Transfer {
            short_id: self.short_id_of(&peer),
            name,
            id,
            peer,
            direction,
            state,
            bytes: 0,
            total: 0,
            file_count: 0,
            reason: None,
            started_at: now,
            updated_at: now,
        }
    }

    /// The transfer `id`, created from the event's fields when no snapshot listed it yet.
    /// The flag says whether it was already known.
    fn transfer_entry(
        &mut self,
        id: &str,
        peer: String,
        direction: TransferDirection,
        state: TransferStatus,
        now: u64,
    ) -> (&mut Transfer, bool) {
        let key = (id.to_owned(), direction);
        let known = self.transfers.contains_key(&key);
        if !known {
            let t = self.new_transfer(id.to_owned(), peer, direction, state, now);
            self.transfers.insert(key.clone(), t);
        }
        let t = self
            .transfers
            .get_mut(&key)
            .expect("inserted just above when missing");
        (t, known)
    }

    fn apply_transfer_event(&mut self, e: HostEvent, now: u64) -> Refresh {
        match e {
            HostEvent::TransferOffer {
                id,
                peer,
                short_id,
                files,
                file_count,
                total_bytes,
                ..
            } => {
                let mut t = self.new_transfer(
                    id.clone(),
                    peer,
                    TransferDirection::Receive,
                    TransferStatus::Offered,
                    now,
                );
                t.short_id = short_id;
                t.total = total_bytes;
                t.file_count = file_count;
                t.name = (file_count == 1)
                    .then(|| files.first().map(|f| manifest_basename(&f.path)))
                    .flatten();
                self.transfers.insert((id, TransferDirection::Receive), t);
                Refresh::NONE
            }
            HostEvent::TransferProgress {
                id,
                peer,
                direction,
                bytes,
                total,
                ..
            } => {
                let (t, known) =
                    self.transfer_entry(&id, peer, direction, TransferStatus::Active, now);
                if t.state.is_finished() {
                    // A late progress event after the end is stale.
                    return Refresh::NONE;
                }
                t.state = TransferStatus::Active;
                t.bytes = bytes.min(total);
                t.total = total;
                t.updated_at = now;
                // The file count only comes with the snapshot.
                if known {
                    Refresh::NONE
                } else {
                    Refresh::TRANSFERS
                }
            }
            HostEvent::TransferComplete {
                id,
                peer,
                direction,
                bytes,
                roots,
                saved,
                ..
            } => {
                let name = single_name(roots.len(), saved.first())
                    .or_else(|| (roots.len() == 1).then(|| manifest_basename(&roots[0].path)));
                let (t, _) =
                    self.transfer_entry(&id, peer, direction, TransferStatus::Complete, now);
                t.state = TransferStatus::Complete;
                t.bytes = bytes;
                t.total = bytes;
                if t.file_count == 0 {
                    t.file_count = roots.len();
                }
                t.name = t.name.take().or(name);
                t.reason = None;
                t.updated_at = now;
                Refresh::NONE
            }
            HostEvent::TransferFailed {
                id,
                peer,
                direction,
                reason,
            } => {
                let (t, _) = self.transfer_entry(&id, peer, direction, TransferStatus::Failed, now);
                // The snapshot tells a cancel from a failure; the event does not.
                t.state = TransferStatus::Failed;
                t.reason = Some(reason);
                t.updated_at = now;
                Refresh::TRANSFERS
            }
            _ => Refresh::NONE,
        }
    }

    fn apply_daemon(&mut self, e: DaemonEvent) -> Refresh {
        match e {
            DaemonEvent::InputSession {
                peer,
                label,
                active,
            } => {
                // Replace any label-only entry for the same label too.
                self.inputs.retain(|s| {
                    s.peer.as_deref() != Some(peer.as_str())
                        && !(s.peer.is_none() && s.label == label)
                });
                if active {
                    self.inputs.push(InputSession {
                        peer: Some(peer),
                        label,
                    });
                }
                Refresh::NONE
            }
            DaemonEvent::LockChanged { locked } => {
                self.locked = locked;
                Refresh::NONE
            }
            // Settings, lock state and input sessions may all have changed too.
            DaemonEvent::HostRestarted => Refresh::ALL,
            // `ShuttingDown` is followed by the stream ending, which is `Disconnected`.
            _ => Refresh::NONE,
        }
    }

    fn short_id_of(&self, peer: &str) -> String {
        self.devices
            .get(peer)
            .map(|d| d.short_id.clone())
            .unwrap_or_default()
    }

    /// Drops finished transfers that are too old or too many, newest kept.
    fn prune(&mut self, now: u64) {
        let mut drop: Vec<TransferKey> = self
            .transfers
            .iter()
            .filter(|(_, t)| {
                t.state.is_finished() && now.saturating_sub(t.updated_at) > FINISHED_FOR_SECS
            })
            .map(|(k, _)| k.clone())
            .collect();
        let mut finished: Vec<(u64, u64, &TransferKey)> = self
            .transfers
            .iter()
            .filter(|(k, t)| t.state.is_finished() && !drop.contains(k))
            .map(|(k, t)| (t.updated_at, t.started_at, k))
            .collect();
        if finished.len() > KEEP_FINISHED {
            finished.sort_by(|a, b| {
                (a.0, a.1, &a.2 .0)
                    .cmp(&(b.0, b.1, &b.2 .0))
                    .then_with(|| dir_rank(a.2 .1).cmp(&dir_rank(b.2 .1)))
            });
            let extra = finished.len() - KEEP_FINISHED;
            drop.extend(finished.into_iter().take(extra).map(|(_, _, k)| k.clone()));
        }
        for key in drop {
            self.transfers.remove(&key);
            if self.dropped.len() == DROPPED_MEMORY {
                self.dropped.pop_front();
            }
            self.dropped.push_back(key);
        }
        let transfers = &self.transfers;
        self.sent_names.retain(|id, _| {
            transfers
                .get(&(id.clone(), TransferDirection::Send))
                .is_none_or(|t| !t.state.is_finished())
        });
    }

    /// Ages out finished transfers; called once a minute while nothing else happens.
    pub fn tick(&mut self, now: u64) {
        self.prune(now);
    }

    /// Marks the action `kind` in flight for a device (`on`) or done.
    pub fn set_busy(&mut self, device_id: &str, kind: Busy, on: bool) {
        let key = (device_id.to_owned(), kind);
        if on {
            self.busy.insert(key);
        } else {
            self.busy.remove(&key);
        }
    }

    /// The device went offline or was unpaired: nothing in flight for it, and no question
    /// about a password-manager clip left behind.
    fn forget_actions(&mut self, device_id: &str) {
        self.busy.retain(|(id, _)| id != device_id);
        if self.confirm_sensitive.as_deref() == Some(device_id) {
            self.confirm_sensitive = None;
        }
    }

    /// Records that the applet started transfer `id` with `paths` (for its row's name).
    pub fn note_sent(&mut self, id: &str, paths: &[std::path::PathBuf]) {
        let name = match paths {
            [one] => one.file_name().map(|n| n.to_string_lossy().into_owned()),
            _ => None,
        };
        if let Some(name) = name {
            if let Some(t) = self
                .transfers
                .get_mut(&(id.to_owned(), TransferDirection::Send))
            {
                if t.name.is_none() {
                    t.name = Some(name.clone());
                }
            }
            self.sent_names.insert(id.to_owned(), name);
        }
    }

    /// Asks the user to confirm sending a password-manager clip to `device_id`, or ends
    /// that question (`None`).
    pub fn set_confirm_sensitive(&mut self, device_id: Option<&str>) {
        // Only for a device that can still receive it; otherwise there is nothing to ask.
        self.confirm_sensitive = device_id
            .filter(|id| {
                self.devices
                    .get(*id)
                    .is_some_and(|d| d.online && !d.removed_by_peer)
            })
            .map(str::to_owned);
    }

    /// The device a password-manager clip waits to be confirmed for.
    pub fn confirm_sensitive(&self) -> Option<&str> {
        self.confirm_sensitive.as_deref()
    }

    /// Sets the one-line message under the header (an action's outcome).
    pub fn set_notice(&mut self, notice: impl Into<String>) {
        self.notice = Some(notice.into());
    }

    /// Clears the message (popup closed).
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// The label of a device for messages, or a neutral word.
    pub fn label_of(&self, device_id: &str) -> String {
        self.devices
            .get(device_id)
            .map_or_else(|| "the device".to_owned(), |d| d.label.clone())
    }

    /// Builds what the popup shows. `can_start` says whether the applet found a daemon
    /// binary it could start; `window_available` whether it found the window.
    pub fn view(&self, can_start: bool, window_available: bool) -> View {
        let running = self.is_running();
        let (status_line, start) = self.status_line(can_start);
        let input_labels: Vec<&str> = self.inputs.iter().map(|s| s.label.as_str()).collect();
        let input_banner = (running && !input_labels.is_empty())
            .then(|| format!("Remote input from {}", input_labels.join(", ")));
        let online = self.devices.values().filter(|d| d.online).count();
        View {
            icon: if running {
                PanelIcon::Normal
            } else {
                PanelIcon::Offline
            },
            tooltip: if running {
                format!(
                    "PlayDrop: {online} of {} {} online",
                    self.devices.len(),
                    plural(self.devices.len(), "device", "devices")
                )
            } else {
                "PlayDrop is not running".to_owned()
            },
            status_line,
            start,
            notice: self.notice.clone(),
            input_banner,
            locked_note: (running && self.locked).then(|| {
                "This desktop is locked. Some actions wait until it is unlocked.".to_owned()
            }),
            devices: self.device_rows(),
            transfers: self.transfer_rows(),
            show_devices: running,
            open_window: if window_available {
                WindowAction::Available
            } else {
                WindowAction::Unavailable(
                    "The PlayDrop window is not installed: playdrop-desktop was not found."
                        .to_owned(),
                )
            },
        }
    }

    fn status_line(&self, can_start: bool) -> (String, StartAction) {
        match &self.daemon {
            DaemonState::Connecting => {
                ("Connecting to PlayDrop...".to_owned(), StartAction::Hidden)
            }
            DaemonState::Starting => ("PlayDrop is starting...".to_owned(), StartAction::Hidden),
            DaemonState::NotReady => (
                "PlayDrop is running but not ready. Restart it, or see its log.".to_owned(),
                StartAction::Hidden,
            ),
            DaemonState::Running { .. } => {
                let online = self.devices.values().filter(|d| d.online).count();
                let line = match self.devices.len() {
                    0 => "PlayDrop is running. No paired devices yet.".to_owned(),
                    n => format!(
                        "PlayDrop is running. {online} of {n} {} online.",
                        plural(n, "device", "devices")
                    ),
                };
                (line, StartAction::Hidden)
            }
            DaemonState::NotRunning => (
                "PlayDrop is not running.".to_owned(),
                if can_start {
                    StartAction::Available
                } else {
                    StartAction::Unavailable(
                        "PlayDrop is not installed for this user: playdropd was not found."
                            .to_owned(),
                    )
                },
            ),
            DaemonState::Failed(m) => (format!("Cannot reach PlayDrop: {m}"), StartAction::Hidden),
        }
    }

    /// Devices online first, then by label ignoring case, then by ID so the order is stable.
    fn device_rows(&self) -> Vec<DeviceRow> {
        let mut devices: Vec<&Device> = self.devices.values().collect();
        devices.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        let duplicate_label = |d: &Device| {
            self.devices
                .values()
                .filter(|o| o.label.to_lowercase() == d.label.to_lowercase())
                .count()
                > 1
        };
        devices
            .into_iter()
            .map(|d| self.device_row(d, duplicate_label(d)))
            .collect()
    }

    /// Active transfers newest first, then the [`SHOW_FINISHED`] most recent finished ones.
    fn transfer_rows(&self) -> Vec<TransferRow> {
        let mut active: Vec<&Transfer> = self
            .transfers
            .values()
            .filter(|t| !t.state.is_finished())
            .collect();
        active.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut finished: Vec<&Transfer> = self
            .transfers
            .values()
            .filter(|t| t.state.is_finished())
            .collect();
        finished.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then_with(|| b.started_at.cmp(&a.started_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        active
            .into_iter()
            .chain(finished.into_iter().take(SHOW_FINISHED))
            .map(|t| self.transfer_row(t))
            .collect()
    }

    fn device_row(&self, d: &Device, show_short_id: bool) -> DeviceRow {
        let title = if show_short_id {
            format!("{} ({})", d.label, d.short_id)
        } else {
            d.label.clone()
        };
        let mut detail = if d.removed_by_peer {
            "Forgot this computer. Pair again from the PlayDrop window.".to_owned()
        } else if d.online {
            "Online".to_owned()
        } else {
            "Offline".to_owned()
        };
        if d.online && !d.removed_by_peer {
            if let Some((level, charging)) = d.battery {
                let _ = write!(
                    detail,
                    ", battery {level}%{}",
                    if charging { " (charging)" } else { "" }
                );
            }
        }
        let busy = |kind: Busy| self.busy.contains(&(d.device_id.clone(), kind));
        let usable = d.online && !d.removed_by_peer;
        let confirm = (usable && self.confirm_sensitive.as_deref() == Some(d.device_id.as_str()))
            .then(|| {
                format!(
                    "This clip came from a password manager. Send it to {}?",
                    d.label
                )
            });
        let clipboard = if !usable || confirm.is_some() {
            Action::Hidden
        } else if busy(Busy::Clipboard) {
            Action::Busy("Sending clipboard...".to_owned())
        } else if !d.clipboard_out {
            Action::Unavailable("Clipboard sharing is off for this device.".to_owned())
        } else if !d.accepts("clipboard") {
            Action::Unavailable("This device does not take clipboard text.".to_owned())
        } else {
            Action::Available
        };
        let files = if !usable {
            Action::Hidden
        } else if busy(Busy::Files) {
            Action::Busy("Choosing what to send...".to_owned())
        } else if !d.accepts("transfer") {
            Action::Unavailable("This device does not take files.".to_owned())
        } else {
            Action::Available
        };
        DeviceRow {
            device_id: d.device_id.clone(),
            title,
            detail,
            online: d.online,
            clipboard,
            files,
            confirm,
        }
    }

    fn transfer_row(&self, t: &Transfer) -> TransferRow {
        let who = self
            .devices
            .get(&t.peer)
            .map(|d| d.label.clone())
            .or_else(|| (!t.short_id.is_empty()).then(|| t.short_id.clone()))
            .unwrap_or_else(|| "a device".to_owned());
        let what = match (&t.name, t.file_count) {
            (Some(n), _) => n.clone(),
            (None, 0) => "files".to_owned(),
            (None, n) => format!("{n} {}", plural(n, "file", "files")),
        };
        let title = match t.direction {
            TransferDirection::Send => format!("{what} to {who}"),
            TransferDirection::Receive => format!("{what} from {who}"),
        };
        let percent = percent(t.bytes, t.total);
        let (detail, progress) = match t.state {
            TransferStatus::Offered => (
                match t.direction {
                    TransferDirection::Send => "Waiting for the other device to accept".to_owned(),
                    TransferDirection::Receive => {
                        "Waiting for you to accept in the PlayDrop window".to_owned()
                    }
                },
                None,
            ),
            TransferStatus::Active => (
                format!(
                    "{} {percent}%",
                    match t.direction {
                        TransferDirection::Send => "Sending",
                        TransferDirection::Receive => "Receiving",
                    }
                ),
                Some(f32::from(percent) / 100.0),
            ),
            TransferStatus::Complete => (
                match t.direction {
                    TransferDirection::Send => "Sent".to_owned(),
                    TransferDirection::Receive => "Received".to_owned(),
                },
                None,
            ),
            TransferStatus::Failed => (
                match &t.reason {
                    Some(r) if !r.is_empty() => format!("Failed: {r}"),
                    _ => "Failed".to_owned(),
                },
                None,
            ),
            TransferStatus::Cancelled => (
                match &t.reason {
                    Some(r) if !r.is_empty() => format!("Cancelled: {r}"),
                    _ => "Cancelled".to_owned(),
                },
                None,
            ),
        };
        TransferRow {
            id: t.id.clone(),
            title,
            detail,
            progress,
        }
    }
}

/// Whole percent of `bytes` out of `total`, 0 to 100.
pub fn percent(bytes: u64, total: u64) -> u8 {
    if total == 0 {
        return 0;
    }
    let p = u128::from(bytes.min(total)) * 100 / u128::from(total);
    u8::try_from(p).unwrap_or(100)
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

/// The plain-words outcome of a clipboard send to `label`.
pub fn clipboard_outcome(label: &str, sent: bool, skipped: Option<ClipSkip>, echo: bool) -> String {
    if sent {
        return format!("Clipboard sent to {label}.");
    }
    let reason = skipped
        .or(echo.then_some(ClipSkip::Echo))
        .map_or("the daemon sent nothing", ClipSkip::reason);
    format!("Clipboard not sent: {reason}.")
}

/// Which panel icon to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelIcon {
    /// Connected.
    Normal,
    /// No daemon.
    Offline,
}

/// A control that may be offered, unavailable with a reason, busy, or not shown at all
/// (anti-pattern 18: never a control that silently does nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show and enable it.
    Available,
    /// Show the reason instead of the control.
    Unavailable(String),
    /// Running now; show this text in place of the control.
    Busy(String),
    /// Not relevant here.
    Hidden,
}

/// "Start `PlayDrop`" on the daemon line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartAction {
    /// The daemon runs or is being reached.
    Hidden,
    /// Offer the button.
    Available,
    /// Say why it cannot be started.
    Unavailable(String),
}

/// "Open `PlayDrop`".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowAction {
    /// Offer the button.
    Available,
    /// Say why there is none.
    Unavailable(String),
}

/// One device line of the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    /// Hex Device ID for the actions.
    pub device_id: String,
    /// Label (with Short ID when two labels match).
    pub title: String,
    /// "Online, battery 80%" and the like.
    pub detail: String,
    /// Session up.
    pub online: bool,
    /// "Send clipboard".
    pub clipboard: Action,
    /// "Send files..." and "Send folder...".
    pub files: Action,
    /// Set while a password-manager clip waits for "Send" or "Cancel" (SR-24).
    pub confirm: Option<String>,
}

/// One transfer line of the popup.
#[derive(Debug, Clone, PartialEq)]
pub struct TransferRow {
    /// Transfer ID.
    pub id: String,
    /// "photo.jpg to Phone".
    pub title: String,
    /// "Sending 45%", "Received", "Failed: reason".
    pub detail: String,
    /// 0.0 to 1.0 while bytes move.
    pub progress: Option<f32>,
}

/// Everything the panel button and popup show.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    /// Panel icon.
    pub icon: PanelIcon,
    /// Panel tooltip.
    pub tooltip: String,
    /// Daemon state line.
    pub status_line: String,
    /// "Start `PlayDrop`".
    pub start: StartAction,
    /// Outcome of the last action.
    pub notice: Option<String>,
    /// "Remote input from X", with a Stop button, while a session is open.
    pub input_banner: Option<String>,
    /// Shown while the desktop is locked.
    pub locked_note: Option<String>,
    /// Show the device section at all (only while connected).
    pub show_devices: bool,
    /// Paired devices, online first, then by label.
    pub devices: Vec<DeviceRow>,
    /// Active transfers, newest first, then the most recent finished ones.
    pub transfers: Vec<TransferRow>,
    /// "Open `PlayDrop`".
    pub open_window: WindowAction,
}

#[cfg(test)]
mod tests;
