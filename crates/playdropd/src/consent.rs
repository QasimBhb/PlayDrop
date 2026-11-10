//! SR-24 consent for `send_local_clipboard` (`docs/ipc.md` 1.7): a clip a password manager
//! marked goes out only when the user agreed to that clip.
//!
//! When the daemon refuses a marked clip it remembers, in memory only, a BLAKE3 digest of
//! that clip (its kind, and for an image its MIME type, then its content) and a fresh one-use
//! token (128 random bits, hex) that it hands to the client.
//! The client sends the token back with `allow_sensitive: true` once the user agreed, and the
//! clip goes only if the clipboard still holds the same clip, the token is the remembered
//! one and it is younger than [`CONSENT_LIFETIME`]. A token presented with
//! `allow_sensitive` is used up whatever happens, also when the clipboard now holds an
//! unmarked clip. The token is random, not derived from the
//! clip, so nothing about a secret leaves the daemon; the digest never leaves it and is never
//! logged.
//!
//! *Choice:* one slot, not a table. There is one clipboard, so only the latest refusal can
//! still match what it holds: a newer refusal replaces the older one, and a client holding the
//! older token is simply asked again. One slot also bounds the memory with no pruning.

use std::time::{Duration, Instant};

use playdrop_ipc::{ClipSkip, ConsentToken};

use crate::clipboard::{ClipContent, LocalClip};

/// How long a consent token stays good. *Choice:* 60 s is long enough to read a prompt and
/// answer it, short enough that a forgotten prompt cannot send a later copy of the same secret.
pub const CONSENT_LIFETIME: Duration = Duration::from_secs(60);

/// The last refusal of a marked clip, waiting for the user's answer.
struct Refusal {
    token: ConsentToken,
    digest: blake3::Hash,
    at: Instant,
}

/// The one remembered refusal. `Debug` never shows the token or the digest (SR-32).
#[derive(Default)]
pub struct ConsentSlot {
    refusal: Option<Refusal>,
}

impl std::fmt::Debug for ConsentSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConsentSlot")
            .field("waiting", &self.refusal.is_some())
            .finish()
    }
}

/// Why a clip was not sent, with the token to send back when the user agrees. `Debug` shows
/// no token: [`ConsentToken`] redacts itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// Always [`ClipSkip::Sensitive`] today.
    pub skip: ClipSkip,
    /// The one-use consent token.
    pub consent: ConsentToken,
}

/// What a refusal remembers of a clip: the kind (and an image's MIME type) before the content,
/// so a text and an image can never share a digest.
fn digest(clip: &LocalClip) -> blake3::Hash {
    let mut h = blake3::Hasher::new();
    match &clip.content {
        ClipContent::Text(_) => {
            h.update(b"text\0");
        }
        ClipContent::Image { mime, .. } => {
            h.update(b"image\0");
            h.update(mime.as_bytes());
            h.update(b"\0");
        }
    }
    h.update(clip.content.hash().as_bytes());
    h.finalize()
}

impl ConsentSlot {
    /// The SR-24 gate for one manual send of `clip`.
    ///
    /// - A clip without the password-manager mark goes, marked sensitive when
    ///   `allow_sensitive` is set (a consented send always goes marked).
    /// - A marked clip without `allow_sensitive` is refused with a new token.
    /// - A marked clip with `allow_sensitive` and no token goes, marked: the caller asked
    ///   for exactly this (the CLI's `--sensitive`).
    /// - A marked clip with `allow_sensitive` and a token goes, marked, only if the token is
    ///   the remembered one, younger than [`CONSENT_LIFETIME`], and the clip has the digest
    ///   that was refused; otherwise the clip is refused again with a new token for what the
    ///   clipboard holds now.
    /// - A token presented with `allow_sensitive` is used up whatever the clip turns out to be,
    ///   marked or not.
    pub fn gate(
        &mut self,
        mut clip: LocalClip,
        allow_sensitive: bool,
        consent: Option<&ConsentToken>,
        now: Instant,
    ) -> Result<LocalClip, Refused> {
        let redeemed = match consent {
            Some(token) if allow_sensitive => self.redeem(token, &clip, now),
            _ => false,
        };
        if clip.sensitive {
            let agreed = allow_sensitive && (consent.is_none() || redeemed);
            if !agreed {
                return Err(self.refuse(&clip, now));
            }
        }
        if allow_sensitive {
            clip.sensitive = true;
        }
        Ok(clip)
    }

    /// Uses up the remembered token: true if `token` is it, still young, for this clip.
    fn redeem(&mut self, token: &ConsentToken, clip: &LocalClip, now: Instant) -> bool {
        let Some(r) = self.refusal.take() else {
            return false;
        };
        r.token == *token
            && now.saturating_duration_since(r.at) < CONSENT_LIFETIME
            && r.digest == digest(clip)
    }

    /// Remembers a refusal of `clip` and returns its new token.
    fn refuse(&mut self, clip: &LocalClip, now: Instant) -> Refused {
        // 16 random bytes as hex, from the same source as transfer IDs.
        let token = ConsentToken::new(playdrop_core::transfer::manifest::new_transfer_id());
        self.refusal = Some(Refusal {
            token: token.clone(),
            digest: digest(clip),
            at: now,
        });
        Refused {
            skip: ClipSkip::Sensitive,
            consent: token,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(text: &str) -> LocalClip {
        LocalClip::text(text, true)
    }

    #[test]
    fn plain_clips_pass_and_consent_marks_them() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let plain = slot
            .gate(LocalClip::text("hi", false), false, None, t)
            .unwrap();
        assert!(!plain.sensitive);
        // `--sensitive` with no hint still goes marked.
        let marked = slot
            .gate(LocalClip::text("hi", false), true, None, t)
            .unwrap();
        assert!(marked.sensitive);
    }

    #[test]
    fn a_token_is_accepted_once_for_the_refused_clip() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let refused = slot.gate(secret("pw"), false, None, t).unwrap_err();
        assert_eq!(refused.skip, ClipSkip::Sensitive);
        assert_eq!(refused.consent.as_str().len(), 32);
        assert!(refused
            .consent
            .as_str()
            .bytes()
            .all(|b| b.is_ascii_hexdigit()));
        let sent = slot
            .gate(secret("pw"), true, Some(&refused.consent), t)
            .unwrap();
        assert!(sent.sensitive);
        // Used up: the same token again is refused, with a fresh one.
        let again = slot
            .gate(secret("pw"), true, Some(&refused.consent), t)
            .unwrap_err();
        assert_ne!(again.consent, refused.consent);
    }

    #[test]
    fn an_expired_token_is_refused() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let refused = slot.gate(secret("pw"), false, None, t).unwrap_err();
        let late = t + CONSENT_LIFETIME;
        assert!(slot
            .gate(secret("pw"), true, Some(&refused.consent), late)
            .is_err());
    }

    #[test]
    fn a_changed_clip_is_refused_and_asked_again() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let first = slot.gate(secret("old"), false, None, t).unwrap_err();
        let second = slot
            .gate(secret("new"), true, Some(&first.consent), t)
            .unwrap_err();
        assert_ne!(second.consent, first.consent);
        // The new token is for the new clip and works for it.
        assert!(slot
            .gate(secret("new"), true, Some(&second.consent), t)
            .is_ok());
    }

    #[test]
    fn a_token_for_another_clip_or_an_unknown_token_is_refused() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let a = slot.gate(secret("a"), false, None, t).unwrap_err();
        // A later refusal replaces the slot: the older token no longer works, even for its
        // own clip.
        let _b = slot.gate(secret("b"), false, None, t).unwrap_err();
        assert!(slot.gate(secret("a"), true, Some(&a.consent), t).is_err());
        assert!(slot
            .gate(secret("a"), true, Some(&ConsentToken::new("00")), t)
            .is_err());
        // Without consent the token is irrelevant.
        assert!(slot.gate(secret("a"), false, Some(&a.consent), t).is_err());
    }

    #[test]
    fn debug_shows_neither_token_nor_digest() {
        let mut slot = ConsentSlot::default();
        let r = slot
            .gate(secret("pw"), false, None, Instant::now())
            .unwrap_err();
        let shown = format!("{slot:?}");
        assert!(!shown.contains(r.consent.as_str()), "{shown}");
        assert_eq!(shown, "ConsentSlot { waiting: true }");
        let shown = format!("{r:?}");
        assert!(!shown.contains(r.consent.as_str()), "{shown}");
    }

    #[test]
    fn a_presented_token_is_used_up_even_for_an_unmarked_clip() {
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let r = slot.gate(secret("pw"), false, None, t).unwrap_err();
        // The clipboard now holds something unmarked: it goes, marked, and the token is spent.
        let sent = slot
            .gate(LocalClip::text("hello", false), true, Some(&r.consent), t)
            .unwrap();
        assert!(sent.sensitive);
        assert!(slot.gate(secret("pw"), true, Some(&r.consent), t).is_err());
    }

    #[test]
    fn text_and_image_never_share_a_digest() {
        let text = LocalClip::text("abc", true);
        let image = LocalClip::image("image/png", b"abc".to_vec(), true);
        let other_mime = LocalClip::image("image/jpeg", b"abc".to_vec(), true);
        assert_ne!(digest(&text), digest(&image));
        assert_ne!(digest(&image), digest(&other_mime));
        // So a token for the text does not send the image.
        let mut slot = ConsentSlot::default();
        let t = Instant::now();
        let r = slot.gate(text, false, None, t).unwrap_err();
        assert!(slot.gate(image, true, Some(&r.consent), t).is_err());
    }
}
