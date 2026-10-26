//! The transfer book: one snapshot per transfer, both directions, so a UI that connects
//! after events were emitted can rebuild its view (Phase 1 handoff gap 2).
//!
//! Live transfers stay until they end. Finished ones (complete, failed, cancelled) are kept
//! for late UIs, bounded by [`KEEP_FINISHED`] entries and [`KEEP_FOR`] age, whichever drops
//! an entry first, except that an entry younger than [`KEEP_AT_LEAST`] is never dropped for
//! the count (only past the hard ceiling of 1,024 entries).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::events::{DeliveredFile, TransferDirection};
use crate::identity::DeviceId;

/// Most finished snapshots kept.
pub const KEEP_FINISHED: usize = 64;

/// Longest a finished snapshot is kept. *Choice:* 64 entries or 10 minutes, whichever is
/// smaller: enough for a UI restarted after a burst of sends to show the outcomes, without
/// the daemon holding history that belongs in a UI-side log.
pub const KEEP_FOR: Duration = Duration::from_mins(10);

/// Shortest time a finished snapshot is kept, even past [`KEEP_FINISHED`]. *Choice:* a
/// client that started a send and polls the list once a second (`playdrop send`) must
/// still find its outcome after a burst of other transfers ended at the same moment, or
/// it cannot tell a finished send from a lost one.
pub const KEEP_AT_LEAST: Duration = Duration::from_mins(1);

/// Hard ceiling on finished snapshots, young or not, so a flood of tiny transfers cannot
/// grow the book without bound.
const MAX_FINISHED: usize = 1024;

/// Where a transfer stands, as a UI shows it. Coarser than the wire states of
/// protocol.md 7.5: `declined` and `expired` are reported as `cancelled` and `failed`
/// with a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Waiting for the receiving user to accept (protocol.md 7.3).
    Offered,
    /// Accepted and streaming, or waiting to resume (protocol.md 7.6). A send becomes
    /// `active` when its first file or bundle `PUT` starts moving bytes, not when the
    /// receiver accepts.
    Active,
    /// Every file verified or skipped.
    Complete,
    /// Ended without completing: integrity failure, expiry, unreachable peer.
    Failed,
    /// Cancelled or declined by either side.
    Cancelled,
}

impl TransferStatus {
    /// True for `complete`, `failed` and `cancelled`.
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Complete | Self::Failed | Self::Cancelled)
    }
}

/// One transfer as [`Host::transfers`](super::Host::transfers) lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferSnapshot {
    /// Transfer ID.
    pub id: String,
    /// The other device's ID (hex).
    pub peer: String,
    /// The other device's Short ID (SR-8).
    pub short_id: String,
    /// Our side.
    pub direction: TransferDirection,
    /// Where it stands.
    pub state: TransferStatus,
    /// File bytes sent (send side) or verified (receive side).
    pub bytes: u64,
    /// Total file bytes in the manifest.
    pub total: u64,
    /// Most recent rate; 0 once finished.
    pub bytes_per_sec: u64,
    /// Files finished so far, as in `HostEvent::TransferProgress`.
    #[serde(default)]
    pub files_done: usize,
    /// When the transfer was offered or reloaded, Unix seconds.
    pub started_at: u64,
    /// Number of files in the manifest.
    pub file_count: usize,
    /// The sender's note while the transfer runs; the reason once it failed or was
    /// cancelled (SR-22 wording). Peer-supplied text: display only.
    pub note: Option<String>,
    /// Receive side, once complete: where each file landed, as in
    /// `HostEvent::TransferComplete`. A UI that connects late uses these for "Open"; the
    /// daemon only opens paths it reported (SR-23). Empty otherwise.
    #[serde(default)]
    pub saved: Vec<PathBuf>,
    /// Receive side, once complete: the receive folder, for "Show in folder". `None` in
    /// `Staged` mode.
    #[serde(default)]
    pub folder: Option<PathBuf>,
    /// Receive side, once complete: where every file went, both modes, as in
    /// `HostEvent::TransferComplete` (`docs/core-api.md` section 2).
    #[serde(default)]
    pub delivered: Vec<DeliveredFile>,
    /// Send side: true on a `failed` send whose record is kept, so it resumes when the
    /// receiver is back or when the same files are sent again (`docs/core-api.md` section
    /// 12; `note` says which). False once it is cancelled, resumed or complete.
    #[serde(default)]
    pub resumable: bool,
}

impl TransferSnapshot {
    /// A new snapshot with no progress yet.
    pub(crate) fn new(
        id: &str,
        peer: &DeviceId,
        direction: TransferDirection,
        state: TransferStatus,
        total: u64,
        file_count: usize,
        note: Option<String>,
    ) -> Self {
        Self {
            id: id.to_owned(),
            peer: peer.to_hex(),
            short_id: peer.short_id(),
            direction,
            state,
            bytes: 0,
            total,
            bytes_per_sec: 0,
            files_done: 0,
            started_at: unix_now(),
            file_count,
            note,
            saved: Vec::new(),
            folder: None,
            delivered: Vec::new(),
            resumable: false,
        }
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

type Key = (String, TransferDirection);

/// One progress reading of a live transfer, as reported to the book and the event stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ProgressReading {
    /// File bytes moved (sent) or verified (received).
    pub bytes: u64,
    /// Total file bytes.
    pub total: u64,
    /// Recent rate.
    pub bytes_per_sec: u64,
    /// Files finished so far.
    pub files_done: usize,
}

/// Live and recently finished transfers.
#[derive(Debug, Default)]
pub(crate) struct TransferBook {
    live: HashMap<Key, TransferSnapshot>,
    finished: VecDeque<(Instant, TransferSnapshot)>,
    /// Failed sends that wait to resume (`resumable`): listed for as long as their record
    /// exists, not pruned with the other finished snapshots, so a UI or `playdrop cancel`
    /// can still find them hours later.
    waiting: HashMap<Key, TransferSnapshot>,
}

impl TransferBook {
    /// Records a new transfer, replacing any live one with the same ID and direction.
    pub(crate) fn begin(&mut self, snap: TransferSnapshot) {
        self.live.insert((snap.id.clone(), snap.direction), snap);
    }

    /// Moves a live transfer to another live state (offered to active).
    pub(crate) fn set_state(&mut self, id: &str, dir: TransferDirection, state: TransferStatus) {
        if let Some(s) = self.live.get_mut(&(id.to_owned(), dir)) {
            s.state = state;
        }
    }

    /// Records progress of a live transfer. The state is left alone: each side moves it
    /// with [`set_state`](Self::set_state) at the moment it really changes, so a reading
    /// never makes a send look `active` while the receiver is still deciding.
    pub(crate) fn progress(&mut self, id: &str, dir: TransferDirection, r: ProgressReading) {
        if let Some(s) = self.live.get_mut(&(id.to_owned(), dir)) {
            s.bytes = r.bytes;
            s.bytes_per_sec = r.bytes_per_sec;
            s.files_done = r.files_done;
        }
    }

    /// Ends a live transfer and keeps its final snapshot for late UIs. `note` replaces the
    /// sender's note when given. Unknown IDs are ignored.
    pub(crate) fn finish(
        &mut self,
        id: &str,
        dir: TransferDirection,
        state: TransferStatus,
        note: Option<String>,
    ) {
        self.finish_with_files(id, dir, state, note, Vec::new(), None, Vec::new());
    }

    /// [`finish`](Self::finish) that also records where a received transfer's files
    /// landed, so a late UI can still offer "Open" and "Show in folder".
    #[allow(clippy::too_many_arguments, reason = "one snapshot's final fields")]
    pub(crate) fn finish_with_files(
        &mut self,
        id: &str,
        dir: TransferDirection,
        state: TransferStatus,
        note: Option<String>,
        saved: Vec<PathBuf>,
        folder: Option<PathBuf>,
        delivered: Vec<DeliveredFile>,
    ) {
        let Some(mut s) = self.live.remove(&(id.to_owned(), dir)) else {
            return;
        };
        s.saved = saved;
        s.folder = folder;
        s.delivered = delivered;
        s.state = state;
        s.bytes_per_sec = 0;
        if state == TransferStatus::Complete {
            s.bytes = s.total;
            s.files_done = s.file_count;
        }
        if note.is_some() {
            s.note = note;
        }
        self.finished.push_back((Instant::now(), s));
        self.prune(Instant::now());
    }

    /// Drops the finished snapshots with this ID and direction. A send that resumes under
    /// its earlier ID (protocol.md 7.6) calls it before [`begin`](Self::begin), so the list
    /// never shows the same transfer twice.
    pub(crate) fn forget_finished(&mut self, id: &str, dir: TransferDirection) {
        self.finished
            .retain(|(_, s)| !(s.id == id && s.direction == dir));
        self.waiting.remove(&(id.to_owned(), dir));
    }

    /// Lists a send that waits to resume, as after a restart of this host (its record
    /// survived, its snapshot did not).
    pub(crate) fn add_waiting(&mut self, mut snap: TransferSnapshot) {
        snap.resumable = true;
        self.waiting.insert((snap.id.clone(), snap.direction), snap);
    }

    /// Changes how a finished transfer is shown, for a failed send that waited to resume
    /// and was then cancelled. Unknown IDs are ignored.
    pub(crate) fn amend_finished(
        &mut self,
        id: &str,
        dir: TransferDirection,
        state: TransferStatus,
        note: String,
    ) {
        if let Some(mut s) = self.waiting.remove(&(id.to_owned(), dir)) {
            s.state = state;
            s.note = Some(note);
            s.resumable = false;
            self.finished.push_back((Instant::now(), s));
            self.prune(Instant::now());
            return;
        }
        if let Some((_, s)) = self
            .finished
            .iter_mut()
            .rev()
            .find(|(_, s)| s.id == id && s.direction == dir)
        {
            s.state = state;
            s.note = Some(note);
            s.resumable = false;
        }
    }

    /// Marks a finished transfer as waiting to resume (or not).
    pub(crate) fn set_resumable(&mut self, id: &str, dir: TransferDirection, resumable: bool) {
        let key = (id.to_owned(), dir);
        if resumable {
            if let Some(i) = self
                .finished
                .iter()
                .rposition(|(_, s)| s.id == id && s.direction == dir)
            {
                if let Some((_, mut s)) = self.finished.remove(i) {
                    s.resumable = true;
                    self.waiting.insert(key, s);
                }
            }
        } else if let Some(mut s) = self.waiting.remove(&key) {
            s.resumable = false;
            self.finished.push_back((Instant::now(), s));
        }
    }

    /// Whether a live transfer with this ID exists in that direction.
    #[cfg(test)]
    pub(crate) fn is_live(&self, id: &str, dir: TransferDirection) -> bool {
        self.live.contains_key(&(id.to_owned(), dir))
    }

    /// Drops finished snapshots, oldest first: past the hard ceiling, past the count once
    /// they are older than [`KEEP_AT_LEAST`], and past [`KEEP_FOR`] age. The queue is in
    /// finishing order, so the front is always the oldest.
    fn prune(&mut self, now: Instant) {
        let age = |at: &Instant| now.saturating_duration_since(*at);
        while self.finished.len() > MAX_FINISHED {
            self.finished.pop_front();
        }
        while self.finished.len() > KEEP_FINISHED
            && self
                .finished
                .front()
                .is_some_and(|(at, _)| age(at) > KEEP_AT_LEAST)
        {
            self.finished.pop_front();
        }
        while self
            .finished
            .front()
            .is_some_and(|(at, _)| age(at) > KEEP_FOR)
        {
            self.finished.pop_front();
        }
    }

    /// Every live transfer and every retained finished one, oldest first.
    pub(crate) fn list(&mut self) -> Vec<TransferSnapshot> {
        self.list_at(Instant::now())
    }

    /// [`list`](Self::list) as of `now`, so tests can look ahead in time.
    fn list_at(&mut self, now: Instant) -> Vec<TransferSnapshot> {
        self.prune(now);
        let mut out: Vec<TransferSnapshot> = self
            .finished
            .iter()
            .map(|(_, s)| s.clone())
            .chain(self.live.values().cloned())
            .chain(self.waiting.values().cloned())
            .collect();
        out.sort_by(|a, b| a.started_at.cmp(&b.started_at).then(a.id.cmp(&b.id)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(n: usize) -> TransferSnapshot {
        let peer: DeviceId = "ab".repeat(32).parse().unwrap();
        TransferSnapshot::new(
            &format!("t{n}"),
            &peer,
            TransferDirection::Receive,
            TransferStatus::Offered,
            100,
            2,
            Some("hi".into()),
        )
    }

    /// A send that resumes under its ID replaces its finished snapshot instead of
    /// listing the ID twice; a cancelled waiting send shows as cancelled.
    #[test]
    fn resumed_id_is_listed_once() {
        let mut b = TransferBook::default();
        b.begin(snap(0));
        b.finish(
            "t0",
            TransferDirection::Receive,
            TransferStatus::Failed,
            Some("receiver unreachable; resumes".into()),
        );
        b.set_resumable("t0", TransferDirection::Receive, true);
        assert!(b.list()[0].resumable);
        b.amend_finished(
            "t0",
            TransferDirection::Receive,
            TransferStatus::Cancelled,
            "cancelled by you".into(),
        );
        assert_eq!(b.list()[0].state, TransferStatus::Cancelled);
        assert!(!b.list()[0].resumable, "a cancelled send no longer resumes");
        assert_eq!(b.list()[0].note.as_deref(), Some("cancelled by you"));
        b.forget_finished("t0", TransferDirection::Send);
        assert_eq!(b.list().len(), 1, "another direction is another transfer");
        b.forget_finished("t0", TransferDirection::Receive);
        b.begin(snap(0));
        let l = b.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].state, TransferStatus::Offered);
    }

    /// A send that waits to resume stays listed past the age and count bounds of the
    /// other finished snapshots, until it is cancelled or resumed.
    #[test]
    fn waiting_sends_outlive_pruning() {
        let mut b = TransferBook::default();
        b.add_waiting(snap(7));
        let later = Instant::now() + KEEP_FOR * 3;
        let l = b.list_at(later);
        assert_eq!(l.len(), 1);
        assert!(l[0].resumable);
        b.forget_finished("t7", TransferDirection::Receive);
        assert!(b.list_at(later).is_empty(), "a resumed send drops it");
        b.add_waiting(snap(7));
        b.amend_finished(
            "t7",
            TransferDirection::Receive,
            TransferStatus::Cancelled,
            "cancelled by you".into(),
        );
        let l = b.list();
        assert_eq!(l[0].state, TransferStatus::Cancelled);
        assert!(!l[0].resumable);
    }

    #[test]
    fn lifecycle_and_bounds() {
        let mut b = TransferBook::default();
        b.begin(snap(0));
        b.progress(
            "t0",
            TransferDirection::Receive,
            ProgressReading {
                bytes: 40,
                total: 100,
                bytes_per_sec: 10,
                files_done: 1,
            },
        );
        let l = b.list();
        assert_eq!(
            l[0].state,
            TransferStatus::Offered,
            "a reading alone does not change the state"
        );
        assert_eq!(l[0].bytes, 40);
        assert_eq!(l[0].files_done, 1);
        b.set_state("t0", TransferDirection::Receive, TransferStatus::Active);
        assert_eq!(b.list()[0].state, TransferStatus::Active);
        b.finish(
            "t0",
            TransferDirection::Receive,
            TransferStatus::Complete,
            None,
        );
        let l = b.list();
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].state, TransferStatus::Complete);
        assert_eq!(l[0].bytes, 100);
        assert_eq!(
            l[0].files_done, 2,
            "a complete transfer has every file done"
        );
        assert_eq!(l[0].bytes_per_sec, 0);
        assert_eq!(l[0].note.as_deref(), Some("hi"));
        // The wrong direction is a different transfer.
        b.progress(
            "t0",
            TransferDirection::Send,
            ProgressReading {
                bytes: 1,
                total: 1,
                bytes_per_sec: 1,
                files_done: 0,
            },
        );
        assert!(!b.is_live("t0", TransferDirection::Send));

        for n in 1..=100 {
            b.begin(snap(n));
            b.finish(
                &format!("t{n}"),
                TransferDirection::Receive,
                TransferStatus::Failed,
                Some("why".into()),
            );
        }
        // All of them finished just now, so none is dropped for the count yet.
        assert_eq!(b.list().len(), 101);
        let l = b.list_at(Instant::now() + KEEP_AT_LEAST + Duration::from_secs(1));
        assert_eq!(l.len(), KEEP_FINISHED);
        assert!(l.iter().all(|s| s.note.as_deref() == Some("why")));
        assert!(l.iter().any(|s| s.id == "t100"));
        assert!(!l.iter().any(|s| s.id == "t0"));
        assert_eq!(
            b.list_at(Instant::now() + KEEP_FOR + Duration::from_secs(1)),
            Vec::new()
        );
    }

    /// A finished send stays listed for at least a minute, however many other transfers
    /// end right after it, so a client polling once a second (`playdrop send`) always
    /// finds its outcome. Only the hard ceiling can drop it sooner.
    #[test]
    fn finished_send_is_listed_for_at_least_a_minute() {
        const { assert!(KEEP_FOR.as_secs() >= KEEP_AT_LEAST.as_secs()) };
        const { assert!(KEEP_AT_LEAST.as_secs() >= 60) };
        let peer: DeviceId = "cd".repeat(32).parse().unwrap();
        let mut b = TransferBook::default();
        let send = |n: usize| {
            TransferSnapshot::new(
                &format!("s{n}"),
                &peer,
                TransferDirection::Send,
                TransferStatus::Active,
                10,
                1,
                None,
            )
        };
        let t0 = Instant::now();
        b.begin(send(0));
        b.finish(
            "s0",
            TransferDirection::Send,
            TransferStatus::Complete,
            None,
        );
        for n in 1..=200 {
            b.begin(send(n));
            b.finish(
                &format!("s{n}"),
                TransferDirection::Send,
                TransferStatus::Complete,
                None,
            );
        }
        let at = t0 + Duration::from_secs(KEEP_AT_LEAST.as_secs() - 1);
        let l = b.list_at(at);
        let s0 = l.iter().find(|s| s.id == "s0").expect("still listed");
        assert_eq!(s0.state, TransferStatus::Complete);
        assert_eq!(l.len(), 201);
        // After the minute the count applies again.
        let l = b.list_at(Instant::now() + KEEP_AT_LEAST + Duration::from_secs(1));
        assert_eq!(l.len(), KEEP_FINISHED);
        assert!(!l.iter().any(|s| s.id == "s0"));
        // The ceiling holds even for young entries.
        for n in 201..=1200 {
            b.begin(send(n));
            b.finish(
                &format!("s{n}"),
                TransferDirection::Send,
                TransferStatus::Failed,
                None,
            );
        }
        assert!(b.list().len() <= MAX_FINISHED);
    }

    #[test]
    fn finished_receive_keeps_saved_paths() {
        let mut b = TransferBook::default();
        b.begin(snap(1));
        b.finish_with_files(
            "t1",
            TransferDirection::Receive,
            TransferStatus::Complete,
            None,
            vec![PathBuf::from("/r/a.txt"), PathBuf::from("/r/Dir/b.txt")],
            Some(PathBuf::from("/r")),
            vec![DeliveredFile {
                path: "a.txt".into(),
                location: "/r/a.txt".into(),
            }],
        );
        let l = b.list();
        assert_eq!(l[0].delivered.len(), 1);
        assert_eq!(l[0].saved.len(), 2);
        assert_eq!(l[0].folder.as_deref(), Some(std::path::Path::new("/r")));
        // Older daemons sent no paths: the fields default to empty.
        let mut v = serde_json::to_value(&l[0]).unwrap();
        v.as_object_mut().unwrap().remove("saved");
        v.as_object_mut().unwrap().remove("folder");
        let back: TransferSnapshot = serde_json::from_value(v).unwrap();
        assert!(back.saved.is_empty() && back.folder.is_none());
    }

    #[test]
    fn snapshot_round_trips_through_json() {
        let s = snap(7);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"state\":\"offered\""));
        assert!(json.contains("\"direction\":\"receive\""));
        let back: TransferSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }
}
