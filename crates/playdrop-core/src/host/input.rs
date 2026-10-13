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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

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

/// Sends one input frame to `peer`. Needs a session ([`Error::NotFound`]) and the matching
/// `input.*` capability on the peer ([`Error::Forbidden`], the message says which). Moves
/// and scrolls are coalesced per [`MOVE_EVERY`]; everything else goes at once after
/// flushing a pending move.
pub(crate) fn send_input(inner: &Arc<Inner>, peer: &DeviceId, input: Input) -> Result<()> {
    let sess = inner.registry.session(peer).ok_or(Error::NotFound)?;
    let needed = capability_for(&input);
    if !needed.iter().any(|c| sess.peer_accepts(c)) {
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
            if matches!(
                other,
                Input::Session {
                    op: SessionOp::Stop
                }
            ) {
                lock(&inner.input).outgoing.remove(peer);
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

/// An `input` frame from `peer` that passed the grant check (SR-13, SR-14). Applies the
/// 5.12 and SR-25 target-side rules and emits [`HostEvent::Input`] for the host to apply.
pub(crate) fn on_frame(inner: &Arc<Inner>, peer: DeviceId, input: Input) {
    let now = Instant::now();
    if inner.lock_state.is_locked() {
        // SR-25: no input into a locked desktop, fail closed. An open session ends so the
        // host releases keys and hides its indicator; the peer must start again.
        let was_open = lock(&inner.input).incoming.remove(&peer).is_some();
        tracing::debug!(peer = %short(&peer), "input while locked; dropped");
        if was_open {
            end_and_tell(inner, peer, "the desktop is locked");
        }
        return;
    }
    match &input {
        Input::Session {
            op: SessionOp::Start,
        } => {
            let fresh = lock(&inner.input)
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
            let admitted = {
                let mut st = lock(&inner.input);
                let Some(s) = st.incoming.get_mut(&peer) else {
                    tracing::debug!(peer = %short(&peer), "input outside a session; dropped");
                    return;
                };
                s.last_frame = now;
                if matches!(input, Input::Key(_)) {
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
                }
            };
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
fn end_and_tell(inner: &Inner, peer: DeviceId, reason: &str) {
    emit_stop(inner, peer);
    if let Some(s) = inner.registry.session(&peer) {
        s.send_raw(
            Frame::error(ErrorCode::InputSessionEnded, Some(reason)),
            None,
        );
    }
}

/// The target of a session we control ended it (`error` `input_session_ended`): drop
/// pending motion and tell the UI why.
pub(crate) fn on_ended_by_target(inner: &Arc<Inner>, peer: DeviceId, reason: Option<String>) {
    lock(&inner.input).outgoing.remove(&peer);
    inner.emit(HostEvent::InputSessionEnded {
        peer: peer.to_hex(),
        reason: reason.unwrap_or_else(|| "ended by the device".to_owned()),
    });
}

/// The session with `peer` ended: an open input session from it ends too (the host gets a
/// final `stop`, T31), and pending motion to it is dropped.
pub(crate) fn on_session_end(inner: &Arc<Inner>, peer: DeviceId) {
    let was_open = {
        let mut st = lock(&inner.input);
        st.outgoing.remove(&peer);
        st.incoming.remove(&peer).is_some()
    };
    if was_open {
        tracing::info!(peer = %short(&peer), "remote input session ended with the connection");
        emit_stop(inner, peer);
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
        end_and_tell(inner, *p, "stopped on the desktop");
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
            end_and_tell(&inner, p, "no input for 10 minutes");
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
