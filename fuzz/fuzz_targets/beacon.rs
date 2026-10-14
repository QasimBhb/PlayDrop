//! Beacon datagrams, protocol.md 8.1 and 8.3 (threat-model.md SR-1 to SR-3, SR-33, T39).
//!
//! Input: one UDP payload, exactly as `recv_from` hands it to the beacon receiver.
//!
//! Drives `Advert::from_beacon` on its own, then the whole receive path the socket task
//! runs: rate limiter, 512-byte cap, JSON parse, own-nonce drop, on-link check, tag
//! verification against pinned announce keys and the "Add device" list. A copy of each
//! parsed advert is re-tagged with a pinned key so the verified branch and the tag cache run
//! too, since the fuzzer cannot forge an HMAC.
#![no_main]

use std::net::{IpAddr, Ipv4Addr};

use libfuzzer_sys::fuzz_target;
use playdrop_core::discovery::advert::{MAX_BEACON_LEN, MAX_NAME_LEN};
use playdrop_core::discovery::beacon::receive_datagram;
use playdrop_core::discovery::engine::{Context, Inbound, OwnNonces, Processor};
use playdrop_core::discovery::tag::ANNOUNCE_KEY_LEN;
use playdrop_core::discovery::{
    tag, Advert, DeviceIdString, DiscoveryEvent, Ipv4Interface, PeerKeys, RateLimiter, Via,
};
use tokio::time::Instant;

const PEER_PORT: u16 = 42190;

struct Pins(Vec<(DeviceIdString, [u8; ANNOUNCE_KEY_LEN])>);

impl PeerKeys for Pins {
    fn announce_keys(&self) -> Vec<(DeviceIdString, [u8; ANNOUNCE_KEY_LEN])> {
        self.0.clone()
    }

    fn stored_port(&self, _device_id: &str) -> u16 {
        PEER_PORT
    }
}

fn pins() -> Pins {
    Pins(vec![("a".repeat(64), [1; 32]), ("b".repeat(64), [2; 32])])
}

fn lan() -> [Ipv4Interface; 1] {
    [Ipv4Interface {
        index: 2,
        addr: Ipv4Addr::new(192, 168, 1, 10),
        netmask: Ipv4Addr::new(255, 255, 255, 0),
    }]
}

/// Display-safety rules every parsed "Add device" field must meet (protocol.md 8.1, SR-8).
fn check_parsed(datagram: &[u8], a: &Advert) {
    assert!(
        datagram.len() <= MAX_BEACON_LEN,
        "oversized datagram parsed"
    );
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
        assert_ne!(f.port, 0);
    }
}

fn check_event(ev: &DiscoveryEvent, src: IpAddr, keys: &Pins, window: bool) {
    match ev {
        DiscoveryEvent::Verified {
            device_id,
            addr,
            via,
            ..
        } => {
            assert!(keys.0.iter().any(|(id, _)| id == device_id));
            // The dial target is the packet source plus the stored port, never the payload.
            assert_eq!(addr.ip(), src);
            assert_eq!(addr.port(), PEER_PORT);
            assert_eq!(*via, Via::Beacon);
        }
        DiscoveryEvent::Unpaired { addr, .. } => {
            assert!(window, "unpaired advert surfaced with the window closed");
            assert_eq!(addr.ip(), src);
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let parsed = Advert::from_beacon(data);
    if let Some(a) = &parsed {
        check_parsed(data, a);
        let again = a
            .to_beacon()
            .expect("a parsed advert re-encodes within 512 bytes");
        assert_eq!(
            Advert::from_beacon(&again).as_ref(),
            Some(a),
            "beacon round trip"
        );
    }

    let keys = pins();
    let interfaces = lan();
    let own = OwnNonces::default();
    let mut limiter = RateLimiter::new();
    let mut proc = Processor::new();
    let now = Instant::now();
    let on_link = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20));
    let off_link = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 20));

    for window in [false, true] {
        let ctx = Context {
            keys: &keys,
            add_device_window: window,
            interfaces: &interfaces,
            own: &own,
        };
        let ev = receive_datagram(&mut limiter, data, on_link, now, |i| {
            proc.handle(i, &ctx, now)
        });
        if let Some(ev) = &ev {
            assert!(parsed.is_some(), "event from an unparseable datagram");
            check_event(ev, on_link, &keys, window);
        }
        let ev = receive_datagram(&mut limiter, data, off_link, now, |i| {
            proc.handle(i, &ctx, now)
        });
        assert!(ev.is_none(), "off-link source produced {ev:?}");
    }

    // Re-tag the advert with a pinned key: it must verify as that peer, both fresh and from
    // the cache, and be dropped once the nonce is recorded as our own.
    if let Some(a) = parsed {
        let forged = Advert {
            t: tag(&[2; 32], &a.n),
            ..a
        };
        let ctx = Context {
            keys: &keys,
            add_device_window: false,
            interfaces: &interfaces,
            own: &own,
        };
        let input = Inbound {
            advert: &forged,
            src: on_link,
            scope_id: 0,
            via: Via::Beacon,
        };
        for _ in 0..2 {
            match proc.handle(input, &ctx, now) {
                Some(DiscoveryEvent::Verified { device_id, .. }) => {
                    assert_eq!(device_id, "b".repeat(64));
                }
                other => panic!("pinned tag did not verify: {other:?}"),
            }
        }
        own.record(forged.n);
        assert!(
            proc.handle(input, &ctx, now).is_none(),
            "own advert not dropped"
        );
    }
});
