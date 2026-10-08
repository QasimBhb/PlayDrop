//! Advert data model, `docs/protocol.md` section 8.1 (SR-2, SR-4).
//!
//! One [`Advert`] type covers both carriers: the beacon JSON object and the mDNS TXT record.
//! Parsing is total: any malformed, oversized or out-of-range input yields `None`.

use serde::{Deserialize, Serialize};

use super::tag::{NONCE_LEN, TAG_LEN};
use crate::constants::FORBIDDEN_PORTS;

/// Largest beacon datagram accepted or sent, in bytes (protocol.md 8.1, SR-2).
pub const MAX_BEACON_LEN: usize = 512;

/// Largest TXT record accepted, in encoded bytes (protocol.md 8.1, SR-2).
pub const MAX_TXT_LEN: usize = 400;

/// Largest sanitised device name, in UTF-8 bytes (protocol.md 8.1).
pub const MAX_NAME_LEN: usize = 63;

/// Largest device `type` token, in bytes. *Choice:* the known types (protocol.md 5.4) are at
/// most 7 bytes; 16 leaves room for new ones while keeping the field a short ASCII token.
pub const MAX_TYPE_LEN: usize = 16;

/// A discovery advert, as carried by a beacon or an mDNS TXT record (protocol.md 8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Advert {
    /// Highest protocol version the sender speaks; a hint only, never trusted (SR-12).
    pub v: u32,
    /// Fresh random nonce `n` (protocol.md 8.1).
    pub n: [u8; NONCE_LEN],
    /// Announce tag `t` over `n` (protocol.md 8.2).
    pub t: [u8; TAG_LEN],
    /// Present only while the sender's "Add device" window is open (protocol.md 8.1, SR-4).
    pub add_device: Option<AddDeviceFields>,
}

/// Fields an advert carries only during an open "Add device" window (protocol.md 8.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddDeviceFields {
    /// Sanitised display name, at most [`MAX_NAME_LEN`] bytes (protocol.md 8.3 rule 4).
    pub name: String,
    /// Device type token such as `phone` or `desktop` (protocol.md 5.4).
    pub device_type: String,
    /// Listening TCP port: the beacon `port` key, or the mDNS SRV port.
    pub port: u16,
}

/// Wire shape of the beacon JSON object (protocol.md 8.1).
#[derive(Serialize, Deserialize)]
struct BeaconJson<'a> {
    t: &'a str,
    v: u32,
    n: &'a str,
    #[serde(skip_serializing_if = "Option::is_none", borrow)]
    name: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    device_type: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    port: Option<u16>,
}

impl Advert {
    /// Parses one beacon datagram (protocol.md 8.1). Returns `None` for anything malformed,
    /// including datagrams over [`MAX_BEACON_LEN`], which are rejected before any parsing.
    ///
    /// Unknown keys are ignored so later versions can add fields. The "Add device" keys
    /// `name`, `type` and `port` must appear all together or not at all.
    pub fn from_beacon(datagram: &[u8]) -> Option<Self> {
        if datagram.len() > MAX_BEACON_LEN {
            return None;
        }
        let raw: BeaconJson<'_> = serde_json::from_slice(datagram).ok()?;
        let n = decode_hex16(raw.n)?;
        let t = decode_hex16(raw.t)?;
        let v = check_version(raw.v)?;
        let add_device = match (raw.name, raw.device_type, raw.port) {
            (None, None, None) => None,
            (Some(name), Some(device_type), Some(port)) => {
                Some(AddDeviceFields::parse(&name, device_type, port)?)
            }
            _ => return None,
        };
        Some(Self {
            v,
            n,
            t,
            add_device,
        })
    }

    /// Encodes the advert as one beacon datagram (protocol.md 8.1). Returns `None` if the
    /// result would exceed [`MAX_BEACON_LEN`], which cannot happen for a sanitised name.
    pub fn to_beacon(&self) -> Option<Vec<u8>> {
        let n = hex(&self.n);
        let t = hex(&self.t);
        let raw = BeaconJson {
            t: &t,
            v: self.v,
            n: &n,
            name: self
                .add_device
                .as_ref()
                .map(|a| std::borrow::Cow::Borrowed(a.name.as_str())),
            device_type: self.add_device.as_ref().map(|a| a.device_type.as_str()),
            port: self.add_device.as_ref().map(|a| a.port),
        };
        let out = serde_json::to_vec(&raw).ok()?;
        (out.len() <= MAX_BEACON_LEN).then_some(out)
    }

    /// Parses an mDNS TXT record (protocol.md 8.1). `entries` are the record's key/value
    /// strings in order; `srv_port` is the port from the SRV record.
    ///
    /// Records whose encoded size exceeds [`MAX_TXT_LEN`] are rejected before any field is
    /// read. Keys compare case-insensitively and only the first occurrence of a key counts
    /// (RFC 6763 section 6.4). `name` and `type` must appear together or not at all.
    pub fn from_txt<'a, I>(entries: I, srv_port: u16) -> Option<Self>
    where
        I: IntoIterator<Item = (&'a str, Option<&'a [u8]>)>,
    {
        let entries: Vec<(&str, Option<&[u8]>)> = entries.into_iter().collect();
        let encoded: usize = entries
            .iter()
            .map(|(k, v)| 1 + k.len() + v.map_or(0, |v| v.len() + 1))
            .sum();
        if encoded > MAX_TXT_LEN {
            return None;
        }
        let get = |key: &str| -> Option<&[u8]> {
            entries
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))
                .and_then(|(_, v)| *v)
        };
        let text = |key: &str| get(key).and_then(|v| std::str::from_utf8(v).ok());

        let v = check_version(text("v")?.parse().ok()?)?;
        let n = decode_hex16(text("n")?)?;
        let t = decode_hex16(text("t")?)?;
        let add_device = match (get("name"), get("type")) {
            (None, None) => None,
            (Some(name), Some(device_type)) => Some(AddDeviceFields::parse(
                std::str::from_utf8(name).ok()?,
                std::str::from_utf8(device_type).ok()?,
                srv_port,
            )?),
            _ => return None,
        };
        Some(Self {
            v,
            n,
            t,
            add_device,
        })
    }

    /// Encodes the advert as mDNS TXT key/value pairs (protocol.md 8.1). The port is not a
    /// TXT key; it travels in the SRV record.
    pub fn to_txt(&self) -> Vec<(String, String)> {
        let mut out = vec![
            ("v".to_owned(), self.v.to_string()),
            ("n".to_owned(), hex(&self.n)),
            ("t".to_owned(), hex(&self.t)),
        ];
        if let Some(a) = &self.add_device {
            out.push(("name".to_owned(), a.name.clone()));
            out.push(("type".to_owned(), a.device_type.clone()));
        }
        out
    }
}

impl AddDeviceFields {
    /// Builds the fields from untrusted input: sanitises the name and validates the type and
    /// port (protocol.md 8.1, 8.3 rule 4). Returns `None` if anything is unusable.
    pub fn parse(name: &str, device_type: &str, port: u16) -> Option<Self> {
        let name = sanitize_name(name);
        if name.is_empty() || !valid_type(device_type) || !valid_port(port) {
            return None;
        }
        Some(Self {
            name,
            device_type: device_type.to_owned(),
            port,
        })
    }
}

/// Makes an untrusted name safe to display (protocol.md 8.1 and 8.3 rule 4, SR-8).
///
/// Strips C0 and C1 controls (including DEL) and the bidi embeddings, overrides and isolates
/// U+202A to U+202E and U+2066 to U+2069, then cuts the result to at most [`MAX_NAME_LEN`]
/// bytes without splitting a character.
pub fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_NAME_LEN));
    for c in raw.chars().filter(|c| !is_stripped(*c)) {
        if out.len() + c.len_utf8() > MAX_NAME_LEN {
            break;
        }
        out.push(c);
    }
    out
}

fn is_stripped(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A `type` token is 1 to [`MAX_TYPE_LEN`] lowercase ASCII letters (protocol.md 5.4 lists
/// `desktop`, `laptop`, `phone`, `tablet`, `tv`; unknown tokens are kept for later versions).
fn valid_type(t: &str) -> bool {
    (1..=MAX_TYPE_LEN).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_lowercase())
}

/// A usable advertised port: non-zero and outside the ports the protocol forbids
/// (protocol.md section 1).
pub(crate) fn valid_port(port: u16) -> bool {
    port != 0 && !FORBIDDEN_PORTS.iter().any(|r| r.contains(&port))
}

/// Version 0 does not exist; any other value is accepted as a hint (SR-12).
fn check_version(v: u32) -> Option<u32> {
    (v >= 1).then_some(v)
}

/// Lowercase hex of `bytes` (protocol.md 8.1 encodes `n` and `t` this way).
pub(crate) fn hex(bytes: &[u8]) -> String {
    data_encoding::HEXLOWER.encode(bytes)
}

/// Decodes exactly 32 lowercase hex characters into 16 bytes. Uppercase is rejected, since
/// protocol.md 8.1 fixes lowercase and a strict parser leaves less room for confusion.
pub(crate) fn decode_hex16(s: &str) -> Option<[u8; 16]> {
    fn nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    if bytes.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, pair) in bytes.as_chunks::<2>().0.iter().enumerate() {
        out[i] = (nibble(pair[0])? << 4) | nibble(pair[1])?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn private() -> Advert {
        Advert {
            v: 1,
            n: [0x4c; 16],
            t: [0x9a; 16],
            add_device: None,
        }
    }

    fn named() -> Advert {
        Advert {
            v: 1,
            n: [1; 16],
            t: [2; 16],
            add_device: Some(AddDeviceFields {
                name: "Pop desktop".into(),
                device_type: "desktop".into(),
                port: 42187,
            }),
        }
    }

    fn txt_ref(pairs: &[(String, String)]) -> Vec<(&str, Option<&[u8]>)> {
        pairs
            .iter()
            .map(|(k, v)| (k.as_str(), Some(v.as_bytes())))
            .collect()
    }

    #[test]
    fn beacon_round_trip() {
        for a in [private(), named()] {
            let wire = a.to_beacon().unwrap();
            assert!(wire.len() <= MAX_BEACON_LEN);
            assert_eq!(Advert::from_beacon(&wire), Some(a));
        }
    }

    #[test]
    fn beacon_wire_shape_matches_spec() {
        let wire = String::from_utf8(private().to_beacon().unwrap()).unwrap();
        assert_eq!(
            wire,
            format!(
                r#"{{"t":"{}","v":1,"n":"{}"}}"#,
                "9a".repeat(16),
                "4c".repeat(16)
            )
        );
        let wire = String::from_utf8(named().to_beacon().unwrap()).unwrap();
        assert!(
            wire.ends_with(r#""name":"Pop desktop","type":"desktop","port":42187}"#),
            "{wire}"
        );
    }

    #[test]
    fn beacon_parses_spec_example_shape_and_ignores_unknown_keys() {
        let n = "4c".repeat(16);
        let t = "9a".repeat(16);
        let wire = format!(r#"{{"t":"{t}","v":2,"n":"{n}","future":[1,2]}}"#);
        let a = Advert::from_beacon(wire.as_bytes()).unwrap();
        assert_eq!(a.v, 2);
        assert!(a.add_device.is_none());
    }

    #[test]
    fn txt_round_trip() {
        for a in [private(), named()] {
            let pairs = a.to_txt();
            let port = a.add_device.as_ref().map_or(42187, |f| f.port);
            assert_eq!(Advert::from_txt(txt_ref(&pairs), port), Some(a));
        }
    }

    #[test]
    fn txt_private_carries_only_v_n_t() {
        let keys: Vec<String> = private().to_txt().into_iter().map(|(k, _)| k).collect();
        assert_eq!(keys, ["v", "n", "t"]);
    }

    #[test]
    fn malformed_beacons_return_none() {
        let n = "4c".repeat(16);
        let t = "9a".repeat(16);
        let cases: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"not json".to_vec(),
            b"[]".to_vec(),
            b"{}".to_vec(),
            vec![0xff, 0xfe, 0x00],
            format!(r#"{{"t":"{t}","v":1}}"#).into_bytes(),
            format!(r#"{{"t":"{t}","v":0,"n":"{n}"}}"#).into_bytes(),
            format!(r#"{{"t":"{t}","v":-1,"n":"{n}"}}"#).into_bytes(),
            format!(r#"{{"t":"{t}","v":"1","n":"{n}"}}"#).into_bytes(),
            format!(r#"{{"t":"{}","v":1,"n":"{n}"}}"#, "9A".repeat(16)).into_bytes(),
            format!(r#"{{"t":"{}","v":1,"n":"{n}"}}"#, "9a".repeat(15)).into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{}zz"}}"#, "4c".repeat(15)).into_bytes(),
            format!(r#"{{"t":"{t}","t":"{t}","v":1,"n":"{n}"}}"#).into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x"}}"#).into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x","type":"phone","port":0}}"#)
                .into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x","type":"phone","port":1716}}"#)
                .into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x","type":"phone","port":53317}}"#)
                .into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x","type":"phone","port":70000}}"#)
                .into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"x","type":"Phone!","port":42187}}"#)
                .into_bytes(),
            format!(r#"{{"t":"{t}","v":1,"n":"{n}","name":"\u202e","type":"phone","port":42187}}"#)
                .into_bytes(),
        ];
        for c in cases {
            assert!(
                Advert::from_beacon(&c).is_none(),
                "{}",
                String::from_utf8_lossy(&c)
            );
        }
    }

    #[test]
    fn oversized_beacon_is_dropped_even_if_valid_json() {
        let pad = "x".repeat(MAX_BEACON_LEN);
        let wire = format!(
            r#"{{"t":"{}","v":1,"n":"{}","pad":"{pad}"}}"#,
            "9a".repeat(16),
            "4c".repeat(16)
        );
        assert!(Advert::from_beacon(wire.as_bytes()).is_none());
    }

    #[test]
    fn malformed_txt_returns_none() {
        let n = "4c".repeat(16);
        let t = "9a".repeat(16);
        let ok = |extra: &[(&str, Option<&[u8]>)]| {
            let mut e: Vec<(&str, Option<&[u8]>)> = vec![
                ("v", Some(b"1")),
                ("n", Some(n.as_bytes())),
                ("t", Some(t.as_bytes())),
            ];
            e.extend_from_slice(extra);
            Advert::from_txt(e, 42187)
        };
        assert!(ok(&[]).is_some());
        assert!(ok(&[("name", Some(b"x"))]).is_none());
        assert!(ok(&[("name", Some(b"x")), ("type", None)]).is_none());
        assert!(ok(&[("name", Some(&[0xff, 0xfe])), ("type", Some(b"phone"))]).is_none());
        assert!(
            Advert::from_txt(vec![("v", Some(&b"1"[..])), ("n", Some(n.as_bytes()))], 1).is_none()
        );
        assert!(Advert::from_txt(
            vec![
                ("v", None),
                ("n", Some(n.as_bytes())),
                ("t", Some(t.as_bytes()))
            ],
            1
        )
        .is_none());
        assert!(Advert::from_txt(
            vec![
                ("v", Some(&b"x"[..])),
                ("n", Some(n.as_bytes())),
                ("t", Some(t.as_bytes()))
            ],
            1
        )
        .is_none());
        // A named record on a forbidden SRV port is unusable.
        let named = vec![
            ("v", Some(&b"1"[..])),
            ("n", Some(n.as_bytes())),
            ("t", Some(t.as_bytes())),
            ("name", Some(&b"a"[..])),
            ("type", Some(&b"phone"[..])),
        ];
        assert!(Advert::from_txt(named, 1714).is_none());
    }

    #[test]
    fn txt_first_key_wins_and_keys_ignore_case() {
        let n = "4c".repeat(16);
        let t = "9a".repeat(16);
        let other = "00".repeat(16);
        let e = vec![
            ("V", Some(&b"1"[..])),
            ("N", Some(n.as_bytes())),
            ("n", Some(other.as_bytes())),
            ("t", Some(t.as_bytes())),
        ];
        assert_eq!(Advert::from_txt(e, 1).unwrap().n, [0x4c; 16]);
    }

    #[test]
    fn oversized_txt_is_dropped() {
        let n = "4c".repeat(16);
        let t = "9a".repeat(16);
        let pad = vec![b'x'; MAX_TXT_LEN];
        let e = vec![
            ("v", Some(&b"1"[..])),
            ("n", Some(n.as_bytes())),
            ("t", Some(t.as_bytes())),
            ("pad", Some(&pad[..])),
        ];
        assert!(Advert::from_txt(e, 1).is_none());
    }

    #[test]
    fn name_sanitisation() {
        assert_eq!(sanitize_name("Pixel 9"), "Pixel 9");
        assert_eq!(sanitize_name("evil\u{202E}gpj.exe"), "evilgpj.exe");
        assert_eq!(sanitize_name("\u{2066}a\u{2069}\u{202A}b\u{202C}"), "ab");
        assert_eq!(sanitize_name("a\0b\nc\u{7f}d\u{85}e\u{9b}f"), "abcdef");
        // Bidi marks outside the listed ranges are not overrides and stay.
        assert_eq!(sanitize_name("a\u{200F}b"), "a\u{200F}b");
        let long = "é".repeat(40); // 80 bytes, 2 per char
        let cut = sanitize_name(&long);
        assert_eq!(cut.len(), 62);
        assert!(cut.chars().all(|c| c == 'é'));
        assert_eq!(sanitize_name(&"a".repeat(100)).len(), MAX_NAME_LEN);
        assert_eq!(sanitize_name(&"😀".repeat(20)).len(), 60);
    }

    #[test]
    fn hex_helpers() {
        assert_eq!(decode_hex16(&hex(&[0xab; 16])), Some([0xab; 16]));
        assert!(decode_hex16("").is_none());
        assert!(decode_hex16(&"é".repeat(16)).is_none());
    }
}
