//! What this host pushes about itself: battery (protocol.md 5.8, at most once per 60 s per
//! peer unless `charging` or `low` changes), presence (5.10), media player state (5.9,
//! coalesced to 2 frames per second per player) and media commands. Battery and presence
//! are remembered and sent again at every session start.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::session::Session;
use super::{lock, Inner};
use crate::identity::DeviceId;
use crate::session::types::{
    Battery, Capability, Frame, Media, MediaAction, MediaCommand, MediaStateFrame, Presence,
    PresenceState,
};
use crate::{Error, Result};

/// Battery frames per peer: at most one per this period unless `charging` or `low`
/// changed (protocol.md 5.8).
pub(crate) const BATTERY_EVERY: Duration = Duration::from_secs(60);

/// Media state frames per player: at most one per this period, 2 per second (5.9).
pub(crate) const MEDIA_EVERY: Duration = Duration::from_millis(500);

/// A media player's state (protocol.md 5.9), as the host reports it and as
/// `HostEvent::MediaState` carries a peer's. Position is a sample at `position_at_ms`
/// (Unix ms) that the UI extrapolates while `playing`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaState {
    /// Player ID, stable per player.
    pub player: String,
    /// Player display name.
    pub name: Option<String>,
    /// Playing now.
    pub playing: bool,
    /// Track title.
    pub title: Option<String>,
    /// Track artist.
    pub artist: Option<String>,
    /// Track album.
    pub album: Option<String>,
    /// Track duration in ms.
    pub duration_ms: Option<u64>,
    /// Position in ms at `position_at_ms`.
    pub position_ms: Option<u64>,
    /// Unix ms at which `position_ms` was sampled.
    pub position_at_ms: Option<u64>,
    /// Volume, 0 to 100.
    pub volume: Option<u8>,
    /// Actions the player supports.
    pub can: Vec<MediaAction>,
}

impl From<MediaStateFrame> for MediaState {
    fn from(f: MediaStateFrame) -> Self {
        Self {
            player: f.player,
            name: f.name,
            playing: f.playing,
            title: f.title,
            artist: f.artist,
            album: f.album,
            duration_ms: f.duration_ms,
            position_ms: f.position_ms,
            position_at_ms: f.position_at,
            volume: f.volume,
            can: f.can,
        }
    }
}

impl From<MediaState> for MediaStateFrame {
    fn from(s: MediaState) -> Self {
        Self {
            player: s.player,
            name: s.name,
            playing: s.playing,
            title: s.title,
            artist: s.artist,
            album: s.album,
            duration_ms: s.duration_ms,
            position_ms: s.position_ms,
            position_at: s.position_at_ms,
            volume: s.volume,
            can: s.can,
            // Album art needs a blob per track; not sent in v1.
            art: None,
        }
    }
}

/// What a rate gate says about one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Send it now.
    Now,
    /// Not yet: schedule one deferred send after this delay.
    Later(Duration),
    /// A deferred send is already scheduled; it will carry the newest value.
    Queued,
}

#[derive(Debug, Clone, Copy)]
struct BatterySent {
    at: Instant,
    charging: bool,
    low: bool,
}

/// Remembered status and the per-peer and per-player rate state.
#[derive(Debug, Default)]
pub(crate) struct StatusState {
    pub(crate) battery: Option<Battery>,
    pub(crate) presence: Option<Presence>,
    battery_sent: HashMap<DeviceId, BatterySent>,
    battery_pending: HashSet<DeviceId>,
    media_sent: HashMap<String, Instant>,
    media_pending: HashMap<String, MediaState>,
}

impl StatusState {
    /// The 5.8 rule for one peer: first frame, a `charging` or `low` change, or 60 s since
    /// the last one goes now; anything else waits for the end of the 60 s.
    pub(crate) fn battery_gate(&mut self, peer: DeviceId, b: &Battery, now: Instant) -> Gate {
        let go = match self.battery_sent.get(&peer) {
            None => true,
            Some(last) => {
                last.charging != b.charging
                    || last.low != b.low
                    || now.duration_since(last.at) >= BATTERY_EVERY
            }
        };
        if go {
            self.battery_record(peer, b, now);
            return Gate::Now;
        }
        if !self.battery_pending.insert(peer) {
            return Gate::Queued;
        }
        let last = self.battery_sent[&peer].at;
        Gate::Later(BATTERY_EVERY.saturating_sub(now.duration_since(last)))
    }

    /// Notes a battery frame sent to `peer` (also used at session start, 5.8).
    pub(crate) fn battery_record(&mut self, peer: DeviceId, b: &Battery, now: Instant) {
        self.battery_pending.remove(&peer);
        self.battery_sent.insert(
            peer,
            BatterySent {
                at: now,
                charging: b.charging,
                low: b.low,
            },
        );
    }

    /// Ends a deferred battery send.
    pub(crate) fn battery_unqueue(&mut self, peer: DeviceId) {
        self.battery_pending.remove(&peer);
    }

    /// The 5.9 rule for one player: at most one state frame per 500 ms. A state that must
    /// wait replaces any older waiting one, so the deferred send carries the newest.
    pub(crate) fn media_gate(
        &mut self,
        state: MediaState,
        now: Instant,
    ) -> (Gate, Option<MediaState>) {
        let player = state.player.clone();
        let due = self
            .media_sent
            .get(&player)
            .is_none_or(|t| now.duration_since(*t) >= MEDIA_EVERY);
        if due && !self.media_pending.contains_key(&player) {
            self.media_sent.insert(player, now);
            return (Gate::Now, Some(state));
        }
        let queued = self.media_pending.insert(player.clone(), state).is_some();
        if queued {
            return (Gate::Queued, None);
        }
        let wait = self.media_sent.get(&player).map_or(Duration::ZERO, |t| {
            MEDIA_EVERY.saturating_sub(now.duration_since(*t))
        });
        (Gate::Later(wait), None)
    }

    /// Takes the waiting state of `player` for a deferred send and notes it as sent.
    pub(crate) fn media_take(&mut self, player: &str, now: Instant) -> Option<MediaState> {
        let s = self.media_pending.remove(player)?;
        self.media_sent.insert(player.to_owned(), now);
        Some(s)
    }

    /// Forgets a peer's battery bookkeeping (unpair).
    pub(crate) fn forget(&mut self, peer: &DeviceId) {
        self.battery_sent.remove(peer);
        self.battery_pending.remove(peer);
    }
}

/// Remembers the battery and pushes it to every online peer under the 5.8 rule.
pub(crate) fn set_battery(inner: &Arc<Inner>, level: u8, charging: bool, low: bool) {
    let b = Battery {
        level: level.min(100),
        charging,
        low,
    };
    lock(&inner.status).battery = Some(b);
    for s in inner.registry.sessions() {
        battery_to(inner, &s);
    }
}

/// Sends the remembered battery to one session, now or deferred (5.8).
fn battery_to(inner: &Arc<Inner>, sess: &Arc<Session>) {
    let peer = sess.peer();
    let gate = {
        let mut st = lock(&inner.status);
        let Some(b) = st.battery.clone() else { return };
        let g = st.battery_gate(peer, &b, Instant::now());
        if g == Gate::Now {
            sess.send(Frame::Battery(b));
        }
        g
    };
    if let Gate::Later(wait) = gate {
        let i2 = Arc::clone(inner);
        inner.spawn(async move {
            tokio::time::sleep(wait).await;
            lock(&i2.status).battery_unqueue(peer);
            if let Some(s) = i2.registry.session(&peer) {
                battery_to(&i2, &s);
            }
        });
    }
}

/// Remembers the presence state and pushes it to every online peer (5.10).
pub(crate) fn set_presence(
    inner: &Arc<Inner>,
    state: PresenceState,
    reason: Option<String>,
    expect_back_ms: Option<u64>,
) {
    let p = Presence {
        state,
        reason,
        expect_back_ms,
    };
    lock(&inner.status).presence = Some(p.clone());
    for s in inner.registry.sessions() {
        s.send(Frame::Presence(p.clone()));
    }
}

/// At session start: the remembered battery (always, 5.8) and presence (5.10).
pub(crate) fn on_activate(inner: &Arc<Inner>, sess: &Arc<Session>) {
    let (battery, presence) = {
        let mut st = lock(&inner.status);
        let b = st.battery.clone();
        if let Some(b) = &b {
            st.battery_record(sess.peer(), b, Instant::now());
        }
        (b, st.presence.clone())
    };
    if let Some(b) = battery {
        sess.send(Frame::Battery(b));
    }
    if let Some(p) = presence {
        sess.send(Frame::Presence(p));
    }
}

/// Pushes a player's state to every online peer with our `media` grant, coalesced to 2
/// frames per second per player (5.9). *Choice:* what is playing is private, so it goes
/// only to peers the user allowed media for.
pub(crate) fn push_media_state(inner: &Arc<Inner>, state: MediaState) {
    let player = state.player.clone();
    let (gate, now_state) = lock(&inner.status).media_gate(state, Instant::now());
    match gate {
        Gate::Now => {
            if let Some(s) = now_state {
                broadcast_media(inner, s);
            }
        }
        Gate::Later(wait) => {
            let i2 = Arc::clone(inner);
            inner.spawn(async move {
                tokio::time::sleep(wait).await;
                let s = lock(&i2.status).media_take(&player, Instant::now());
                if let Some(s) = s {
                    broadcast_media(&i2, s);
                }
            });
        }
        Gate::Queued => {}
    }
}

fn broadcast_media(inner: &Inner, state: MediaState) {
    let frame = Frame::Media(Media::State(state.into()));
    for s in inner.registry.sessions() {
        let allowed = inner.pins.get(&s.peer()).is_some_and(|p| p.grants.media);
        if allowed {
            s.send(frame.clone());
        }
    }
}

/// Sends a media command to `peer` (5.9). Needs a session ([`Error::NotFound`]) and the
/// peer's `media.control` capability ([`Error::Forbidden`]).
pub(crate) fn send_media_command(
    inner: &Inner,
    peer: &DeviceId,
    player: &str,
    action: MediaAction,
    value: Option<u64>,
) -> Result<()> {
    let s = inner.registry.session(peer).ok_or(Error::NotFound)?;
    if !s.peer_accepts(&Capability::MediaControl) {
        return Err(Error::Forbidden("the device does not accept media control"));
    }
    s.send(Frame::Media(Media::Command(MediaCommand {
        player: player.to_owned(),
        action,
        value,
    })));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(level: u8, charging: bool, low: bool) -> Battery {
        Battery {
            level,
            charging,
            low,
        }
    }

    /// protocol.md 5.8: one frame per 60 s per peer unless `charging` or `low` changes;
    /// a held-back level is sent once when the period ends.
    #[test]
    fn battery_rate_limit() {
        let peer: DeviceId = "ab".repeat(32).parse().unwrap();
        let other: DeviceId = "cd".repeat(32).parse().unwrap();
        let mut st = StatusState::default();
        let t0 = Instant::now();
        assert_eq!(st.battery_gate(peer, &b(80, false, false), t0), Gate::Now);
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(
            st.battery_gate(peer, &b(79, false, false), t1),
            Gate::Later(Duration::from_secs(50))
        );
        assert_eq!(
            st.battery_gate(peer, &b(78, false, false), t1),
            Gate::Queued
        );
        // Another peer has its own budget.
        assert_eq!(st.battery_gate(other, &b(78, false, false), t1), Gate::Now);
        // Plugging in goes at once.
        assert_eq!(st.battery_gate(peer, &b(78, true, false), t1), Gate::Now);
        let t2 = t1 + Duration::from_secs(1);
        assert!(matches!(
            st.battery_gate(peer, &b(79, true, false), t2),
            Gate::Later(_)
        ));
        assert_eq!(st.battery_gate(peer, &b(79, true, true), t2), Gate::Now);
        let t3 = t2 + BATTERY_EVERY;
        assert_eq!(st.battery_gate(peer, &b(70, true, true), t3), Gate::Now);
    }

    /// protocol.md 5.9: 2 state frames per second per player, the newest one wins.
    #[test]
    fn media_coalesced_per_player() {
        let state = |player: &str, title: &str| MediaState {
            player: player.into(),
            name: None,
            playing: true,
            title: Some(title.into()),
            artist: None,
            album: None,
            duration_ms: None,
            position_ms: None,
            position_at_ms: None,
            volume: None,
            can: vec![MediaAction::Play],
        };
        let mut st = StatusState::default();
        let t0 = Instant::now();
        assert_eq!(st.media_gate(state("a", "1"), t0).0, Gate::Now);
        let t1 = t0 + Duration::from_millis(100);
        assert_eq!(
            st.media_gate(state("a", "2"), t1).0,
            Gate::Later(Duration::from_millis(400))
        );
        assert_eq!(st.media_gate(state("a", "3"), t1).0, Gate::Queued);
        assert_eq!(st.media_gate(state("b", "x"), t1).0, Gate::Now);
        let t2 = t0 + MEDIA_EVERY;
        let sent = st.media_take("a", t2).unwrap();
        assert_eq!(sent.title.as_deref(), Some("3"));
        assert!(st.media_take("a", t2).is_none());
        assert!(matches!(
            st.media_gate(state("a", "4"), t2).0,
            Gate::Later(_)
        ));
        let frame: MediaStateFrame = state("a", "5").into();
        assert_eq!(MediaState::from(frame), state("a", "5"));
    }
}
