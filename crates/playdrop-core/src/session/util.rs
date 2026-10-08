//! Small encoding helpers shared by the session modules.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

/// Lowercase hex encoding.
pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 0x0f)]));
    }
    out
}

/// Decodes hex (either case) into exactly `N` bytes.
pub(crate) fn hex_decode<const N: usize>(s: &str) -> Option<[u8; N]> {
    fn nibble(c: u8) -> Option<u8> {
        match c {
            b'0'..=b'9' => Some(c - b'0'),
            b'a'..=b'f' => Some(c - b'a' + 10),
            b'A'..=b'F' => Some(c - b'A' + 10),
            _ => None,
        }
    }
    let raw = s.as_bytes();
    if raw.len() != N * 2 {
        return None;
    }
    let mut out = [0u8; N];
    for (i, pair) in raw.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

/// True if `s` is a Device ID: 64 lowercase hex characters (protocol.md section 2).
pub(crate) fn is_device_id(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Base64url without padding (protocol.md 6.2, 6.3, 8.2).
pub(crate) fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes unpadded base64url into exactly `N` bytes.
pub(crate) fn b64_decode<const N: usize>(s: &str) -> Option<[u8; N]> {
    // Reject oversized input before decoding so a hostile value costs nothing.
    if s.len() > N.div_ceil(3) * 4 {
        return None;
    }
    let v = URL_SAFE_NO_PAD.decode(s).ok()?;
    v.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trip() {
        let b = [0u8, 1, 0xab, 0xff];
        assert_eq!(hex_encode(&b), "0001abff");
        assert_eq!(hex_decode::<4>("0001ABff"), Some(b));
        assert_eq!(hex_decode::<4>("0001abf"), None);
        assert_eq!(hex_decode::<4>("0001abfg"), None);
    }

    #[test]
    fn b64_round_trip() {
        let b = [7u8; 16];
        let s = b64_encode(&b);
        assert_eq!(s.len(), 22);
        assert_eq!(b64_decode::<16>(&s), Some(b));
        assert_eq!(b64_decode::<16>("AAAA"), None);
        assert_eq!(b64_decode::<16>(&format!("{s}==")), None);
    }
}
