//! Announce tags, `docs/protocol.md` section 8.2 (SR-5).
//!
//! A device proves it holds announce key K without revealing who it is:
//! `t = first 16 bytes of HMAC-SHA256(K, "pd-announce-v1" || n)`, with `n` the 16 raw nonce
//! bytes. A receiver recomputes `t` with each pinned peer's K and dials only on a match (SR-1).

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

/// Device ID as 64 lowercase hex characters (protocol.md section 2).
///
/// A plain `String` until the identity module's `DeviceId` type lands; the integrator swaps it.
pub type DeviceIdString = String;

/// Length in bytes of an announce key K (protocol.md 8.2).
pub const ANNOUNCE_KEY_LEN: usize = 32;

/// Length in bytes of an advert nonce `n` (protocol.md 8.1).
pub const NONCE_LEN: usize = 16;

/// Length in bytes of an announce tag `t` (protocol.md 8.2).
pub const TAG_LEN: usize = 16;

/// Domain separation prefix fed to the HMAC before the nonce (protocol.md 8.2).
pub const TAG_DOMAIN: &[u8] = b"pd-announce-v1";

type HmacSha256 = Hmac<Sha256>;

/// Source of the announce keys of every pinned peer (protocol.md 8.2, SR-1, SR-5).
///
/// Implemented by the pin store. Discovery calls it for each advert that is not in the cache,
/// so it should be cheap; a few pins is the expected case.
pub trait PeerKeys: Send + Sync {
    /// Returns `(device_id, K)` for every pinned peer, using the highest `kid` received.
    fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; ANNOUNCE_KEY_LEN])>;

    /// Stored listening port of a pinned peer, the only port a verified advert may dial
    /// (protocol.md 8.3 rule 1). Defaults to the protocol port.
    fn stored_port(&self, device_id: &str) -> u16 {
        let _ = device_id;
        crate::constants::TCP_PORT
    }

    /// A counter that changes whenever a pin or announce key is added, removed or rotated.
    ///
    /// Discovery drops its cached tag outcomes when this changes, so a new pin is matched at
    /// once and a removed one stops matching at once (SR-5). The default `0` means "never
    /// changes"; in that case call `Discovery::peers_changed` instead.
    fn generation(&self) -> u64 {
        0
    }
}

/// Computes the announce tag for `nonce` under `key` (protocol.md 8.2).
pub fn tag(key: &[u8; ANNOUNCE_KEY_LEN], nonce: &[u8; NONCE_LEN]) -> [u8; TAG_LEN] {
    let full = keyed_mac(key, nonce).finalize().into_bytes();
    let mut out = [0u8; TAG_LEN];
    out.copy_from_slice(&full[..TAG_LEN]);
    out
}

/// Checks `tag` against one key in constant time (protocol.md 8.2).
pub fn verify_one(
    key: &[u8; ANNOUNCE_KEY_LEN],
    nonce: &[u8; NONCE_LEN],
    tag: &[u8; TAG_LEN],
) -> bool {
    keyed_mac(key, nonce).verify_truncated_left(tag).is_ok()
}

/// Finds which pinned peer, if any, produced `tag` for `nonce` (protocol.md 8.2, SR-1).
///
/// Every candidate is checked, with no early exit, and each comparison is constant time, so
/// the running time does not depend on where or whether a match is found.
pub fn verify<'a, I>(candidates: I, nonce: &[u8; NONCE_LEN], tag: &[u8; TAG_LEN]) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a (DeviceIdString, [u8; ANNOUNCE_KEY_LEN])>,
{
    let mut found: Option<&'a str> = None;
    for (id, key) in candidates {
        let ok = verify_one(key, nonce, tag);
        if ok && found.is_none() {
            found = Some(id.as_str());
        }
    }
    found
}

fn keyed_mac(key: &[u8; ANNOUNCE_KEY_LEN], nonce: &[u8; NONCE_LEN]) -> HmacSha256 {
    // HMAC accepts keys of any length, so a 32-byte key can never fail here.
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
    mac.update(TAG_DOMAIN);
    mac.update(nonce);
    mac
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex16(s: &str) -> [u8; 16] {
        let mut out = [0u8; 16];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    fn seq<const N: usize>() -> [u8; N] {
        let mut out = [0u8; N];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::try_from(i).unwrap();
        }
        out
    }

    // Vectors computed independently with Python's `hmac` and `hashlib` modules.
    #[test]
    fn tag_vectors() {
        assert_eq!(
            tag(&seq::<32>(), &seq::<16>()),
            hex16("d91cfa6504cefb87f2fcda04517ca3e8")
        );
        assert_eq!(
            tag(&[0x42; 32], &[0; 16]),
            hex16("6f892f4d7f3844095266a96bed6adbfd")
        );
    }

    #[test]
    fn verify_finds_the_matching_peer() {
        let nonce = [7u8; 16];
        let keys = vec![
            ("a".repeat(64), [1u8; 32]),
            ("b".repeat(64), [2u8; 32]),
            ("c".repeat(64), [3u8; 32]),
        ];
        let t = tag(&[2u8; 32], &nonce);
        assert_eq!(verify(&keys, &nonce, &t), Some("b".repeat(64).as_str()));
        assert!(verify_one(&[2u8; 32], &nonce, &t));
        assert!(!verify_one(&[1u8; 32], &nonce, &t));
    }

    #[test]
    fn verify_rejects_wrong_tag_nonce_or_empty_set() {
        let nonce = [7u8; 16];
        let keys = vec![("a".repeat(64), [1u8; 32])];
        let mut t = tag(&[1u8; 32], &nonce);
        assert!(verify(&keys, &[8u8; 16], &t).is_none());
        t[15] ^= 1;
        assert!(verify(&keys, &nonce, &t).is_none());
        assert!(verify(&[], &nonce, &t).is_none());
    }
}
