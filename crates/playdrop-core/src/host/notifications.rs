//! Notification mirroring (protocol.md 5.16, SR-41 to SR-44): the phone sends `posted` and
//! `removed` with its `notificationsOut` grant, the desktop shows them with its
//! `notificationsIn` grant and answers a closed copy with `dismiss`.
//!
//! Nothing here logs an app name, a title, a text or a key at any level (SR-43): log lines
//! name the peer's Short ID and the op only.

use std::fmt;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use super::events::HostEvent;
use super::session::Session;
use super::Inner;
use crate::identity::DeviceId;
use crate::net::connection::short;
use crate::session::types::{
    notification_key_is_valid, truncate_bytes, Capability, ErrorCode, Frame, Notification,
    NotificationPosted, NOTIFICATION_APP_MAX, NOTIFICATION_TEXT_MAX, NOTIFICATION_TITLE_MAX,
};
use crate::session::util::hex_encode;
use crate::{Error, Result};

/// At most this many `posted` frames are handled per connection in any 60 s window; more
/// are dropped (protocol.md 5.16, SR-44).
pub const NOTIFICATIONS_POSTED_PER_MINUTE: usize = 30;

/// A notification mirrored from a peer, or one this host mirrors to a peer (protocol.md
/// 5.16). `key` is the opaque key from [`crate::host::Host::notification_key`]; `when_ms`
/// is when it was posted, in Unix ms. `Debug` hides every field (SR-43).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MirroredNotification {
    /// Opaque key, 32 lowercase hex characters.
    pub key: String,
    /// App name, at most 64 bytes on the wire.
    pub app: String,
    /// Title, at most 256 bytes on the wire; may be empty.
    pub title: String,
    /// Text, at most 1 KiB on the wire; may be empty.
    pub text: String,
    /// When it was posted, Unix ms.
    pub when_ms: u64,
}

impl fmt::Debug for MirroredNotification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MirroredNotification(redacted)")
    }
}

impl From<NotificationPosted> for MirroredNotification {
    fn from(p: NotificationPosted) -> Self {
        Self {
            key: p.key,
            app: p.app,
            title: p.title,
            text: p.text,
            when_ms: p.when,
        }
    }
}

impl MirroredNotification {
    /// The `posted` frame body, each field cut to its limit at a character boundary.
    fn into_wire(self) -> NotificationPosted {
        let cut = |s: String, max: usize| {
            if s.len() <= max {
                s
            } else {
                truncate_bytes(&s, max).to_owned()
            }
        };
        NotificationPosted {
            key: self.key,
            app: cut(self.app, NOTIFICATION_APP_MAX),
            title: cut(self.title, NOTIFICATION_TITLE_MAX),
            text: cut(self.text, NOTIFICATION_TEXT_MAX),
            when: self.when_ms,
        }
    }
}

/// The per-process salt for [`notification_key`]: random, never stored or sent, so a key
/// cannot be linked to the platform key it came from.
fn salt() -> &'static [u8; 32] {
    static SALT: OnceLock<[u8; 32]> = OnceLock::new();
    SALT.get_or_init(|| {
        let mut s = [0u8; 32];
        if crate::identity::fill_random(&mut s).is_err() {
            // The system random source failed. A salt from the clock and the process
            // still keeps keys unlinkable across runs; it only has to differ per process.
            tracing::warn!("system random source failed; notification key salt from the clock");
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let mut h = blake3::Hasher::new_derive_key("playdrop notification key salt");
            h.update(&nanos.to_le_bytes());
            h.update(&std::process::id().to_le_bytes());
            s = *h.finalize().as_bytes();
        }
        s
    })
}

/// The opaque `key` for a platform notification key (protocol.md 5.16): the first 128 bits
/// of BLAKE3 keyed by the per-process salt over `raw`, as 32 lowercase hex characters.
pub(crate) fn notification_key(raw: &str) -> String {
    let h = blake3::keyed_hash(salt(), raw.as_bytes());
    hex_encode(&h.as_bytes()[..16])
}

/// Which of this host's grants a send needs.
#[derive(Clone, Copy)]
enum Need {
    /// `notificationsOut`: `posted`.
    Out,
    /// `notificationsIn`: `dismiss`.
    In,
    /// None: `removed` carries no content, and must still go after the grant was turned
    /// off so the peer can close its copies (SR-42).
    Nothing,
}

/// The live session with `peer` that accepts notifications, after the grant check.
fn session_for(inner: &Inner, peer: &DeviceId, need: Need) -> Result<std::sync::Arc<Session>> {
    let pin = inner.pins.get(peer).ok_or(Error::NotPaired)?;
    match need {
        Need::Out if !pin.grants.notifications_out => {
            return Err(Error::Forbidden("notificationsOut grant is off"));
        }
        Need::In if !pin.grants.notifications_in => {
            return Err(Error::Forbidden("notificationsIn grant is off"));
        }
        _ => {}
    }
    let s = inner.registry.session(peer).ok_or(Error::NotFound)?;
    if !s.peer_accepts(&Capability::Notifications) {
        return Err(Error::Forbidden("the device does not accept notifications"));
    }
    Ok(s)
}

fn check_key(key: &str) -> Result<()> {
    if notification_key_is_valid(key) {
        Ok(())
    } else {
        Err(Error::BadRequest(
            "notification key must be 32 lowercase hex characters".into(),
        ))
    }
}

/// Sends `posted` to `peer` (5.16). Needs this host's `notificationsOut` grant for it.
pub(crate) fn send_posted(inner: &Inner, peer: &DeviceId, n: MirroredNotification) -> Result<()> {
    check_key(&n.key)?;
    let s = session_for(inner, peer, Need::Out)?;
    s.send(Frame::Notification(Notification::Posted(n.into_wire())));
    Ok(())
}

/// Sends `removed` to `peer` (5.16). Needs no grant: it carries no content, and a phone
/// that just turned `notificationsOut` off uses it to clear the peer's copies (SR-42).
pub(crate) fn send_removed(inner: &Inner, peer: &DeviceId, key: String) -> Result<()> {
    check_key(&key)?;
    let s = session_for(inner, peer, Need::Nothing)?;
    s.send(Frame::Notification(Notification::Removed { key }));
    Ok(())
}

/// Sends `dismiss` to `peer` (5.16). Needs this host's `notificationsIn` grant for it.
pub(crate) fn send_dismiss(inner: &Inner, peer: &DeviceId, key: String) -> Result<()> {
    check_key(&key)?;
    let s = session_for(inner, peer, Need::In)?;
    s.send(Frame::Notification(Notification::Dismiss { key }));
    Ok(())
}

/// A `notification` frame that passed the grant check (5.3). Bad keys are dropped,
/// over-limit fields are dropped with `error` `bad_request`, and `posted` frames past
/// [`NOTIFICATIONS_POSTED_PER_MINUTE`] on this connection are dropped (SR-44).
pub(crate) fn on_frame(inner: &Inner, sess: &Session, seq: u64, n: Notification) {
    let peer = sess.peer();
    if matches!(n, Notification::Posted(_)) && !sess.allow_notification_posted() {
        tracing::debug!(peer = %short(&peer), "notification over the rate limit; dropped");
        return;
    }
    if !n.key_is_valid() {
        tracing::debug!(peer = %short(&peer), "notification with a malformed key; dropped");
        return;
    }
    if !n.within_limits() {
        tracing::debug!(peer = %short(&peer), "notification over its size limits; dropped");
        sess.send_raw(
            Frame::error(
                ErrorCode::BadRequest,
                Some("notification field over its limit"),
            ),
            Some(seq),
        );
        return;
    }
    let peer = peer.to_hex();
    match n {
        Notification::Posted(p) => inner.emit(HostEvent::NotificationPosted {
            peer,
            notification: p.into(),
        }),
        Notification::Removed { key } => inner.emit(HostEvent::NotificationRemoved { peer, key }),
        Notification::Dismiss { key } => inner.emit(HostEvent::NotificationDismiss { peer, key }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_salted_and_32_hex() {
        let a = notification_key("0|com.signal|42|tag|10123");
        assert_eq!(a, notification_key("0|com.signal|42|tag|10123"));
        assert!(notification_key_is_valid(&a), "{a}");
        assert_ne!(a, notification_key("0|com.signal|43|tag|10123"));
        // Salted: not the plain hash of the raw key.
        let plain = hex_encode(&blake3::hash(b"0|com.signal|42|tag|10123").as_bytes()[..16]);
        assert_ne!(a, plain);
        assert!(notification_key_is_valid(&notification_key("")));
    }

    #[test]
    fn wire_form_cuts_fields_at_char_boundaries() {
        let n = MirroredNotification {
            key: "0".repeat(32),
            app: "\u{e9}".repeat(40),
            title: "\u{1f600}".repeat(100),
            text: "a\u{e9}".repeat(1000),
            when_ms: 7,
        };
        let w = n.into_wire();
        assert_eq!(w.app.len(), 64);
        assert_eq!(w.title.len(), 256);
        assert!(w.text.len() <= NOTIFICATION_TEXT_MAX && w.text.len() >= 1022);
        assert!(Notification::Posted(w.clone()).within_limits());
        assert_eq!(w.when, 7);
        let short = MirroredNotification {
            key: "1".repeat(32),
            app: "A".into(),
            title: String::new(),
            text: "t".into(),
            when_ms: 1,
        }
        .into_wire();
        assert_eq!((short.app.as_str(), short.text.as_str()), ("A", "t"));
    }

    #[test]
    fn debug_is_redacted() {
        let n = MirroredNotification {
            key: "ab".repeat(16),
            app: "SecretApp".into(),
            title: "SecretTitle".into(),
            text: "SecretText".into(),
            when_ms: 1,
        };
        let d = format!("{n:?}");
        assert!(!d.contains("Secret") && !d.contains("abab"), "{d}");
    }
}
