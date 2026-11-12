//! Notifications a paired phone mirrors to this desktop (protocol.md 5.16, SR-42 to SR-44).
//!
//! The decisions live here, away from any notification server, so they are unit-tested:
//! - [`Table`] keeps, per peer, which opaque `key` is shown under which server ID, oldest
//!   first, at most [`SHOWN_PER_PEER`] (SR-44): a 21st copy closes the oldest.
//! - A later `posted` with the same `key` replaces the shown copy (freedesktop
//!   `replaces_id`); `removed` closes it.
//! - Every copy of a peer closes when it goes offline, is unpaired, or its
//!   `notificationsIn` grant is turned off (SR-42), and every copy closes when the host
//!   stops or desktop notifications are turned off.
//! - Only a copy the user dismissed sends `dismiss` back to the phone. An expired copy, one
//!   closed by the daemon itself, or one closed for any other reason does not.
//!
//! [`run`] applies those decisions through a [`MirrorUi`], one command at a time, so a
//! show's server ID is known before the next command for the same key is decided. The
//! platform side is `notify.rs` (freedesktop on Linux; a plain toast without replace,
//! close or dismiss-back on Windows).
//!
//! SR-43: nothing here logs or formats the app name, title, text or key. Logs carry the
//! peer's short hex and counts only, and [`Cmd`] has no `Debug`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::future::BoxFuture;
use playdrop_core::host::MirroredNotification;
use tokio::sync::mpsc;

use super::Note;

/// The most mirrored notifications shown at once per peer (SR-44).
pub const SHOWN_PER_PEER: usize = 20;

/// Why the notification server closed a notification (freedesktop `NotificationClosed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    /// It timed out.
    Expired,
    /// The user dismissed it.
    Dismissed,
    /// A `CloseNotification` call closed it (ours).
    Closed,
    /// Undefined or reserved.
    Other,
}

impl CloseReason {
    /// The reason from the freedesktop code (1 expired, 2 dismissed, 3 closed by a call).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn from_freedesktop(code: u32) -> Self {
        match code {
            1 => Self::Expired,
            2 => Self::Dismissed,
            3 => Self::Closed,
            _ => Self::Other,
        }
    }
}

/// One shown copy.
struct Shown {
    key: String,
    /// Server ID; `None` where the platform gives none (Windows) or the show failed.
    id: Option<u32>,
}

/// What showing a `posted` copy needs: the ID it replaces, and the oldest copy to close
/// first when the peer is at [`SHOWN_PER_PEER`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PostPlan {
    /// The server ID of the copy with the same key.
    pub replaces: Option<u32>,
    /// The server ID of the oldest copy, closed to stay within the cap.
    pub close: Option<u32>,
}

/// Which mirrored copies are shown, per peer, oldest first.
#[derive(Default)]
pub struct Table {
    peers: HashMap<String, VecDeque<Shown>>,
}

impl Table {
    /// Decides how a `posted` copy for `(peer, key)` is shown. Its entry is taken out of the
    /// table; [`Table::finish_post`] puts it back as the newest with the shown ID.
    pub fn begin_post(&mut self, peer: &str, key: &str) -> PostPlan {
        let q = self.peers.entry(peer.to_owned()).or_default();
        if let Some(i) = q.iter().position(|s| s.key == key) {
            let replaces = q.remove(i).and_then(|s| s.id);
            return PostPlan {
                replaces,
                close: None,
            };
        }
        let mut close = None;
        while q.len() >= SHOWN_PER_PEER {
            close = q.pop_front().and_then(|s| s.id).or(close);
        }
        PostPlan {
            replaces: None,
            close,
        }
    }

    /// Records the copy shown for `(peer, key)` as the peer's newest.
    pub fn finish_post(&mut self, peer: &str, key: String, id: Option<u32>) {
        self.peers
            .entry(peer.to_owned())
            .or_default()
            .push_back(Shown { key, id });
    }

    /// Forgets `(peer, key)` (the phone's notification went away); returns the server ID to
    /// close.
    pub fn remove(&mut self, peer: &str, key: &str) -> Option<u32> {
        let q = self.peers.get_mut(peer)?;
        let i = q.iter().position(|s| s.key == key)?;
        let id = q.remove(i).and_then(|s| s.id);
        if q.is_empty() {
            self.peers.remove(peer);
        }
        id
    }

    /// Forgets every copy of `peer`; returns the server IDs to close (SR-42).
    pub fn close_peer(&mut self, peer: &str) -> Vec<u32> {
        self.peers
            .remove(peer)
            .map(|q| q.into_iter().filter_map(|s| s.id).collect())
            .unwrap_or_default()
    }

    /// Forgets every copy; returns the server IDs to close.
    pub fn close_all(&mut self) -> Vec<u32> {
        self.peers
            .drain()
            .flat_map(|(_, q)| q.into_iter().filter_map(|s| s.id))
            .collect()
    }

    /// The server closed notification `id`. Returns the peer and key to dismiss on the
    /// phone only when the user dismissed it. An ID the table no longer has (the daemon
    /// closed or replaced it, or it is not a mirrored copy) returns `None`.
    ///
    /// An expired copy is kept: servers with a history panel (GNOME, KDE, likely COSMIC)
    /// still hold it, so a later `removed`, replace, cap or user dismissal must still find
    /// it. Closing or replacing an ID the server no longer has is harmless. Every other
    /// reason forgets the copy.
    pub fn server_closed(&mut self, id: u32, reason: CloseReason) -> Option<(String, String)> {
        if reason == CloseReason::Expired {
            return None;
        }
        let (peer, i) = self.peers.iter().find_map(|(peer, q)| {
            q.iter()
                .position(|s| s.id == Some(id))
                .map(|i| (peer.clone(), i))
        })?;
        let q = self.peers.get_mut(&peer)?;
        let shown = q.remove(i)?;
        if q.is_empty() {
            self.peers.remove(&peer);
        }
        (reason == CloseReason::Dismissed).then_some((peer, shown.key))
    }

    /// How many copies of `peer` are shown.
    #[cfg(test)]
    pub fn len(&self, peer: &str) -> usize {
        self.peers.get(peer).map_or(0, VecDeque::len)
    }
}

/// The desktop copy of a mirrored notification: "<app> - <phone label>", then the title and
/// the text. No buttons: a mirrored copy is not one of the daemon's own (no "Open").
pub fn note(n: &MirroredNotification, label: &str) -> Note {
    let app = n.app.trim();
    let app = if app.is_empty() { "Notification" } else { app };
    let body = match (n.title.trim(), n.text.trim()) {
        ("", text) => text.to_owned(),
        (title, "") => title.to_owned(),
        (title, text) => format!("{title}\n{text}"),
    };
    Note::plain(format!("{app} - {label}"), body)
}

/// One step for the mirror worker. No `Debug`: it carries notification content (SR-43).
pub enum Cmd {
    /// Show or replace a copy.
    Posted {
        /// Device ID (hex).
        peer: String,
        /// The note to show.
        note: Note,
        /// The opaque key.
        key: String,
    },
    /// The phone's notification went away: close the copy.
    Removed {
        /// Device ID (hex).
        peer: String,
        /// The opaque key.
        key: String,
    },
    /// Close every copy of a peer (SR-42).
    ClosePeer {
        /// Device ID (hex).
        peer: String,
    },
    /// Close every copy.
    CloseAll,
    /// The notification server closed a notification. Only Linux reports closes so far.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    ServerClosed {
        /// Server ID.
        id: u32,
        /// Why.
        reason: CloseReason,
    },
}

/// Sends `dismiss` for `(peer, key)` to the phone (`Host::send_notification_dismiss`).
pub type Dismiss = Arc<dyn Fn(&str, String) + Send + Sync>;

/// Where the daemon puts its [`Dismiss`] once the host exists.
pub type DismissSlot = Arc<Mutex<Option<Dismiss>>>;

/// The notification calls the mirror makes; tests use a fake so nothing real shows.
pub trait MirrorUi: Send + Sync + 'static {
    /// Shows `note`, replacing notification `replaces`; returns its server ID.
    fn show(&self, note: Note, replaces: Option<u32>) -> BoxFuture<'static, Option<u32>>;
    /// Closes notification `id`.
    fn close(&self, id: u32) -> BoxFuture<'static, ()>;
    /// Starts reporting the server's closes as [`Cmd::ServerClosed`]; called once, at the
    /// first copy shown.
    fn watch_closed(&self, tx: mpsc::UnboundedSender<Cmd>);
}

/// Applies mirror commands in order until every sender is gone.
pub async fn run(
    ui: impl MirrorUi,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::WeakUnboundedSender<Cmd>,
    dismiss: DismissSlot,
) {
    let mut table = Table::default();
    let mut watching = false;
    while let Some(cmd) = rx.recv().await {
        match cmd {
            Cmd::Posted { peer, note, key } => {
                let plan = table.begin_post(&peer, &key);
                if let Some(id) = plan.close {
                    tracing::debug!(
                        peer = short(&peer),
                        "mirrored notifications at the cap; closing the oldest"
                    );
                    ui.close(id).await;
                }
                let id = ui.show(note, plan.replaces).await;
                // A failed replace leaves the old copy shown under its old ID.
                table.finish_post(&peer, key, id.or(plan.replaces));
                if id.is_some() && !watching {
                    if let Some(tx) = tx.upgrade() {
                        ui.watch_closed(tx);
                        watching = true;
                    }
                }
            }
            Cmd::Removed { peer, key } => {
                if let Some(id) = table.remove(&peer, &key) {
                    ui.close(id).await;
                }
            }
            Cmd::ClosePeer { peer } => {
                let ids = table.close_peer(&peer);
                if !ids.is_empty() {
                    tracing::debug!(
                        peer = short(&peer),
                        count = ids.len(),
                        "closing mirrored notifications"
                    );
                }
                for id in ids {
                    ui.close(id).await;
                }
            }
            Cmd::CloseAll => {
                for id in table.close_all() {
                    ui.close(id).await;
                }
            }
            Cmd::ServerClosed { id, reason } => {
                if let Some((peer, key)) = table.server_closed(id, reason) {
                    let f = dismiss
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone();
                    if let Some(f) = f {
                        f(&peer, key);
                    } else {
                        tracing::debug!(
                            peer = short(&peer),
                            "mirrored notification dismissed with no host to tell"
                        );
                    }
                }
            }
        }
    }
}

/// First 8 characters of a hex Device ID, the only form logs carry (SR-32).
fn short(peer: &str) -> &str {
    peer.get(..8).unwrap_or(peer)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn key(n: u32) -> String {
        format!("{n:032x}")
    }

    /// Shows `key` for `peer` the way [`run`] does, with server ID `id`.
    fn post(t: &mut Table, peer: &str, k: &str, id: u32) -> PostPlan {
        let plan = t.begin_post(peer, k);
        t.finish_post(peer, k.to_owned(), Some(id));
        plan
    }

    #[test]
    fn same_key_replaces_the_shown_copy() {
        let mut t = Table::default();
        assert_eq!(post(&mut t, A, &key(1), 10), PostPlan::default());
        let plan = post(&mut t, A, &key(1), 10);
        assert_eq!(plan.replaces, Some(10));
        assert_eq!(plan.close, None);
        assert_eq!(t.len(A), 1);
        // The same key from another peer is another notification.
        assert_eq!(post(&mut t, B, &key(1), 11).replaces, None);
        assert_eq!(t.len(B), 1);
    }

    #[test]
    fn removed_closes_only_that_copy() {
        let mut t = Table::default();
        post(&mut t, A, &key(1), 10);
        post(&mut t, A, &key(2), 11);
        assert_eq!(t.remove(A, &key(1)), Some(10));
        assert_eq!(t.remove(A, &key(1)), None);
        assert_eq!(t.remove(B, &key(2)), None);
        assert_eq!(t.len(A), 1);
    }

    #[test]
    fn the_21st_copy_closes_the_oldest() {
        let mut t = Table::default();
        for n in 0..20 {
            assert_eq!(post(&mut t, A, &key(n), 100 + n).close, None);
        }
        assert_eq!(t.len(A), 20);
        // A replace at the cap closes nothing.
        assert_eq!(post(&mut t, A, &key(0), 100).close, None);
        // Key 0 is now the newest, so key 1 (ID 101) is the oldest.
        let plan = post(&mut t, A, &key(20), 120);
        assert_eq!(plan.close, Some(101));
        assert_eq!(t.len(A), 20);
        assert_eq!(t.remove(A, &key(1)), None);
        // Another peer has its own 20.
        assert_eq!(post(&mut t, B, &key(0), 200).close, None);
    }

    #[test]
    fn close_peer_and_close_all() {
        let mut t = Table::default();
        post(&mut t, A, &key(1), 10);
        post(&mut t, A, &key(2), 11);
        post(&mut t, B, &key(1), 12);
        let mut ids = t.close_peer(A);
        ids.sort_unstable();
        assert_eq!(ids, vec![10, 11]);
        assert_eq!(t.len(A), 0);
        assert_eq!(t.len(B), 1);
        assert_eq!(t.close_peer(A), Vec::<u32>::new());
        assert_eq!(t.close_all(), vec![12]);
        assert_eq!(t.len(B), 0);
    }

    #[test]
    fn only_a_user_dismissal_goes_back_to_the_phone() {
        let mut t = Table::default();
        post(&mut t, A, &key(1), 10);
        post(&mut t, A, &key(2), 11);
        post(&mut t, A, &key(3), 12);
        // Expired: kept, since a history panel may still show it.
        assert_eq!(t.server_closed(10, CloseReason::Expired), None);
        assert_eq!(t.len(A), 3);
        assert_eq!(t.server_closed(11, CloseReason::Closed), None);
        assert_eq!(
            t.server_closed(12, CloseReason::Dismissed),
            Some((A.to_owned(), key(3)))
        );
        assert_eq!(t.len(A), 1);
        // The expired copy is still replaced, and a later user dismissal still goes back.
        assert_eq!(t.begin_post(A, &key(1)).replaces, Some(10));
        t.finish_post(A, key(1), Some(10));
        assert_eq!(
            t.server_closed(10, CloseReason::Dismissed),
            Some((A.to_owned(), key(1)))
        );
        assert_eq!(t.len(A), 0);
        // An undefined reason forgets the copy without telling the phone.
        post(&mut t, A, &key(5), 14);
        assert_eq!(t.server_closed(14, CloseReason::Other), None);
        assert_eq!(t.len(A), 0);
        // Our own close forgets the copy first, so its signal finds nothing.
        post(&mut t, A, &key(4), 13);
        assert_eq!(t.remove(A, &key(4)), Some(13));
        assert_eq!(t.server_closed(13, CloseReason::Dismissed), None);
        // A notification that is not a mirrored copy.
        assert_eq!(t.server_closed(99, CloseReason::Dismissed), None);
    }

    #[test]
    fn freedesktop_reasons() {
        assert_eq!(CloseReason::from_freedesktop(1), CloseReason::Expired);
        assert_eq!(CloseReason::from_freedesktop(2), CloseReason::Dismissed);
        assert_eq!(CloseReason::from_freedesktop(3), CloseReason::Closed);
        assert_eq!(CloseReason::from_freedesktop(4), CloseReason::Other);
    }

    fn mirrored(app: &str, title: &str, text: &str) -> MirroredNotification {
        MirroredNotification {
            key: key(1),
            app: app.into(),
            title: title.into(),
            text: text.into(),
            when_ms: 0,
        }
    }

    #[test]
    fn note_names_the_app_and_the_phone() {
        let n = note(&mirrored("Signal", "Alex", "On my way"), "Pixel");
        assert_eq!(n.summary, "Signal - Pixel");
        assert_eq!(n.body, "Alex\nOn my way");
        assert!(n.actions.is_empty() && !n.persistent && !n.ring);
        assert_eq!(note(&mirrored("Signal", "Alex", ""), "P").body, "Alex");
        assert_eq!(note(&mirrored("Signal", "", "hi"), "P").body, "hi");
        assert_eq!(note(&mirrored("", "", ""), "P").summary, "Notification - P");
    }

    /// Records what the worker asked of the notification server.
    #[derive(Clone, Default)]
    struct FakeUi {
        log: Arc<Mutex<Vec<String>>>,
        next: Arc<AtomicU32>,
        watched: Arc<Mutex<Option<mpsc::UnboundedSender<Cmd>>>>,
    }

    impl MirrorUi for FakeUi {
        fn show(&self, _note: Note, replaces: Option<u32>) -> BoxFuture<'static, Option<u32>> {
            let id = replaces.unwrap_or_else(|| 1 + self.next.fetch_add(1, Ordering::SeqCst));
            self.log
                .lock()
                .unwrap()
                .push(format!("show {id} replaces {replaces:?}"));
            Box::pin(async move { Some(id) })
        }

        fn close(&self, id: u32) -> BoxFuture<'static, ()> {
            self.log.lock().unwrap().push(format!("close {id}"));
            Box::pin(async {})
        }

        fn watch_closed(&self, tx: mpsc::UnboundedSender<Cmd>) {
            *self.watched.lock().unwrap() = Some(tx);
        }
    }

    fn posted(peer: &str, n: u32) -> Cmd {
        Cmd::Posted {
            peer: peer.to_owned(),
            note: note(&mirrored("App", "t", "x"), "Phone"),
            key: key(n),
        }
    }

    /// Ends the worker once it has applied every queued command. The watcher sender the
    /// fake was handed at the first copy also keeps the channel open, so it is dropped too.
    async fn finish(
        ui: &FakeUi,
        tx: mpsc::UnboundedSender<Cmd>,
        worker: tokio::task::JoinHandle<()>,
    ) {
        // The worker upgrades its weak sender at the first copy, so `tx` lives until then.
        let watched = loop {
            if let Some(w) = ui.watched.lock().unwrap().take() {
                break w;
            }
            tokio::task::yield_now().await;
        };
        drop(watched);
        drop(tx);
        worker.await.unwrap();
    }

    #[tokio::test]
    async fn worker_replaces_closes_and_dismisses_back() {
        let ui = FakeUi::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let dismissed = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&dismissed);
        let slot: DismissSlot =
            Arc::new(Mutex::new(Some(Arc::new(move |peer: &str, key: String| {
                seen.lock().unwrap().push((peer.to_owned(), key));
            }) as Dismiss)));
        let worker = tokio::spawn(run(ui.clone(), rx, tx.downgrade(), slot));

        tx.send(posted(A, 1)).unwrap(); // shown as 1
        tx.send(posted(A, 1)).unwrap(); // replaces 1
        tx.send(posted(A, 2)).unwrap(); // shown as 2
        tx.send(posted(B, 1)).unwrap(); // shown as 3
        tx.send(Cmd::Removed {
            peer: A.to_owned(),
            key: key(2),
        })
        .unwrap(); // closes 2
        tx.send(Cmd::ServerClosed {
            id: 2,
            reason: CloseReason::Dismissed,
        })
        .unwrap(); // our own close: no dismiss
        tx.send(Cmd::ServerClosed {
            id: 3,
            reason: CloseReason::Expired,
        })
        .unwrap(); // expiry: no dismiss, still tracked
        tx.send(Cmd::ServerClosed {
            id: 1,
            reason: CloseReason::Dismissed,
        })
        .unwrap(); // the user: dismiss (A, key 1)
        tx.send(posted(A, 3)).unwrap(); // shown as 4
        tx.send(posted(B, 3)).unwrap(); // shown as 5
        tx.send(Cmd::ClosePeer { peer: A.to_owned() }).unwrap(); // closes 4
        tx.send(Cmd::CloseAll).unwrap(); // closes 5
        finish(&ui, tx, worker).await;

        assert_eq!(
            *ui.log.lock().unwrap(),
            vec![
                "show 1 replaces None",
                "show 1 replaces Some(1)",
                "show 2 replaces None",
                "show 3 replaces None",
                "close 2",
                "show 4 replaces None",
                "show 5 replaces None",
                "close 4",
                // The expired copy (3) is still closed with the rest.
                "close 3",
                "close 5",
            ]
        );
        assert_eq!(*dismissed.lock().unwrap(), vec![(A.to_owned(), key(1))]);
    }

    #[tokio::test]
    async fn worker_closes_the_oldest_past_the_cap() {
        let ui = FakeUi::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run(
            ui.clone(),
            rx,
            tx.downgrade(),
            Arc::new(Mutex::new(None)),
        ));
        for n in 0..21 {
            tx.send(posted(A, n)).unwrap();
        }
        finish(&ui, tx, worker).await;
        let log = ui.log.lock().unwrap();
        assert_eq!(log.len(), 22);
        assert_eq!(log[20], "close 1");
        assert_eq!(log[21], "show 21 replaces None");
    }
}
