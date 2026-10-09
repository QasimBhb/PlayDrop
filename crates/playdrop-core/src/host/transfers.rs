//! The transfer book: one snapshot per transfer, both directions, so a UI that connects
//! after events were emitted can rebuild its view (Phase 1 handoff gap 2).
//!
//! Live transfers stay until they end. Finished ones (complete, failed, cancelled) are kept
//! for late UIs, bounded by [`KEEP_FINISHED`] entries and [`KEEP_FOR`] age, whichever drops
//! an entry first.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::events::TransferDirection;
use crate::identity::DeviceId;

/// Most finished snapshots kept.
pub const KEEP_FINISHED: usize = 64;

/// Longest a finished snapshot is kept. *Choice:* 64 entries or 10 minutes, whichever is
/// smaller: enough for a UI restarted after a burst of sends to show the outcomes, without
/// the daemon holding history that belongs in a UI-side log.
pub const KEEP_FOR: Duration = Duration::from_mins(10);

/// Where a transfer stands, as a UI shows it. Coarser than the wire states of
/// protocol.md 7.5: `declined` and `expired` are reported as `cancelled` and `failed`
/// with a note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransferStatus {
    /// Waiting for the receiving user to accept (protocol.md 7.3).
    Offered,
    /// Accepted and streaming, or waiting to resume (protocol.md 7.6).
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
    /// Receive side, once complete: the receive folder, for "Show in folder".
    #[serde(default)]
    pub folder: Option<PathBuf>,
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
            started_at: unix_now(),
            file_count,
            note,
            saved: Vec::new(),
            folder: None,
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

/// Live and recently finished transfers.
#[derive(Debug, Default)]
pub(crate) struct TransferBook {
    live: HashMap<Key, TransferSnapshot>,
    finished: VecDeque<(Instant, TransferSnapshot)>,
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

    /// Records progress of a live transfer.
    pub(crate) fn progress(&mut self, id: &str, dir: TransferDirection, bytes: u64, rate: u64) {
        if let Some(s) = self.live.get_mut(&(id.to_owned(), dir)) {
            s.bytes = bytes;
            s.bytes_per_sec = rate;
            if s.state == TransferStatus::Offered {
                s.state = TransferStatus::Active;
            }
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
        self.finish_with_files(id, dir, state, note, Vec::new(), None);
    }

    /// [`finish`](Self::finish) that also records where a received transfer's files
    /// landed, so a late UI can still offer "Open" and "Show in folder".
    pub(crate) fn finish_with_files(
        &mut self,
        id: &str,
        dir: TransferDirection,
        state: TransferStatus,
        note: Option<String>,
        saved: Vec<PathBuf>,
        folder: Option<PathBuf>,
    ) {
        let Some(mut s) = self.live.remove(&(id.to_owned(), dir)) else {
            return;
        };
        s.saved = saved;
        s.folder = folder;
        s.state = state;
        s.bytes_per_sec = 0;
        if state == TransferStatus::Complete {
            s.bytes = s.total;
        }
        if note.is_some() {
            s.note = note;
        }
        self.finished.push_back((Instant::now(), s));
        self.prune(Instant::now());
    }

    /// Whether a live transfer with this ID exists in that direction.
    #[cfg(test)]
    pub(crate) fn is_live(&self, id: &str, dir: TransferDirection) -> bool {
        self.live.contains_key(&(id.to_owned(), dir))
    }

    fn prune(&mut self, now: Instant) {
        while self.finished.len() > KEEP_FINISHED {
            self.finished.pop_front();
        }
        while self
            .finished
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > KEEP_FOR)
        {
            self.finished.pop_front();
        }
    }

    /// Every live transfer and every retained finished one, oldest first.
    pub(crate) fn list(&mut self) -> Vec<TransferSnapshot> {
        self.prune(Instant::now());
        let mut out: Vec<TransferSnapshot> = self
            .finished
            .iter()
            .map(|(_, s)| s.clone())
            .chain(self.live.values().cloned())
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

    #[test]
    fn lifecycle_and_bounds() {
        let mut b = TransferBook::default();
        b.begin(snap(0));
        b.progress("t0", TransferDirection::Receive, 40, 10);
        let l = b.list();
        assert_eq!(l[0].state, TransferStatus::Active);
        assert_eq!(l[0].bytes, 40);
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
        assert_eq!(l[0].bytes_per_sec, 0);
        assert_eq!(l[0].note.as_deref(), Some("hi"));
        // The wrong direction is a different transfer.
        b.progress("t0", TransferDirection::Send, 1, 1);
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
        let l = b.list();
        assert_eq!(l.len(), KEEP_FINISHED);
        assert!(l.iter().all(|s| s.note.as_deref() == Some("why")));
        assert!(l.iter().any(|s| s.id == "t100"));
        assert!(!l.iter().any(|s| s.id == "t0"));
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
        );
        let l = b.list();
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
