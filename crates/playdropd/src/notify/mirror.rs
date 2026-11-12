//! Notifications a paired phone mirrors to this desktop (protocol.md 5.16, SR-42 to SR-44).
//!
//! The decisions live here, away from any notification server, so they are unit-tested:
//! - [`Table`] keeps, per peer, which opaque `key` is shown under which server ID, oldest
//!   first, at most [`SHOWN_PER_PEER`] (SR-44): a 21st copy closes the oldest.
//! - A later `posted` with the same `key` and new content replaces the shown copy
//!   (freedesktop `replaces_id`); the same content again changes nothing; `removed` closes it.
//! - Every copy of a peer closes at once when it is unpaired or its `notificationsIn` grant
//!   is turned off (SR-42), and every copy closes when the host stops or desktop
//!   notifications are turned off.
//! - Going offline is not the end of the phone's notifications, so the copies wait
//!   [`GRACE`] for it to come back (see "After a reconnect" below) before they close.
//! - Only a copy the user dismissed sends `dismiss` back to the phone. An expired copy, one
//!   closed by the daemon itself, or one closed for any other reason does not.
//!
//! # After a reconnect
//!
//! A phone sends its current notifications again each time it comes online (it cannot know
//! what the desktop still shows). Closing every copy at offline would show all of them again
//! as new, so instead:
//! - Offline, the copies stay for [`GRACE`]. Back within it, a `posted` that matches a kept
//!   copy changes nothing. The phone's keys are salted per process, so a phone app that
//!   restarted sends the same notification under a new key: a kept copy also matches by
//!   [`Fingerprint`] (content), and the copy then takes the new key so a later `removed` or
//!   user dismissal still finds it.
//! - Kept copies the phone has not sent again within [`RESYNC`] of coming back were removed
//!   on the phone while it was away: they close.
//! - Still offline after [`GRACE`], the copies close (SR-42, later) and their fingerprints
//!   are remembered, at most [`SEEN_PER_PEER`] per peer, so the same old notifications sent
//!   again when the phone returns are not shown again. Trade-off: such a notification, still
//!   on the phone, is then missing from the desktop's list; the user already saw it once.
//!   New content under the same key, or a new `when`, is a new notification and is shown.
//! - A copy the user dismissed while the phone was away, or while a kept copy still waited
//!   to be sent again, is remembered the same way: the `dismiss` may not reach the phone or
//!   may name a key the phone no longer uses, and the copy it sends again must not pop up.
//! - The remembered content is consulted only inside the [`RESYNC`] window after the phone
//!   comes back, the only time it sends old notifications again, and is forgotten when that
//!   window ends. Outside it a `posted` is shown unless it matches a copy still shown, so an
//!   app that posts the same content again later (a recurring reminder with a fixed `when`)
//!   is shown each time.
//! - Where the platform cannot close a copy later (Windows toasts), there is nothing to keep
//!   or close: going offline remembers the fingerprints at once, under the same window rule.
//!
//! [`run`] applies those decisions through a [`MirrorUi`], one command at a time, so a
//! show's server ID is known before the next command for the same key is decided. The
//! platform side is `notify.rs` (freedesktop on Linux; a plain toast without replace,
//! close or dismiss-back on Windows).
//!
//! SR-43: nothing here logs or formats the app name, title, text, key or fingerprint. Logs
//! carry the peer's short hex and counts only, and neither [`Cmd`] nor [`Fingerprint`] nor
//! [`ContentKey`] has `Debug`. Fingerprints and their key live in memory only.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use playdrop_core::host::MirroredNotification;
use tokio::sync::mpsc;

use super::Note;

/// The most mirrored notifications shown at once per peer (SR-44).
pub const SHOWN_PER_PEER: usize = 20;

/// How long a peer's copies stay after it went offline. Long enough for a Wi-Fi roam, a
/// phone reboot or an app update; short enough that a phone carried away does not leave its
/// notifications on the desktop for long (SR-42).
pub const GRACE: Duration = Duration::from_mins(5);

/// How long after a peer came back its kept copies wait to be sent again. The phone sends
/// at most 20 at once right after the session opens (well inside the 30 a minute of SR-44),
/// so a few seconds is plenty; 10 leaves room for a slow phone.
pub const RESYNC: Duration = Duration::from_secs(10);

/// The most fingerprints of closed copies remembered per peer. Three times the shown cap:
/// enough for the copies closed after a grace plus a few rounds of change, small enough to
/// stay a fixed cost per paired phone.
pub const SEEN_PER_PEER: usize = 64;

/// The table's timers; tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// See [`GRACE`].
    pub grace: Duration,
    /// See [`RESYNC`].
    pub resync: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            grace: GRACE,
            resync: RESYNC,
        }
    }
}

/// The random key of the content [`Fingerprint`]s, one per daemon run, never stored.
///
/// Keyed so a fingerprint cannot be checked against guessed content (a one-time code, say)
/// by someone who reads the daemon's memory or a future log mistake. No `Debug` (SR-43).
#[derive(Clone)]
pub struct ContentKey([u8; 32]);

impl ContentKey {
    /// A fresh key from the system random source (the one transfer IDs use).
    pub fn random() -> Self {
        let seed = format!(
            "{}{}",
            playdrop_core::transfer::manifest::new_transfer_id(),
            playdrop_core::transfer::manifest::new_transfer_id()
        );
        Self(blake3::derive_key(
            "playdrop mirrored notification content key",
            seed.as_bytes(),
        ))
    }

    /// A fixed key (tests).
    #[cfg(test)]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The fingerprint of `n`'s content: app, title, text and `when`, not its key.
    pub fn fingerprint(&self, n: &MirroredNotification) -> Fingerprint {
        let mut h = blake3::Hasher::new_keyed(&self.0);
        // Length prefixes keep ("ab", "c") and ("a", "bc") apart.
        for field in [&n.app, &n.title, &n.text] {
            h.update(&(field.len() as u64).to_le_bytes());
            h.update(field.as_bytes());
        }
        h.update(&n.when_ms.to_le_bytes());
        let mut out = [0u8; 16];
        out.copy_from_slice(&h.finalize().as_bytes()[..16]);
        Fingerprint(out)
    }
}

/// 128 bits of a keyed BLAKE3 over a notification's content. No `Debug` (SR-43).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Fingerprint([u8; 16]);

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
    content: Fingerprint,
    /// Carried over a reconnect and not yet sent again by the phone.
    kept: bool,
}

/// What the table holds for one peer.
#[derive(Default)]
struct Peer {
    /// Shown copies, oldest first.
    shown: VecDeque<Shown>,
    /// When the peer went offline with copies shown; the grace runs from here.
    offline_since: Option<Instant>,
    /// The peer went offline and has not been seen back yet. Outlives the grace, so the
    /// return after it still opens the resend window for the remembered content.
    away: bool,
    /// When the resend window after the peer came back ends: the copies still `kept` then
    /// close, and the remembered content is forgotten.
    resync_until: Option<Instant>,
    /// Fingerprints of copies closed while the peer was away (after the grace, by the
    /// user, or at once where copies cannot be closed), oldest first. Consulted only while
    /// `resync_until` is set.
    seen: VecDeque<Fingerprint>,
}

impl Peer {
    /// Nothing worth keeping an entry for: timers only ever act on shown copies and the
    /// remembered content.
    fn is_empty(&self) -> bool {
        self.shown.is_empty() && self.seen.is_empty()
    }

    /// The peer is back: every shown copy waits [`Timing::resync`] to be sent again, and
    /// for that long the remembered content is not shown again.
    fn back_online(&mut self, now: Instant, resync: Duration) {
        self.offline_since = None;
        self.away = false;
        for s in &mut self.shown {
            s.kept = true;
        }
        self.resync_until = (!self.shown.is_empty() || !self.seen.is_empty()).then(|| now + resync);
    }

    fn remember(&mut self, content: Fingerprint) {
        if self.seen.contains(&content) {
            return;
        }
        if self.seen.len() >= SEEN_PER_PEER {
            self.seen.pop_front();
        }
        self.seen.push_back(content);
    }

    /// Forgets every shown copy, remembering its content; returns the IDs to close.
    fn retire_all(&mut self) -> Vec<u32> {
        let mut ids = Vec::with_capacity(self.shown.len());
        while let Some(s) = self.shown.pop_front() {
            self.remember(s.content);
            ids.extend(s.id);
        }
        ids
    }
}

/// What to do with a `posted` copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostPlan {
    /// Already shown, or shown before and closed: no popup, no call.
    Unchanged,
    /// Show it, replacing `replaces`, after closing `close`.
    Show {
        /// The server ID of the copy with the same key.
        replaces: Option<u32>,
        /// The server ID of the oldest copy, closed to stay within the cap.
        close: Option<u32>,
    },
}

/// Which mirrored copies are shown, per peer, oldest first, and what a reconnect keeps.
pub struct Table {
    peers: HashMap<String, Peer>,
    timing: Timing,
    /// Whether the platform can close a copy later (not Windows toasts yet).
    closes: bool,
}

impl Default for Table {
    fn default() -> Self {
        Self::new(Timing::default(), true)
    }
}

impl Table {
    /// A table with `timing`; `closes` is [`MirrorUi::closes`].
    pub fn new(timing: Timing, closes: bool) -> Self {
        Self {
            peers: HashMap::new(),
            timing,
            closes,
        }
    }

    /// Decides how a `posted` copy for `(peer, key)` with `content` is shown. On
    /// [`PostPlan::Show`] the entry with the same key is taken out of the table, and
    /// [`Table::finish_post`] puts it back as the newest with the shown ID.
    pub fn begin_post(
        &mut self,
        peer: &str,
        key: &str,
        content: Fingerprint,
        now: Instant,
    ) -> PostPlan {
        let p = self.peers.entry(peer.to_owned()).or_default();
        if p.away {
            // A frame proves the peer is back even if its online event has not been seen.
            p.back_online(now, self.timing.resync);
        }
        if let Some(i) = p.shown.iter().position(|s| s.key == key) {
            if p.shown[i].content == content {
                p.shown[i].kept = false;
                return PostPlan::Unchanged;
            }
            let replaces = p.shown.remove(i).and_then(|s| s.id);
            return PostPlan::Show {
                replaces,
                close: None,
            };
        }
        // Only kept copies match by content: two live notifications with equal content
        // (same `when` to the millisecond) are rare, but must not merge.
        if let Some(s) = p.shown.iter_mut().find(|s| s.kept && s.content == content) {
            // The phone app restarted, so its salt and every key changed.
            key.clone_into(&mut s.key);
            s.kept = false;
            return PostPlan::Unchanged;
        }
        // Only in the resend window: what the phone sends again on its return, not a later
        // post of the same content.
        if p.resync_until.is_some() && p.seen.contains(&content) {
            return PostPlan::Unchanged;
        }
        let mut close = None;
        while p.shown.len() >= SHOWN_PER_PEER {
            close = p.shown.pop_front().and_then(|s| s.id).or(close);
        }
        PostPlan::Show {
            replaces: None,
            close,
        }
    }

    /// Records the copy shown for `(peer, key)` as the peer's newest.
    pub fn finish_post(&mut self, peer: &str, key: String, content: Fingerprint, id: Option<u32>) {
        self.peers
            .entry(peer.to_owned())
            .or_default()
            .shown
            .push_back(Shown {
                key,
                id,
                content,
                kept: false,
            });
    }

    /// Forgets `(peer, key)` (the phone's notification went away); returns the server ID to
    /// close.
    pub fn remove(&mut self, peer: &str, key: &str) -> Option<u32> {
        let p = self.peers.get_mut(peer)?;
        let i = p.shown.iter().position(|s| s.key == key)?;
        let id = p.shown.remove(i).and_then(|s| s.id);
        if p.is_empty() {
            self.peers.remove(peer);
        }
        id
    }

    /// `peer` went offline: its copies wait [`Timing::grace`] for it. Where copies cannot
    /// be closed later, they are retired at once so only "not shown again" applies.
    pub fn peer_offline(&mut self, peer: &str, now: Instant) {
        let Some(p) = self.peers.get_mut(peer) else {
            return;
        };
        p.resync_until = None;
        p.away = true;
        if !self.closes {
            // Nothing to close: the IDs are dropped, the content is remembered.
            p.retire_all();
            return;
        }
        if !p.shown.is_empty() && p.offline_since.is_none() {
            p.offline_since = Some(now);
        }
    }

    /// `peer` came online: copies kept over its absence wait for it to send them again.
    pub fn peer_online(&mut self, peer: &str, now: Instant) {
        if let Some(p) = self.peers.get_mut(peer) {
            if p.away {
                p.back_online(now, self.timing.resync);
            }
        }
    }

    /// The next time [`Table::tick`] has something to do.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.peers
            .values()
            .flat_map(|p| {
                [
                    p.offline_since.map(|t| t + self.timing.grace),
                    p.resync_until,
                ]
            })
            .flatten()
            .min()
    }

    /// Applies every timer due at `now`; returns the server IDs to close, by peer.
    pub fn tick(&mut self, now: Instant) -> Vec<(String, Vec<u32>)> {
        let grace = self.timing.grace;
        let mut out = Vec::new();
        for (peer, p) in &mut self.peers {
            let mut ids = Vec::new();
            if p.offline_since.is_some_and(|t| now >= t + grace) {
                p.offline_since = None;
                ids.extend(p.retire_all());
            }
            if p.resync_until.is_some_and(|t| now >= t) {
                p.resync_until = None;
                // Removed on the phone while it was away: closed, not remembered, since the
                // phone will not send them again.
                p.shown.retain(|s| {
                    if s.kept {
                        ids.extend(s.id);
                    }
                    !s.kept
                });
                // The resend is over: a later post of the same content is a new one.
                p.seen.clear();
            }
            if !ids.is_empty() {
                out.push((peer.clone(), ids));
            }
        }
        self.peers.retain(|_, p| !p.is_empty());
        out
    }

    /// Forgets everything about `peer`; returns the server IDs to close (SR-42). The
    /// remembered content goes too: after an unpair or a grant turned back on, the phone's
    /// current notifications are wanted again.
    pub fn close_peer(&mut self, peer: &str) -> Vec<u32> {
        self.peers
            .remove(peer)
            .map(|p| p.shown.into_iter().filter_map(|s| s.id).collect())
            .unwrap_or_default()
    }

    /// Forgets everything; returns the server IDs to close.
    pub fn close_all(&mut self) -> Vec<u32> {
        self.peers
            .drain()
            .flat_map(|(_, p)| p.shown.into_iter().filter_map(|s| s.id))
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
        let (peer, i) = self.peers.iter().find_map(|(peer, p)| {
            p.shown
                .iter()
                .position(|s| s.id == Some(id))
                .map(|i| (peer.clone(), i))
        })?;
        let p = self.peers.get_mut(&peer)?;
        let shown = p.shown.remove(i)?;
        let dismissed = reason == CloseReason::Dismissed;
        if dismissed && (p.away || shown.kept) {
            // The `dismiss` cannot reach an offline phone, and for a kept copy it names the
            // key from before the return, which a restarted phone app no longer uses: the
            // phone sends this again in its resend, and remembering it keeps the user's
            // dismissal.
            p.remember(shown.content);
        }
        if p.is_empty() {
            self.peers.remove(&peer);
        }
        dismissed.then_some((peer, shown.key))
    }

    /// How many copies of `peer` are shown.
    #[cfg(test)]
    pub fn len(&self, peer: &str) -> usize {
        self.peers.get(peer).map_or(0, |p| p.shown.len())
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
        /// The content's fingerprint.
        content: Fingerprint,
    },
    /// The phone's notification went away: close the copy.
    Removed {
        /// Device ID (hex).
        peer: String,
        /// The opaque key.
        key: String,
    },
    /// The peer went offline: keep its copies for the grace.
    PeerOffline {
        /// Device ID (hex).
        peer: String,
    },
    /// The peer came online: wait for it to send its kept copies again.
    PeerOnline {
        /// Device ID (hex).
        peer: String,
    },
    /// Close every copy of a peer at once (SR-42).
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
    /// Whether [`MirrorUi::close`] really closes a shown copy (SR-42 exception: not on
    /// Windows yet).
    fn closes(&self) -> bool;
}

/// Applies mirror commands in order until every sender is gone.
pub async fn run(
    ui: impl MirrorUi,
    rx: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::WeakUnboundedSender<Cmd>,
    dismiss: DismissSlot,
) {
    run_with(ui, rx, tx, dismiss, Timing::default()).await;
}

/// [`run`] with the table's timers set by the caller (tests shorten them).
async fn run_with(
    ui: impl MirrorUi,
    mut rx: mpsc::UnboundedReceiver<Cmd>,
    tx: mpsc::WeakUnboundedSender<Cmd>,
    dismiss: DismissSlot,
    timing: Timing,
) {
    let mut table = Table::new(timing, ui.closes());
    let mut watching = false;
    loop {
        let cmd = match table.next_deadline() {
            Some(at) => tokio::select! {
                cmd = rx.recv() => cmd,
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => {
                    for (peer, ids) in table.tick(Instant::now()) {
                        tracing::debug!(
                            peer = short(&peer),
                            count = ids.len(),
                            "closing mirrored notifications the phone no longer has"
                        );
                        for id in ids {
                            ui.close(id).await;
                        }
                    }
                    continue;
                }
            },
            None => rx.recv().await,
        };
        let Some(cmd) = cmd else { break };
        match cmd {
            Cmd::Posted {
                peer,
                note,
                key,
                content,
            } => {
                let PostPlan::Show { replaces, close } =
                    table.begin_post(&peer, &key, content, Instant::now())
                else {
                    continue;
                };
                if let Some(id) = close {
                    tracing::debug!(
                        peer = short(&peer),
                        "mirrored notifications at the cap; closing the oldest"
                    );
                    ui.close(id).await;
                }
                let id = ui.show(note, replaces).await;
                // A failed replace leaves the old copy shown under its old ID.
                table.finish_post(&peer, key, content, id.or(replaces));
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
            Cmd::PeerOffline { peer } => table.peer_offline(&peer, Instant::now()),
            Cmd::PeerOnline { peer } => table.peer_online(&peer, Instant::now()),
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

    /// The same key as [`key`] under another phone process salt.
    fn salted(n: u32) -> String {
        format!("{:032x}", 0xffff_0000 + n)
    }

    fn content_key() -> ContentKey {
        ContentKey::from_bytes([7; 32])
    }

    /// Notification `n` (its text and `when` follow `n`), under key `k`.
    fn notification(k: &str, n: u32) -> MirroredNotification {
        MirroredNotification {
            key: k.to_owned(),
            app: "App".into(),
            title: "t".into(),
            text: format!("text {n}"),
            when_ms: 1_000 + u64::from(n),
        }
    }

    /// The fingerprint of notification `n`.
    fn fp(n: u32) -> Fingerprint {
        content_key().fingerprint(&notification(&key(n), n))
    }

    /// Shows notification `n` under key `k` for `peer` the way [`run`] does, with server ID
    /// `id` when it is shown.
    fn post_as(t: &mut Table, peer: &str, k: &str, n: u32, id: u32, now: Instant) -> PostPlan {
        let plan = t.begin_post(peer, k, fp(n), now);
        if let PostPlan::Show { replaces, .. } = plan {
            t.finish_post(peer, k.to_owned(), fp(n), Some(id).or(replaces));
        }
        plan
    }

    fn post(t: &mut Table, peer: &str, n: u32, id: u32, now: Instant) -> PostPlan {
        post_as(t, peer, &key(n), n, id, now)
    }

    fn shown() -> PostPlan {
        PostPlan::Show {
            replaces: None,
            close: None,
        }
    }

    /// A table with notifications `0..count` of peer A shown as IDs `100 + n`.
    fn with_twenty(now: Instant, count: u32) -> Table {
        let mut t = Table::default();
        for n in 0..count {
            assert_eq!(post(&mut t, A, n, 100 + n, now), shown());
        }
        t
    }

    /// No server IDs.
    const NONE: Vec<u32> = Vec::new();

    fn all_closed(ticks: Vec<(String, Vec<u32>)>) -> Vec<u32> {
        let mut ids: Vec<u32> = ticks.into_iter().flat_map(|(_, ids)| ids).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn fingerprint_covers_content_not_key() {
        let k = content_key();
        let a = notification(&key(1), 1);
        let same_under_new_key = MirroredNotification {
            key: salted(1),
            ..notification(&key(1), 1)
        };
        assert!(k.fingerprint(&a) == k.fingerprint(&same_under_new_key));
        let new_when = MirroredNotification {
            when_ms: a.when_ms + 1,
            ..notification(&key(1), 1)
        };
        assert!(k.fingerprint(&a) != k.fingerprint(&new_when));
        let shifted = MirroredNotification {
            app: "Ap".into(),
            title: "pt".into(),
            ..notification(&key(1), 1)
        };
        assert!(k.fingerprint(&a) != k.fingerprint(&shifted));
        // Another daemon run, another key.
        assert!(k.fingerprint(&a) != ContentKey::random().fingerprint(&a));
    }

    #[test]
    fn same_key_new_content_replaces_and_same_content_is_unchanged() {
        let now = Instant::now();
        let mut t = Table::default();
        assert_eq!(post(&mut t, A, 1, 10, now), shown());
        assert_eq!(post(&mut t, A, 1, 10, now), PostPlan::Unchanged);
        assert_eq!(
            post_as(&mut t, A, &key(1), 2, 10, now),
            PostPlan::Show {
                replaces: Some(10),
                close: None
            }
        );
        assert_eq!(t.len(A), 1);
        // The same key from another peer is another notification.
        assert_eq!(post(&mut t, B, 1, 11, now), shown());
        assert_eq!(t.len(B), 1);
    }

    #[test]
    fn equal_content_under_two_live_keys_is_two_copies() {
        let now = Instant::now();
        let mut t = Table::default();
        post_as(&mut t, A, &key(1), 1, 10, now);
        assert_eq!(post_as(&mut t, A, &key(2), 1, 11, now), shown());
        assert_eq!(t.len(A), 2);
    }

    #[test]
    fn removed_closes_only_that_copy() {
        let now = Instant::now();
        let mut t = Table::default();
        post(&mut t, A, 1, 10, now);
        post(&mut t, A, 2, 11, now);
        assert_eq!(t.remove(A, &key(1)), Some(10));
        assert_eq!(t.remove(A, &key(1)), None);
        assert_eq!(t.remove(B, &key(2)), None);
        assert_eq!(t.len(A), 1);
    }

    #[test]
    fn the_21st_copy_closes_the_oldest() {
        let now = Instant::now();
        let mut t = with_twenty(now, 20);
        assert_eq!(t.len(A), 20);
        // A replace at the cap closes nothing.
        assert_eq!(
            post_as(&mut t, A, &key(0), 50, 100, now),
            PostPlan::Show {
                replaces: Some(100),
                close: None
            }
        );
        // Key 0 is now the newest, so key 1 (ID 101) is the oldest.
        assert_eq!(
            post(&mut t, A, 20, 120, now),
            PostPlan::Show {
                replaces: None,
                close: Some(101)
            }
        );
        assert_eq!(t.len(A), 20);
        assert_eq!(t.remove(A, &key(1)), None);
        // Another peer has its own 20.
        assert_eq!(post(&mut t, B, 0, 200, now), shown());
    }

    #[test]
    fn reconnect_within_grace_sending_all_20_again_shows_and_closes_nothing() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 20);
        t.peer_offline(A, t0);
        let back = t0 + Duration::from_secs(299);
        assert_eq!(all_closed(t.tick(back)), NONE);
        t.peer_online(A, back);
        for n in 0..20 {
            assert_eq!(post(&mut t, A, n, 999, back), PostPlan::Unchanged);
        }
        assert_eq!(all_closed(t.tick(back + RESYNC)), NONE);
        assert_eq!(t.len(A), 20);
        assert_eq!(t.next_deadline(), None);
    }

    #[test]
    fn kept_copies_the_phone_does_not_send_again_close_after_the_resync_window() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 20);
        t.peer_offline(A, t0);
        let back = t0 + Duration::from_secs(60);
        t.peer_online(A, back);
        // Notifications 3 and 7 were removed on the phone while it was away.
        for n in (0..20).filter(|n| *n != 3 && *n != 7) {
            assert_eq!(post(&mut t, A, n, 999, back), PostPlan::Unchanged);
        }
        assert_eq!(t.next_deadline(), Some(back + RESYNC));
        assert_eq!(
            all_closed(t.tick(back + Duration::from_millis(9_999))),
            NONE
        );
        assert_eq!(all_closed(t.tick(back + RESYNC)), vec![103, 107]);
        assert_eq!(t.len(A), 18);
        // They are not remembered: the phone may post them again, and then they show.
        assert_eq!(post(&mut t, A, 3, 103, back + RESYNC), shown());
    }

    #[test]
    fn a_restarted_phone_matches_by_content_and_the_new_key_takes_over() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 20);
        t.peer_offline(A, t0);
        let back = t0 + Duration::from_secs(30);
        t.peer_online(A, back);
        for n in 0..20 {
            assert_eq!(
                post_as(&mut t, A, &salted(n), n, 999, back),
                PostPlan::Unchanged
            );
        }
        assert_eq!(all_closed(t.tick(back + RESYNC)), NONE);
        // A later `removed` under the new key closes the right copy; the old key is gone.
        assert_eq!(t.remove(A, &salted(5)), Some(105));
        assert_eq!(t.remove(A, &key(6)), None);
        // So does a user dismissal: it goes back under the new key.
        assert_eq!(
            t.server_closed(106, CloseReason::Dismissed),
            Some((A.to_owned(), salted(6)))
        );
        assert_eq!(t.len(A), 18);
    }

    #[test]
    fn past_the_grace_copies_close_and_the_same_ones_are_not_shown_again() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 20);
        t.peer_offline(A, t0);
        assert_eq!(t.next_deadline(), Some(t0 + GRACE));
        let closed = all_closed(t.tick(t0 + GRACE));
        assert_eq!(closed, (100..120).collect::<Vec<_>>());
        assert_eq!(t.len(A), 0);
        assert_eq!(t.next_deadline(), None);

        let later = t0 + GRACE + Duration::from_secs(3600);
        t.peer_online(A, later);
        // The same old notifications, under the same or a new salt: none shown.
        for n in 0..10 {
            assert_eq!(post(&mut t, A, n, 999, later), PostPlan::Unchanged);
        }
        for n in 10..20 {
            assert_eq!(
                post_as(&mut t, A, &salted(n), n, 999, later),
                PostPlan::Unchanged
            );
        }
        // A changed one (same key, new content) and a brand new one are shown.
        assert_eq!(post_as(&mut t, A, &key(4), 40, 300, later), shown());
        assert_eq!(post(&mut t, A, 21, 301, later), shown());
        // The same text with a new `when` is new too.
        let again = MirroredNotification {
            when_ms: 9_999,
            ..notification(&key(8), 8)
        };
        assert_eq!(
            t.begin_post(A, &key(8), content_key().fingerprint(&again), later),
            shown()
        );
        // Once the resend window is over, the remembered content is forgotten: the same
        // content posted again is shown.
        t.tick(later + RESYNC);
        assert_eq!(post(&mut t, A, 0, 302, later + RESYNC), shown());
    }

    /// An app that posts the same content again with a fixed `when` (a recurring reminder,
    /// `when` 0) is shown each time outside the resend window, long after a reconnect.
    #[test]
    fn a_repost_of_the_same_content_after_the_resend_window_is_shown() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 1);
        t.peer_offline(A, t0);
        t.tick(t0 + GRACE);
        let back = t0 + GRACE + Duration::from_secs(60);
        t.peer_online(A, back);
        assert_eq!(t.next_deadline(), Some(back + RESYNC));
        // The phone's resend: not shown again.
        assert_eq!(post(&mut t, A, 0, 200, back), PostPlan::Unchanged);
        assert_eq!(t.tick(back + RESYNC), Vec::new());
        assert_eq!(t.next_deadline(), None);
        // A day later the app posts it again under a new key, then again after a removal.
        let later = back + Duration::from_hours(24);
        assert_eq!(post_as(&mut t, A, &salted(0), 0, 201, later), shown());
        assert_eq!(t.remove(A, &salted(0)), Some(201));
        assert_eq!(post_as(&mut t, A, &salted(0), 0, 202, later), shown());
        // Same key and content as a copy still shown changes nothing.
        assert_eq!(
            post_as(&mut t, A, &salted(0), 0, 203, later),
            PostPlan::Unchanged
        );
    }

    #[test]
    fn remembered_content_is_capped_per_peer() {
        let t0 = Instant::now();
        let mut t = Table::default();
        let mut now = t0;
        // Four rounds of 20 retired copies: 80, of which the newest 64 are remembered.
        for round in 0..4 {
            for n in 0..20 {
                post(&mut t, A, round * 20 + n, round * 20 + n, now);
            }
            t.peer_offline(A, now);
            now += GRACE;
            t.tick(now);
            t.peer_online(A, now);
        }
        assert_eq!(t.peers[A].seen.len(), SEEN_PER_PEER);
        assert_eq!(post(&mut t, A, 15, 1, now), shown());
        assert_eq!(post(&mut t, A, 16, 1, now), PostPlan::Unchanged);
    }

    #[test]
    fn unpair_and_grant_off_during_the_grace_close_at_once() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 3);
        t.peer_offline(A, t0);
        let mut ids = t.close_peer(A);
        ids.sort_unstable();
        assert_eq!(ids, vec![100, 101, 102]);
        assert_eq!(t.next_deadline(), None);
        assert_eq!(all_closed(t.tick(t0 + GRACE)), NONE);
        // Nothing is remembered either: the current set is wanted again.
        assert_eq!(post(&mut t, A, 0, 200, t0), shown());
    }

    #[test]
    fn offline_with_nothing_shown_starts_no_timer() {
        let now = Instant::now();
        let mut t = Table::default();
        t.peer_offline(A, now);
        post(&mut t, B, 1, 10, now);
        t.peer_offline(A, now);
        assert_eq!(t.next_deadline(), None);
    }

    #[test]
    fn a_frame_while_offline_counts_as_back_online() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 2);
        t.peer_offline(A, t0);
        assert_eq!(post(&mut t, A, 0, 999, t0), PostPlan::Unchanged);
        // The late online event does not restart the window over a confirmed copy.
        t.peer_online(A, t0);
        assert_eq!(all_closed(t.tick(t0 + RESYNC)), vec![101]);
        assert_eq!(t.len(A), 1);
    }

    #[test]
    fn without_close_offline_remembers_content_at_once() {
        let now = Instant::now();
        let mut t = Table::new(Timing::default(), false);
        post(&mut t, A, 1, 10, now);
        post(&mut t, A, 2, 11, now);
        t.peer_offline(A, now);
        assert_eq!(t.len(A), 0);
        assert_eq!(t.next_deadline(), None);
        // The first frame after the return opens the resend window: the old copy sent
        // again is not shown a second time, a new one is.
        assert_eq!(
            post_as(&mut t, A, &salted(1), 1, 12, now),
            PostPlan::Unchanged
        );
        assert_eq!(post(&mut t, A, 3, 12, now), shown());
        assert_eq!(t.next_deadline(), Some(now + RESYNC));
        // After the window the same content is a new post again.
        assert_eq!(t.tick(now + RESYNC), Vec::new());
        assert_eq!(post(&mut t, A, 2, 13, now + RESYNC), shown());
        // Online all along: nothing is remembered, a repost after a removal is shown.
        assert_eq!(t.remove(A, &key(2)), Some(13));
        assert_eq!(post(&mut t, A, 2, 14, now + RESYNC), shown());
    }

    #[test]
    fn a_dismissal_while_offline_is_kept_over_the_return() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 2);
        t.peer_offline(A, t0);
        assert_eq!(
            t.server_closed(100, CloseReason::Dismissed),
            Some((A.to_owned(), key(0)))
        );
        t.peer_online(A, t0 + Duration::from_secs(5));
        assert_eq!(
            post(&mut t, A, 0, 999, t0 + Duration::from_secs(5)),
            PostPlan::Unchanged
        );
        assert_eq!(t.len(A), 1);
    }

    /// A copy the user dismissed in the resend window, before the phone sent it again
    /// (under the new key of a restarted app), does not pop up again with that resend.
    #[test]
    fn a_dismissal_of_a_kept_copy_in_the_resend_window_is_kept() {
        let t0 = Instant::now();
        let mut t = with_twenty(t0, 2);
        t.peer_offline(A, t0);
        let back = t0 + Duration::from_secs(5);
        t.peer_online(A, back);
        assert_eq!(
            t.server_closed(100, CloseReason::Dismissed),
            Some((A.to_owned(), key(0)))
        );
        assert_eq!(
            post_as(&mut t, A, &salted(0), 0, 999, back + Duration::from_secs(1)),
            PostPlan::Unchanged
        );
        assert_eq!(
            post_as(&mut t, A, &salted(1), 1, 999, back + Duration::from_secs(1)),
            PostPlan::Unchanged
        );
        // A copy dismissed online, outside any window, is not remembered: the phone got the
        // `dismiss`, and a later post of the same content is new.
        t.tick(back + RESYNC);
        assert_eq!(
            t.server_closed(101, CloseReason::Dismissed),
            Some((A.to_owned(), salted(1)))
        );
        assert_eq!(post(&mut t, A, 1, 300, back + RESYNC), shown());
    }

    #[test]
    fn close_peer_and_close_all() {
        let now = Instant::now();
        let mut t = Table::default();
        post(&mut t, A, 1, 10, now);
        post(&mut t, A, 2, 11, now);
        post(&mut t, B, 1, 12, now);
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
        let now = Instant::now();
        let mut t = Table::default();
        post(&mut t, A, 1, 10, now);
        post(&mut t, A, 2, 11, now);
        post(&mut t, A, 3, 12, now);
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
        assert_eq!(
            post_as(&mut t, A, &key(1), 9, 10, now),
            PostPlan::Show {
                replaces: Some(10),
                close: None
            }
        );
        assert_eq!(
            t.server_closed(10, CloseReason::Dismissed),
            Some((A.to_owned(), key(1)))
        );
        assert_eq!(t.len(A), 0);
        // An undefined reason forgets the copy without telling the phone.
        post(&mut t, A, 5, 14, now);
        assert_eq!(t.server_closed(14, CloseReason::Other), None);
        assert_eq!(t.len(A), 0);
        // Our own close forgets the copy first, so its signal finds nothing.
        post(&mut t, A, 4, 13, now);
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

        fn closes(&self) -> bool {
            true
        }
    }

    /// Notification `n` of `peer` under key `k`.
    fn posted_as(peer: &str, k: &str, n: u32) -> Cmd {
        Cmd::Posted {
            peer: peer.to_owned(),
            note: note(&mirrored("App", "t", "x"), "Phone"),
            key: k.to_owned(),
            content: fp(n),
        }
    }

    fn posted(peer: &str, n: u32) -> Cmd {
        posted_as(peer, &key(n), n)
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
        tx.send(posted(A, 1)).unwrap(); // same content: nothing
        tx.send(posted_as(A, &key(1), 9)).unwrap(); // new content: replaces 1
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

    /// The worker's own timers, shortened: a reconnect that sends 18 of 20 again shows
    /// nothing new and closes the other 2 when the resync window ends; then, offline past
    /// the grace, the rest close.
    #[tokio::test]
    async fn worker_runs_the_resync_and_grace_timers() {
        let ui = FakeUi::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let timing = Timing {
            grace: Duration::from_millis(400),
            resync: Duration::from_millis(100),
        };
        let worker = tokio::spawn(run_with(
            ui.clone(),
            rx,
            tx.downgrade(),
            Arc::new(Mutex::new(None)),
            timing,
        ));
        for n in 0..20 {
            tx.send(posted(A, n)).unwrap(); // shown as n + 1
        }
        tx.send(Cmd::PeerOffline { peer: A.to_owned() }).unwrap();
        tx.send(Cmd::PeerOnline { peer: A.to_owned() }).unwrap();
        for n in 2..20 {
            tx.send(posted_as(A, &salted(n), n)).unwrap();
        }
        let wait_for = |count: usize| {
            let log = Arc::clone(&ui.log);
            async move {
                tokio::time::timeout(Duration::from_secs(10), async {
                    while log.lock().unwrap().len() < count {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("the worker's timer fired");
            }
        };
        wait_for(22).await;
        {
            let log = ui.log.lock().unwrap();
            assert!(log[..20].iter().all(|l| l.starts_with("show ")));
            assert_eq!(log[20..], ["close 1", "close 2"]);
        }
        tx.send(Cmd::PeerOffline { peer: A.to_owned() }).unwrap();
        wait_for(40).await;
        // Back after the grace, the same 18 are not shown again.
        tx.send(Cmd::PeerOnline { peer: A.to_owned() }).unwrap();
        for n in 2..20 {
            tx.send(posted(A, n)).unwrap();
        }
        finish(&ui, tx, worker).await;
        let log = ui.log.lock().unwrap();
        assert_eq!(log.len(), 40);
        let mut closed: Vec<&str> = log[22..].iter().map(String::as_str).collect();
        closed.sort_unstable();
        let mut want: Vec<String> = (3..=20).map(|id| format!("close {id}")).collect();
        want.sort_unstable();
        assert_eq!(closed, want);
    }
}
