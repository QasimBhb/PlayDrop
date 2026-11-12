//! Remote input (protocol.md 5.12, SR-25), both directions.
//!
//! **Controller side** ([`send_input`]): a phone or desktop sends `input` frames to a peer
//! that advertises the matching `input.*` capability. Pointer moves and scrolls are
//! coalesced to one frame per [`MOVE_EVERY`] (5.12: "one per 8 ms"); a button, key or
//! presenter frame flushes any pending move first so ordering is kept. Nothing is
//! remembered across sessions.
//!
//! **Target side** ([`on_frame`]): frames are applied only inside an input session the peer
//! opened with `session`/`start` and only while the desktop is unlocked (SR-25; the grant
//! check happened before this module sees the frame, SR-13, SR-14). Keys are capped at
//! [`MAX_KEYS_PER_SEC`] (5.12); a session idle for [`IDLE_TIMEOUT`] ends by itself, and a
//! session ends when its connection drops, so the host always gets a final `stop` and can
//! release every key and button (T31). The core never injects anything itself: it emits
//! [`HostEvent::Input`] and the host's platform backend applies it.
//!
//! **Why a session ended or never started** ([`InputEndReason`]): the target answers a
//! `start` it will not honour, and ends a running session, with an `error`
//! `input_session_ended` that carries a machine-readable `reason` next to its message
//! (protocol.md 5.12, 11.1). The controller turns that, an older target's `forbidden` or
//! message, a refusal of its own (the peer lists no `input.*`), or the loss of the
//! connection into exactly one [`HostEvent::InputSessionEnded`] per session it started.
//! An answer that names an earlier `start` (its `re`) is ignored, so it cannot end a newer
//! session. A target older than 0.11.0 answers only its first refusal per connection: a
//! later `start` to it that can be known to be refused ends at once without being sent,
//! and a locked older target gives no answer at all (protocol.md 11.1).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use super::events::HostEvent;
use super::session::Session;
use super::{lock, Inner};
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::session::types::{Capability, Frame, Input, PointerInput, ScrollUnit, SessionOp};
use crate::session::ErrorCode;
use crate::{Error, Result};

/// Pointer moves and scrolls to one peer: at most one frame per this period (5.12).
pub(crate) const MOVE_EVERY: Duration = Duration::from_millis(8);
/// Key frames accepted per second per peer (5.12).
pub(crate) const MAX_KEYS_PER_SEC: u32 = 60;
/// An input session with no frame for this long ends (SR-25).
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_mins(10);
/// How often idle sessions are checked.
const IDLE_SWEEP: Duration = Duration::from_secs(15);

/// Why an input session this host controls ended, or why its `start` was not honoured
/// (protocol.md 5.12). Carried by [`HostEvent::InputSessionEnded`] so a UI can say it in its
/// own words instead of showing the target's English text.
///
/// The serde names are also the wire values of the `reason` field of `error`
/// `input_session_ended` for the five reasons a target sends; [`Self::ConnectionLost`] and
/// [`Self::Other`] are decided by the controller and never sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputEndReason {
    /// The target has the `input` grant off for this device.
    GrantOff,
    /// The target is locked (SR-25).
    Locked,
    /// The target does not accept remote input at all: it lists no `input.*` capability
    /// (no input backend, or the feature is hidden there).
    NotAccepted,
    /// The target's user ended it (the kill switch, SR-25).
    StoppedByUser,
    /// No input for 10 minutes (SR-25).
    IdleTimeout,
    /// The connection to the target ended, or there was none.
    ConnectionLost,
    /// Anything else, including an older target whose message is not one of the known
    /// ones, and a reason this build does not know.
    #[default]
    #[serde(other)]
    Other,
}

impl InputEndReason {
    /// Every reason a target may put on the wire.
    const ON_THE_WIRE: [Self; 5] = [
        Self::GrantOff,
        Self::Locked,
        Self::NotAccepted,
        Self::StoppedByUser,
        Self::IdleTimeout,
    ];

    /// The `reason` value on the wire, `None` for the two only a controller decides.
    pub fn wire(self) -> Option<&'static str> {
        match self {
            Self::GrantOff => Some("grant_off"),
            Self::Locked => Some("locked"),
            Self::NotAccepted => Some("not_accepted"),
            Self::StoppedByUser => Some("stopped_by_user"),
            Self::IdleTimeout => Some("idle_timeout"),
            Self::ConnectionLost | Self::Other => None,
        }
    }

    /// A `reason` from the wire; `None` for a value this build does not know.
    pub fn from_wire(code: &str) -> Option<Self> {
        Self::ON_THE_WIRE
            .into_iter()
            .find(|r| r.wire() == Some(code))
    }

    /// The English text for this reason: the `message` a target sends next to the code,
    /// and the text of an event the controller raises itself. The texts of locked, stopped
    /// and idle are the ones 0.10.0 sent, so [`Self::from_message`] reads an old target.
    pub fn message(self) -> &'static str {
        match self {
            Self::GrantOff => "remote input is turned off for this device on the target",
            Self::Locked => "the desktop is locked",
            Self::NotAccepted => "the device does not accept remote input",
            Self::StoppedByUser => "stopped on the desktop",
            Self::IdleTimeout => "no input for 10 minutes",
            Self::ConnectionLost => "the connection to the device was lost",
            Self::Other => "ended by the device",
        }
    }

    /// The reason behind a message from a target older than the `reason` field (0.10.0
    /// sent only these three texts), [`Self::Other`] for anything else.
    pub fn from_message(message: &str) -> Self {
        [Self::Locked, Self::StoppedByUser, Self::IdleTimeout]
            .into_iter()
            .find(|r| r.message() == message)
            .unwrap_or(Self::Other)
    }
}

/// A move or scroll waiting for the next send slot, summed over the period.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct Pending {
    dx: f64,
    dy: f64,
    set: bool,
}

impl Pending {
    fn add(&mut self, dx: f64, dy: f64) {
        self.dx += dx;
        self.dy += dy;
        self.set = true;
    }

    fn take(&mut self) -> Option<(f64, f64)> {
        if !self.set {
            return None;
        }
        let out = (self.dx, self.dy);
        *self = Self::default();
        Some(out)
    }
}

/// Controller-side state for one peer.
#[derive(Debug, Default)]
struct Outgoing {
    last_sent: Option<Instant>,
    /// A deferred flush is scheduled.
    scheduled: bool,
    mv: Pending,
    scroll_line: Pending,
    scroll_pixel: Pending,
}

/// Target-side state for one peer with an open input session.
#[derive(Debug)]
struct IncomingSession {
    last_frame: Instant,
    /// Start of the current one-second key window and the keys counted in it.
    key_window: Instant,
    keys_in_window: u32,
    /// Keys dropped by the cap since the last log line, to log once rather than per key.
    dropped_keys: u32,
}

/// Remote input bookkeeping, both directions.
#[derive(Debug, Default)]
pub(crate) struct InputState {
    outgoing: HashMap<DeviceId, Outgoing>,
    incoming: HashMap<DeviceId, IncomingSession>,
    /// Peers we sent `start` to and have not seen the session end with: each gets exactly
    /// one [`HostEvent::InputSessionEnded`], whichever of the end signals comes first.
    controlling: HashSet<DeviceId>,
}

impl InputState {
    /// Whether an input session from `peer` is open on this host.
    #[cfg(test)]
    fn session_open(&self, peer: &DeviceId) -> bool {
        self.incoming.contains_key(peer)
    }

    /// Peers with an open input session.
    pub(crate) fn open_sessions(&self) -> Vec<DeviceId> {
        self.incoming.keys().copied().collect()
    }

    /// Applies the 5.12 key cap for one key frame at `now`: `true` to pass it on.
    fn admit_key(&mut self, peer: &DeviceId, now: Instant) -> bool {
        let Some(s) = self.incoming.get_mut(peer) else {
            return false;
        };
        if now.duration_since(s.key_window) >= Duration::from_secs(1) {
            s.key_window = now;
            s.keys_in_window = 0;
        }
        if s.keys_in_window >= MAX_KEYS_PER_SEC {
            s.dropped_keys += 1;
            return false;
        }
        s.keys_in_window += 1;
        true
    }

    /// Sessions idle since before `cutoff`, removed.
    fn expire_idle(&mut self, cutoff: Instant) -> Vec<DeviceId> {
        let gone: Vec<DeviceId> = self
            .incoming
            .iter()
            .filter(|(_, s)| s.last_frame < cutoff)
            .map(|(p, _)| *p)
            .collect();
        for p in &gone {
            self.incoming.remove(p);
        }
        gone
    }
}

/// The capability a frame needs on the peer (protocol.md 5.3, section 4).
fn capability_for(input: &Input) -> &'static [Capability] {
    match input {
        Input::Session { .. } => &[
            Capability::InputPointer,
            Capability::InputKey,
            Capability::InputPresenter,
        ],
        Input::Pointer(_) => &[Capability::InputPointer],
        Input::Key(_) => &[Capability::InputKey],
        Input::Presenter { .. } => &[Capability::InputPresenter],
    }
}

/// True for `session`/`start`.
fn is_start(input: &Input) -> bool {
    matches!(
        input,
        Input::Session {
            op: SessionOp::Start
        }
    )
}

/// Sends one input frame to `peer`. Needs a session ([`Error::NotFound`]) and the matching
/// `input.*` capability on the peer ([`Error::Forbidden`], the message says which). Moves
/// and scrolls are coalesced per [`MOVE_EVERY`]; everything else goes at once after
/// flushing a pending move. A `start` that fails either way also raises
/// [`HostEvent::InputSessionEnded`] ([`InputEndReason::ConnectionLost`] or
/// [`InputEndReason::NotAccepted`]), so the UI learns why from the same event as every other
/// end.
pub(crate) fn send_input(inner: &Arc<Inner>, peer: &DeviceId, input: Input) -> Result<()> {
    let start = is_start(&input);
    let Some(sess) = inner.registry.session(peer) else {
        if start {
            emit_ended(inner, *peer, InputEndReason::ConnectionLost, None);
        }
        return Err(Error::NotFound);
    };
    let needed = capability_for(&input);
    if !needed.iter().any(|c| sess.peer_accepts(c)) {
        if start {
            emit_ended(inner, *peer, InputEndReason::NotAccepted, None);
        }
        return Err(Error::Forbidden(match input {
            Input::Session { .. } => "the device does not accept remote input",
            Input::Pointer(_) => "the device does not accept pointer input",
            Input::Key(_) => "the device does not accept keyboard input",
            Input::Presenter { .. } => "the device does not accept presenter input",
        }));
    }
    match input {
        Input::Pointer(PointerInput::Move { dx, dy }) => {
            queue_motion(inner, &sess, |o| o.mv.add(dx, dy));
        }
        Input::Pointer(PointerInput::Scroll { dx, dy, unit }) => {
            let pixel = unit == Some(ScrollUnit::Pixel);
            queue_motion(inner, &sess, |o| {
                if pixel {
                    o.scroll_pixel.add(dx, dy);
                } else {
                    o.scroll_line.add(dx, dy);
                }
            });
        }
        other => {
            flush_now(inner, &sess);
            match &other {
                Input::Session {
                    op: SessionOp::Stop,
                } => {
                    // Our own stop: the UI knows, so no event follows.
                    let mut st = lock(&inner.input);
                    st.outgoing.remove(peer);
                    st.controlling.remove(peer);
                }
                Input::Session {
                    op: SessionOp::Start,
                } => {
                    if sess.start_known_refused() {
                        // A 0.10.0 target that refused input once on this connection drops
                        // every later `input` frame unanswered (5.3), so this `start` would
                        // never end. It is not sent: if the grant was turned back on there,
                        // a session the UI does not know about would open.
                        emit_ended(inner, *peer, InputEndReason::GrantOff, None);
                        return Ok(());
                    }
                    lock(&inner.input).controlling.insert(*peer);
                    sess.note_start_queued();
                }
                _ => {}
            }
            sess.send(Frame::Input(other));
        }
    }
    Ok(())
}

/// Adds motion to the peer's pending state and sends now or schedules one deferred flush.
fn queue_motion(inner: &Arc<Inner>, sess: &Arc<Session>, add: impl FnOnce(&mut Outgoing)) {
    let peer = sess.peer();
    let now = Instant::now();
    let wait = {
        let mut st = lock(&inner.input);
        let o = st.outgoing.entry(peer).or_default();
        add(o);
        let due = o
            .last_sent
            .is_none_or(|t| now.duration_since(t) >= MOVE_EVERY);
        if due && !o.scheduled {
            o.last_sent = Some(now);
            None
        } else if o.scheduled {
            return;
        } else {
            o.scheduled = true;
            Some(o.last_sent.map_or(Duration::ZERO, |t| {
                MOVE_EVERY.saturating_sub(now.duration_since(t))
            }))
        }
    };
    match wait {
        None => flush_now(inner, sess),
        Some(wait) => {
            let i2 = Arc::clone(inner);
            let s2 = Arc::clone(sess);
            inner.spawn(async move {
                tokio::time::sleep(wait).await;
                {
                    let mut st = lock(&i2.input);
                    if let Some(o) = st.outgoing.get_mut(&peer) {
                        o.scheduled = false;
                        o.last_sent = Some(Instant::now());
                    }
                }
                flush_now(&i2, &s2);
            });
        }
    }
}

/// Sends whatever motion waits for `sess`'s peer, move first, then scrolls.
fn flush_now(inner: &Arc<Inner>, sess: &Arc<Session>) {
    let frames = {
        let mut st = lock(&inner.input);
        let Some(o) = st.outgoing.get_mut(&sess.peer()) else {
            return;
        };
        let mut frames = Vec::with_capacity(3);
        if let Some((dx, dy)) = o.mv.take() {
            frames.push(Input::Pointer(PointerInput::Move { dx, dy }));
        }
        if let Some((dx, dy)) = o.scroll_line.take() {
            frames.push(Input::Pointer(PointerInput::Scroll { dx, dy, unit: None }));
        }
        if let Some((dx, dy)) = o.scroll_pixel.take() {
            frames.push(Input::Pointer(PointerInput::Scroll {
                dx,
                dy,
                unit: Some(ScrollUnit::Pixel),
            }));
        }
        frames
    };
    for f in frames {
        if !sess.is_live() {
            return;
        }
        sess.send(Frame::Input(f));
    }
}

/// A target-side refusal before any session state: `input` from `peer` that this host will
/// not handle at all (it lists no `input.*`, or the `input` grant is off). A `start` is
/// answered with `error` `input_session_ended` and `reason`, so the controller can say
/// why; every other frame is dropped silently (the grant case sends its one `forbidden`
/// elsewhere, 5.3). Only a paired peer gets this far, and the answers are capped per
/// connection ([`Session::allow_start_answer`]), so a peer that loops on `start` cannot make
/// this host queue without bound.
pub(crate) fn refuse(sess: &Session, seq: u64, input: &Input, reason: InputEndReason) {
    tracing::debug!(peer = %short(&sess.peer()), ?reason, "input refused; dropped");
    if is_start(input) && sess.allow_start_answer() {
        sess.send_raw(ended_frame(reason), Some(seq));
    }
}

/// The `error` `input_session_ended` frame for `reason`.
fn ended_frame(reason: InputEndReason) -> Frame {
    match reason.wire() {
        Some(code) => {
            Frame::error_with_reason(ErrorCode::InputSessionEnded, reason.message(), code)
        }
        None => Frame::error(ErrorCode::InputSessionEnded, Some(reason.message())),
    }
}

/// Whether `peer` holds the `input` grant now (SR-14). Read under the input lock by
/// [`on_frame`], which is what makes turning the grant off final: the grant is stored
/// before [`end_incoming`] takes the lock, so a frame either is handled before that end or
/// sees the grant off.
fn input_granted(inner: &Inner, peer: &DeviceId) -> bool {
    inner.pins.get(peer).is_some_and(|p| p.grants.input)
}

/// An `input` frame from `peer` (frame number `seq`) that passed the capability and grant
/// checks (SR-13, SR-14). Applies the 5.12 and SR-25 target-side rules and emits
/// [`HostEvent::Input`] for the host to apply. The grant is checked again under the input
/// lock, and events are emitted under it, so a session cannot open, or a frame be applied,
/// after the grant was turned off or the peer unpaired and its session ended.
pub(crate) fn on_frame(inner: &Arc<Inner>, peer: DeviceId, seq: u64, input: Input) {
    let now = Instant::now();
    if inner.lock_state.is_locked() {
        // SR-25: no input into a locked desktop, fail closed. An open session ends so the
        // host releases keys and hides its indicator; the peer must start again. A `start`
        // is answered too, so the controller says "locked" instead of waiting.
        let was_open = lock(&inner.input).incoming.remove(&peer).is_some();
        tracing::debug!(peer = %short(&peer), "input while locked; dropped");
        if was_open {
            emit_stop(inner, peer);
        }
        let start = is_start(&input);
        if let Some(s) = inner.registry.session(&peer) {
            if was_open {
                s.send_raw(ended_frame(InputEndReason::Locked), start.then_some(seq));
            } else if start && s.allow_start_answer() {
                s.send_raw(ended_frame(InputEndReason::Locked), Some(seq));
            }
        }
        return;
    }
    match &input {
        Input::Session {
            op: SessionOp::Start,
        } => {
            let mut st = lock(&inner.input);
            if !input_granted(inner, &peer) {
                drop(st);
                tracing::debug!(peer = %short(&peer), "input start after the grant went; dropped");
                if let Some(s) = inner.registry.session(&peer) {
                    refuse(&s, seq, &input, InputEndReason::GrantOff);
                }
                return;
            }
            let fresh = st
                .incoming
                .insert(
                    peer,
                    IncomingSession {
                        last_frame: now,
                        key_window: now,
                        keys_in_window: 0,
                        dropped_keys: 0,
                    },
                )
                .is_none();
            if fresh {
                tracing::info!(peer = %short(&peer), "remote input session started");
                inner.emit(HostEvent::Input {
                    peer: peer.to_hex(),
                    input,
                });
            }
        }
        Input::Session {
            op: SessionOp::Stop,
        } => {
            if lock(&inner.input).incoming.remove(&peer).is_some() {
                tracing::info!(peer = %short(&peer), "remote input session stopped by the peer");
                emit_stop(inner, peer);
            }
        }
        Input::Session { .. } => {}
        _ => {
            let mut st = lock(&inner.input);
            let Some(s) = st.incoming.get_mut(&peer) else {
                tracing::debug!(peer = %short(&peer), "input outside a session; dropped");
                return;
            };
            s.last_frame = now;
            let admitted = if matches!(input, Input::Key(_)) {
                let ok = st.admit_key(&peer, now);
                if let Some(s) = st.incoming.get_mut(&peer) {
                    if !ok && s.dropped_keys == 1 {
                        tracing::info!(peer = %short(&peer), "keys over the 60 per second cap are dropped (5.12)");
                    }
                    if ok {
                        s.dropped_keys = 0;
                    }
                }
                ok
            } else {
                true
            };
            // Emitted under the lock: an end that removes this session afterwards emits
            // its `stop` after this frame, so the host never applies input past its stop.
            if admitted {
                inner.emit(HostEvent::Input {
                    peer: peer.to_hex(),
                    input,
                });
            }
        }
    }
}

fn emit_stop(inner: &Inner, peer: DeviceId) {
    inner.emit(HostEvent::Input {
        peer: peer.to_hex(),
        input: Input::Session {
            op: SessionOp::Stop,
        },
    });
}

/// Ends a session we are the target of: the host gets its final `stop`, and the controller
/// gets an `error` `input_session_ended` with the reason (protocol.md 11.1), so both sides
/// agree the session is over. An `error` is used because `input` frames flow only from the
/// controller (5.3) and a phone lists no `input.*` capability.
fn end_and_tell(inner: &Inner, peer: DeviceId, reason: InputEndReason) {
    emit_stop(inner, peer);
    if let Some(s) = inner.registry.session(&peer) {
        s.send_raw(ended_frame(reason), None);
    }
}

/// Raises the one [`HostEvent::InputSessionEnded`] for a session we control, with `text`
/// (the target's words) or the reason's own text.
fn emit_ended(inner: &Inner, peer: DeviceId, code: InputEndReason, text: Option<String>) {
    inner.emit(HostEvent::InputSessionEnded {
        peer: peer.to_hex(),
        reason: text.unwrap_or_else(|| code.message().to_owned()),
        code,
    });
}

/// Forgets the session we control with `peer` and its pending motion; true if there was
/// one, so its end is reported once.
fn take_controlled(inner: &Inner, peer: &DeviceId) -> bool {
    let mut st = lock(&inner.input);
    st.outgoing.remove(peer);
    st.controlling.remove(peer)
}

/// The target of a session we control ended it or refused its `start` (`error`
/// `input_session_ended`): drop pending motion and tell the UI why. The reason is the
/// frame's `reason`, else read from the message of an older target, else
/// [`InputEndReason::Other`]. Nothing is raised when no session of ours is open: a target
/// answers every refused `start`, and the frames that followed it must not repeat the event.
pub(crate) fn on_ended_by_target(
    inner: &Arc<Inner>,
    peer: DeviceId,
    message: Option<String>,
    reason: Option<&str>,
) {
    if !take_controlled(inner, &peer) {
        tracing::debug!(peer = %short(&peer), "input session end for no session of ours");
        return;
    }
    let code = reason
        .and_then(InputEndReason::from_wire)
        .unwrap_or_else(|| {
            message
                .as_deref()
                .map_or(InputEndReason::Other, InputEndReason::from_message)
        });
    emit_ended(inner, peer, code, message);
}

/// The target answered one of our `input` frames with `forbidden`: its `input` grant is off
/// for us. A target from before the `reason` field answers a refused `start` only this way
/// (once per connection, 5.3), so this ends the session we started with
/// [`InputEndReason::GrantOff`].
pub(crate) fn on_refused_by_target(inner: &Arc<Inner>, peer: DeviceId) {
    if take_controlled(inner, &peer) {
        emit_ended(inner, peer, InputEndReason::GrantOff, None);
    }
}

/// The session with `peer` ended: an open input session from it ends too (the host gets a
/// final `stop`, T31), pending motion to it is dropped, and a session we control ends with
/// [`InputEndReason::ConnectionLost`].
pub(crate) fn on_session_end(inner: &Arc<Inner>, peer: DeviceId) {
    let (was_open, controlled) = {
        let mut st = lock(&inner.input);
        st.outgoing.remove(&peer);
        (
            st.incoming.remove(&peer).is_some(),
            st.controlling.remove(&peer),
        )
    };
    if was_open {
        tracing::info!(peer = %short(&peer), "remote input session ended with the connection");
        emit_stop(inner, peer);
    }
    if controlled {
        emit_ended(inner, peer, InputEndReason::ConnectionLost, None);
    }
}

/// Ends the input session `peer` holds on this host, if any, and tells it `reason`.
pub(crate) fn end_incoming(inner: &Arc<Inner>, peer: DeviceId, reason: InputEndReason) {
    if lock(&inner.input).incoming.remove(&peer).is_some() {
        tracing::info!(peer = %short(&peer), ?reason, "remote input session ended here");
        end_and_tell(inner, peer, reason);
    }
}

/// Ends every open input session this host is the target of (the host's kill switch,
/// SR-25) and tells the peers. Returns the peers that had one.
pub(crate) fn stop_all_incoming(inner: &Arc<Inner>) -> Vec<DeviceId> {
    let peers: Vec<DeviceId> = lock(&inner.input)
        .incoming
        .drain()
        .map(|(p, _)| p)
        .collect();
    for p in &peers {
        end_and_tell(inner, *p, InputEndReason::StoppedByUser);
    }
    peers
}

/// Ends input sessions idle for [`IDLE_TIMEOUT`] (SR-25).
pub(crate) async fn maintenance(inner: Arc<Inner>) {
    let mut stop = inner.shutdown_rx();
    loop {
        tokio::select! {
            () = tokio::time::sleep(IDLE_SWEEP) => {}
            _ = stop.wait_for(|s| *s) => return,
        }
        let cutoff = Instant::now() - IDLE_TIMEOUT;
        let gone = lock(&inner.input).expire_idle(cutoff);
        for p in gone {
            tracing::info!(peer = %short(&p), "remote input session idle for 10 minutes; ended");
            end_and_tell(&inner, p, InputEndReason::IdleTimeout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(b: u8) -> DeviceId {
        format!("{b:02x}").repeat(32).parse().unwrap()
    }

    /// 5.12: at most 60 key frames per second per peer; the window resets after a second.
    #[test]
    fn key_cap_per_peer() {
        let mut st = InputState::default();
        let t0 = Instant::now();
        let a = peer(0xab);
        let b = peer(0xcd);
        assert!(!st.admit_key(&a, t0), "no session, no keys");
        for p in [a, b] {
            st.incoming.insert(
                p,
                IncomingSession {
                    last_frame: t0,
                    key_window: t0,
                    keys_in_window: 0,
                    dropped_keys: 0,
                },
            );
        }
        for _ in 0..60 {
            assert!(st.admit_key(&a, t0));
        }
        assert!(!st.admit_key(&a, t0 + Duration::from_millis(900)));
        assert!(st.admit_key(&b, t0), "another peer has its own budget");
        assert!(st.admit_key(&a, t0 + Duration::from_secs(1)));
    }

    /// SR-25: sessions idle for the timeout are expired, busy ones kept.
    #[test]
    fn idle_sessions_expire() {
        let mut st = InputState::default();
        let t0 = Instant::now();
        let a = peer(1);
        let b = peer(2);
        st.incoming.insert(
            a,
            IncomingSession {
                last_frame: t0,
                key_window: t0,
                keys_in_window: 0,
                dropped_keys: 0,
            },
        );
        st.incoming.insert(
            b,
            IncomingSession {
                last_frame: t0 + IDLE_TIMEOUT,
                key_window: t0,
                keys_in_window: 0,
                dropped_keys: 0,
            },
        );
        let gone = st.expire_idle(t0 + Duration::from_secs(1));
        assert_eq!(gone, vec![a]);
        assert!(st.session_open(&b));
        assert!(!st.session_open(&a));
    }

    /// Motion sums until taken, then starts over.
    #[test]
    fn pending_motion_sums() {
        let mut p = Pending::default();
        assert_eq!(p.take(), None);
        p.add(1.5, -2.0);
        p.add(0.5, 1.0);
        assert_eq!(p.take(), Some((2.0, -1.0)));
        assert_eq!(p.take(), None);
    }

    /// The reason codes on the wire and the old texts read back to the same reason, and
    /// the two controller-only reasons never go on the wire.
    #[test]
    fn end_reasons_round_trip_wire_and_legacy_text() {
        for r in InputEndReason::ON_THE_WIRE {
            let code = r.wire().unwrap();
            assert_eq!(InputEndReason::from_wire(code), Some(r));
            // The serde name is the wire value, so IPC and the wire use one vocabulary.
            assert_eq!(serde_json::to_value(r).unwrap(), code);
        }
        assert_eq!(InputEndReason::ConnectionLost.wire(), None);
        assert_eq!(InputEndReason::Other.wire(), None);
        assert_eq!(InputEndReason::from_wire("connection_lost"), None);
        assert_eq!(InputEndReason::from_wire("something_new"), None);
        // 0.10.0 texts.
        assert_eq!(
            InputEndReason::from_message("the desktop is locked"),
            InputEndReason::Locked
        );
        assert_eq!(
            InputEndReason::from_message("stopped on the desktop"),
            InputEndReason::StoppedByUser
        );
        assert_eq!(
            InputEndReason::from_message("no input for 10 minutes"),
            InputEndReason::IdleTimeout
        );
        assert_eq!(
            InputEndReason::from_message("whatever"),
            InputEndReason::Other
        );
        // An IPC client newer than the daemon, or the other way round.
        let back: InputEndReason = serde_json::from_str("\"future_reason\"").unwrap();
        assert_eq!(back, InputEndReason::Other);
    }

    #[test]
    fn capabilities_match_frame_kinds() {
        assert_eq!(
            capability_for(&Input::Presenter {
                action: crate::session::types::PresenterAction::Next
            }),
            &[Capability::InputPresenter]
        );
        assert_eq!(
            capability_for(&Input::Session {
                op: SessionOp::Start
            })
            .len(),
            3
        );
    }
}
