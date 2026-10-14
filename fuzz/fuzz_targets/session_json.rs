//! Session frame bodies, protocol.md 5.3 to 5.14 and 6.2 to 6.4 (threat-model.md SR-6, SR-8,
//! SR-9, SR-12, SR-16, SR-33).
//!
//! Input: one or more frame JSON payloads (without the length prefix), separated by the
//! byte 0x1E, which cannot appear unescaped in JSON. The first payload is usually `hello`.
//!
//! Each payload is parsed straight into `Frame` (every type, plus `hello` validation and
//! the capability and grant tables) and also through one `InboundDecoder::accept_payload`
//! shared by all payloads, as one connection would. Accepted frames then drive four pairing
//! state machines (SAS and QR, initiator and responder) the way a host routes `pair_*`
//! frames before the peer is pinned. Every frame a state machine asks to send must encode.
#![no_main]

use libfuzzer_sys::fuzz_target;
use playdrop_core::identity::device_id_from_spki;
use playdrop_core::session::hello::NAME_MAX_CHARS;
use playdrop_core::session::pairing::{PairingEvent, PairingOutput};
use playdrop_core::session::{
    may_send_frame, validate_hello, Capability, Frame, Inbound, InboundDecoder, Nonce,
    OutboundEncoder, PairingContext, PairingSession, Party, QrOffer, QrPayload, QrSecret,
};
use std::time::{Duration, Instant};

const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

const CAPS: &[&str] = &[
    "ping",
    "find",
    "clipboard",
    "clipboard.image",
    "battery",
    "media.state",
    "media.control",
    "presence",
    "input.pointer",
    "input.key",
    "input.presenter",
    "transfer",
    "transfer.bundle",
    "outbox",
    "blobs",
];

fn party(fill: u8) -> Party {
    let mut der = SPKI_PREFIX.to_vec();
    der.extend_from_slice(&[fill; 32]);
    Party::new(device_id_from_spki(&der).to_string(), der)
}

fn ctx() -> PairingContext {
    PairingContext::new(
        party(0x11),
        party(0x22),
        [0x33; 32],
        "Peer",
        "abcd-efgh-ijkl-mnop",
    )
    .expect("valid pairing context")
}

/// Checks the body alone: validation, `hello` rules and the capability tables.
fn check_frame(payload: &[u8]) {
    let Ok(frame) = serde_json::from_slice::<Frame>(payload) else {
        return;
    };
    assert!(
        !matches!(frame, Frame::Unknown { .. }),
        "Unknown is never parsed"
    );
    let _ = frame.type_name();
    let _ = frame.required_grant();
    let all: Vec<Capability> = CAPS.iter().map(|c| Capability::from_wire(c)).collect();
    let _ = may_send_frame(&frame, &all, &all);
    assert!(
        !may_send_frame(&frame, &[], &[])
            || matches!(
                frame.required_capabilities(),
                playdrop_core::session::types::CapRequirement::None
            ),
        "frame allowed with no capabilities"
    );
    if frame.validate().is_ok() {
        let again: Frame = serde_json::from_slice(&serde_json::to_vec(&frame).unwrap())
            .expect("re-serialised frame parses");
        assert_eq!(again, frame, "frame round trip");
    }
    if let Frame::Hello(h) = &frame {
        let peer = party(0x22).device_id;
        for (expected, pinned) in [(h.device_id.as_str(), false), (peer.as_str(), true)] {
            if let Ok(v) = validate_hello(h, expected, 1, pinned) {
                assert_eq!(h.device_id, expected);
                assert!(
                    h.device_id.len() == 64
                        && h.device_id
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                    "bad device id accepted: {:?}",
                    h.device_id
                );
                assert_eq!(h.protocol_version, 1);
                for s in [&v.name, &v.app] {
                    assert!(s.chars().count() <= NAME_MAX_CHARS);
                    assert!(!s.chars().any(char::is_control), "unsanitised {s:?}");
                    assert_eq!(s.trim(), s);
                }
            }
        }
    }
}

struct Machine {
    session: PairingSession,
    finished: bool,
}

impl Machine {
    fn new(session: PairingSession) -> Self {
        Self {
            session,
            finished: false,
        }
    }

    /// Applies outputs: every frame to send must encode, prompts are confirmed at once,
    /// and nothing at all comes out after the attempt ended.
    fn apply(&mut self, out: Vec<PairingOutput>, now: Instant) {
        if self.finished {
            assert!(out.is_empty(), "output after the attempt ended: {out:?}");
            return;
        }
        let mut confirm = false;
        for o in &out {
            match o {
                PairingOutput::Send { frame, re } => {
                    OutboundEncoder::new()
                        .encode(frame, *re)
                        .expect("pairing output encodes");
                }
                PairingOutput::Event(PairingEvent::ShowSas { sas, .. }) => {
                    assert!(
                        sas.as_str().len() == 6 && sas.as_str().bytes().all(|b| b.is_ascii_digit())
                    );
                    confirm = true;
                }
                PairingOutput::Event(PairingEvent::AskConfirmScanner { .. }) => confirm = true,
                PairingOutput::Event(PairingEvent::Paired { peer_id, .. }) => {
                    assert_eq!(*peer_id, party(0x22).device_id);
                }
                PairingOutput::Event(_) => {}
            }
        }
        self.finished = self.session.is_finished();
        if confirm && !self.finished {
            let more = self.session.confirm(now);
            self.apply(more, now);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let now = Instant::now();
    let qr = QrPayload {
        device_id: party(0x22).device_id,
        fingerprint: "a8".repeat(32),
        addrs: vec!["192.168.1.20:42187".parse().unwrap()],
        secret: QrSecret::from_bytes([5; 16]),
        exp: 1_791_460_920,
    };
    let (sas_i, out_sas) = PairingSession::sas_initiator(ctx(), now, Nonce::from_bytes([1; 16]));
    let (qr_i, out_qr) =
        PairingSession::qr_initiator(ctx(), now, &qr, &qr.fingerprint).expect("code matches");
    let mut machines = [
        Machine::new(sas_i),
        Machine::new(qr_i),
        Machine::new(PairingSession::responder(
            ctx(),
            now,
            Nonce::from_bytes([2; 16]),
            None,
        )),
        Machine::new(PairingSession::responder(
            ctx(),
            now,
            Nonce::from_bytes([3; 16]),
            Some(QrOffer::new(
                QrSecret::from_bytes([5; 16]),
                now + Duration::from_secs(120),
            )),
        )),
    ];
    machines[0].apply(out_sas, now);
    machines[1].apply(out_qr, now);

    let mut decoder = InboundDecoder::new();
    for payload in data.split(|&b| b == 0x1E) {
        check_frame(payload);
        let Ok(item) = decoder.accept_payload(payload) else {
            // Every decoder error closes the connection (protocol.md 5.2).
            break;
        };
        if let Inbound::Frame(env) = item {
            for m in &mut machines {
                let out = m.session.handle_frame(now, &env);
                m.apply(out, now);
            }
        }
    }
    let later = now + Duration::from_secs(3600);
    for m in &mut machines {
        let out = m.session.poll(later);
        m.apply(out, later);
        assert!(m.session.is_finished(), "timeout did not end the attempt");
        let out = m.session.reject(later);
        m.apply(out, later);
    }
});
