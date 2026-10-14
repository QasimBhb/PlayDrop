//! QR pairing codes, protocol.md 6.3 (threat-model.md SR-9, SR-33).
//!
//! Input: the text a camera hands over after scanning, which anyone can print and show.
//! `QrPayload::parse` must refuse anything malformed, and whatever it accepts must carry a
//! real Device ID, fingerprint, address and secret and survive `to_uri` unchanged.
#![no_main]

use libfuzzer_sys::fuzz_target;
use playdrop_core::session::pairing::{QR_MAX_ADDRS, QR_URI_MAX_LEN};
use playdrop_core::session::QrPayload;

fn lower_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    let Ok(q) = QrPayload::parse(text) else {
        return;
    };
    assert!(text.len() <= QR_URI_MAX_LEN);
    assert!(lower_hex64(&q.device_id), "bad id {:?}", q.device_id);
    assert!(lower_hex64(&q.fingerprint), "bad fp {:?}", q.fingerprint);
    assert!(!q.addrs.is_empty() && q.addrs.len() <= QR_MAX_ADDRS);
    assert!(q.matches_peer(&q.device_id, &q.fingerprint));

    let uri = q.to_uri();
    assert!(uri.starts_with("playdrop://pair?"));
    assert_eq!(
        QrPayload::parse(&uri).as_ref(),
        Ok(&q),
        "QR round trip of {uri}"
    );
});
