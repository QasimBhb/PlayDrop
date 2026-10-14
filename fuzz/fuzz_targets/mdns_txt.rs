//! mDNS TXT records, protocol.md 8.1 (threat-model.md SR-2, SR-4, SR-33, T39).
//!
//! Input: two bytes of SRV port (big-endian), then raw TXT RDATA as it arrives in a DNS
//! answer: length-prefixed `key=value` strings (RFC 6763 section 6).
//!
//! Decodes the RDATA with the same `mdns-sd` decoder the browser uses and parses it with
//! `Advert::from_txt`, the path every resolved `_playdrop._tcp` service takes. Parsed adverts
//! must meet the display rules and survive a TXT round trip unchanged.
#![no_main]

use libfuzzer_sys::fuzz_target;
use playdrop_core::discovery::advert::{MAX_NAME_LEN, MAX_TXT_LEN};
use playdrop_core::discovery::Advert;
use playdrop_core::fuzz_api::mdns_txt;

/// Encodes key/value pairs as TXT RDATA, the inverse of the decoder under test.
fn encode(pairs: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in pairs {
        let entry = format!("{k}={v}");
        let len = u8::try_from(entry.len()).expect("advert TXT entries fit one length byte");
        out.push(len);
        out.extend_from_slice(entry.as_bytes());
    }
    out
}

fn check_parsed(a: &Advert, port: u16) {
    assert!(a.v >= 1, "version 0 accepted");
    if let Some(f) = &a.add_device {
        assert!(
            !f.name.is_empty() && f.name.len() <= MAX_NAME_LEN,
            "name length"
        );
        assert!(
            !f.name.chars().any(|c| c.is_control()
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')),
            "unsanitised name {:?}",
            f.name
        );
        assert!(!f.device_type.is_empty() && f.device_type.bytes().all(|b| b.is_ascii_lowercase()));
        assert_eq!(f.port, port, "port must come from SRV");
        assert_ne!(f.port, 0);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((port, rdata)) = data.split_first_chunk::<2>() else {
        return;
    };
    let port = u16::from_be_bytes(*port);
    let Some(advert) = mdns_txt(rdata, port) else {
        return;
    };
    check_parsed(&advert, port);

    let txt = advert.to_txt();
    let rdata = encode(&txt);
    assert!(
        rdata.len() <= MAX_TXT_LEN,
        "re-encoded TXT is {} bytes",
        rdata.len()
    );
    assert_eq!(mdns_txt(&rdata, port), Some(advert), "TXT round trip");
});
