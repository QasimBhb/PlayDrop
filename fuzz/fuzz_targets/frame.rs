//! Session frame codec, protocol.md 5.2 (threat-model.md SR-15, SR-16, SR-33, T39).
//!
//! Input: one byte that picks how the stream is cut into reads, then raw session-stream
//! bytes (`[u32 BE length][JSON]` repeated), as the peer would send them on `/v1/session`.
//!
//! Runs both decoders: `FrameDecoder` (length prefix only) and `InboundDecoder` (envelope,
//! replay rule, version check and body), each fed once whole and once in pieces, plus the
//! `FrameReader` async adapter over a reader that returns the same pieces. All runs must
//! agree. Every accepted frame must respect the 64 KiB cap and the seq rule, and re-encode
//! with `OutboundEncoder` into bytes that decode to the same frame.
#![no_main]

use std::io;
use std::pin::Pin;
use std::sync::OnceLock;
use std::task::{Context, Poll};

use bytes::{Bytes, BytesMut};
use libfuzzer_sys::fuzz_target;
use playdrop_core::constants::MAX_FRAME_LEN;
use playdrop_core::session::frame::LENGTH_PREFIX_LEN;
use playdrop_core::session::types::FRAME_VERSION;
use playdrop_core::session::{
    Envelope, Frame, FrameDecoder, FrameError, FrameReader, Inbound, InboundDecoder,
    OutboundEncoder,
};
use tokio::io::{AsyncRead, ReadBuf};

/// Read sizes from a small xorshift seeded by the first input byte; 0 means one big read.
fn cuts(seed: u8, len: usize) -> Vec<usize> {
    if seed == 0 {
        return vec![len.max(1)];
    }
    let mut x = u32::from(seed).wrapping_mul(0x9E37_79B9) | 1;
    let max = 1 + usize::from(seed % 64);
    let mut out = Vec::new();
    let mut total = 0;
    while total < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let n = 1 + (x as usize) % max;
        out.push(n);
        total += n;
    }
    out
}

fn pieces<'a>(wire: &'a [u8], cuts: &[usize]) -> Vec<&'a [u8]> {
    let mut out = Vec::new();
    let mut rest = wire;
    for &n in cuts {
        if rest.is_empty() {
            break;
        }
        let (a, b) = rest.split_at(n.min(rest.len()));
        out.push(a);
        rest = b;
    }
    out
}

/// Runs `FrameDecoder` over the pieces; returns the payloads and whether it failed.
fn split_payloads(parts: &[&[u8]]) -> (Vec<Bytes>, bool) {
    let mut dec = FrameDecoder::new();
    let mut buf = BytesMut::new();
    let mut out = Vec::new();
    for p in parts {
        buf.extend_from_slice(p);
        loop {
            match dec.decode(&mut buf) {
                Ok(Some(payload)) => {
                    assert!(payload.len() <= MAX_FRAME_LEN, "payload over the cap");
                    out.push(payload);
                }
                Ok(None) => break,
                Err(FrameError::TooLarge { len, max }) => {
                    assert!(len > max && max == MAX_FRAME_LEN);
                    return (out, true);
                }
                Err(e) => panic!("framer can only fail on length: {e}"),
            }
        }
        // Never buffers more than one maximal frame waiting for its body.
        assert!(buf.len() < LENGTH_PREFIX_LEN + MAX_FRAME_LEN + p.len());
    }
    (out, false)
}

/// Runs `InboundDecoder` over the pieces; returns the items and the error, if any.
fn inbound(parts: &[&[u8]]) -> (Vec<Inbound>, Option<String>) {
    let mut dec = InboundDecoder::new();
    let mut buf = BytesMut::new();
    let mut out = Vec::new();
    for p in parts {
        buf.extend_from_slice(p);
        loop {
            let last = dec.last_seq();
            match dec.decode(&mut buf) {
                Ok(Some(item)) => {
                    check_item(&item, last, dec.last_seq());
                    out.push(item);
                }
                Ok(None) => break,
                Err(e) => return (out, Some(e.to_string())),
            }
        }
    }
    (out, None)
}

/// SR-16: accepted frames move `seq` up by exactly one, duplicates leave it alone.
fn check_item(item: &Inbound, before: u64, after: u64) {
    match item {
        Inbound::Frame(env) => {
            assert_eq!(env.seq, before + 1);
            assert_eq!(after, env.seq);
            if !matches!(env.frame, Frame::Unknown { .. }) {
                assert!(env.v >= 1 && env.v <= FRAME_VERSION);
                assert!(env.frame.validate().is_ok(), "invalid frame accepted");
            }
        }
        Inbound::UnsupportedVersion { seq, v, .. } => {
            assert_eq!(*seq, before + 1);
            assert_eq!(after, *seq);
            assert!(*v > u64::from(FRAME_VERSION));
        }
        Inbound::Duplicate { seq } => {
            assert!(*seq <= before && *seq >= 1);
            assert_eq!(after, before);
        }
    }
}

/// Re-encodes an accepted frame and decodes it on a fresh connection.
fn round_trip(env: &Envelope) {
    if matches!(env.frame, Frame::Unknown { .. }) {
        return;
    }
    let mut enc = OutboundEncoder::new();
    let bytes = match enc.encode(&env.frame, env.re) {
        Ok((seq, bytes)) => {
            assert_eq!(seq, 1);
            bytes
        }
        // Re-encoding can grow a frame (for example `1e2` becomes `100.0`).
        Err(FrameError::TooLarge { .. }) => return,
        Err(e) => panic!("accepted frame does not re-encode: {e}"),
    };
    let mut buf = BytesMut::from(&bytes[..]);
    match InboundDecoder::new().decode(&mut buf) {
        Ok(Some(Inbound::Frame(back))) => {
            assert_eq!(back.frame, env.frame, "frame round trip");
            assert_eq!(back.re, env.re);
            assert_eq!(back.seq, 1);
        }
        other => panic!("re-encoded frame decodes as {other:?}"),
    }
    assert!(buf.is_empty());
}

struct Pieces<'a> {
    parts: Vec<&'a [u8]>,
    i: usize,
}

impl AsyncRead for Pieces<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let i = self.i;
        if let Some(p) = self.parts.get(i).copied() {
            let n = p.len().min(buf.remaining());
            buf.put_slice(&p[..n]);
            if n == p.len() {
                self.i += 1;
            } else {
                self.parts[i] = &p[n..];
            }
        }
        Poll::Ready(Ok(()))
    }
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime")
    })
}

fuzz_target!(|data: &[u8]| {
    let Some((&seed, wire)) = data.split_first() else {
        return;
    };
    let whole = [wire];
    let parts = pieces(wire, &cuts(seed, wire.len()));

    let (a, a_err) = split_payloads(&whole);
    let (b, b_err) = split_payloads(&parts);
    assert_eq!(a, b, "framing depends on read sizes");
    assert_eq!(a_err, b_err);
    let framed: usize = a.iter().map(|p| LENGTH_PREFIX_LEN + p.len()).sum();
    assert!(framed <= wire.len());

    let (x, x_err) = inbound(&whole);
    let (y, y_err) = inbound(&parts);
    assert_eq!(x, y, "decoding depends on read sizes");
    assert_eq!(x_err, y_err);
    for item in &x {
        if let Inbound::Frame(env) = item {
            round_trip(env);
        }
        if let Some((reply, re)) = item.reply() {
            let mut enc = OutboundEncoder::new();
            let mut out = BytesMut::new();
            enc.encode_into(&reply, Some(re), &mut out)
                .expect("the reply to an unsupported version encodes");
        }
    }

    // The async adapter must see the same items, then end cleanly, end mid-frame or fail
    // exactly where the buffer decoder did.
    let (z, z_end) = runtime().block_on(async {
        let mut r = FrameReader::new(Pieces {
            parts: parts.clone(),
            i: 0,
        });
        let mut items = Vec::new();
        loop {
            match r.read_frame().await {
                Ok(Some(item)) => items.push(item),
                Ok(None) => return (items, None),
                Err(e) => return (items, Some(e)),
            }
        }
    });
    assert_eq!(z, x, "FrameReader disagrees with InboundDecoder");
    match (&x_err, &z_end) {
        (None, None) => assert_eq!(framed, wire.len(), "clean end with bytes left"),
        (None, Some(FrameError::Truncated)) => assert!(framed < wire.len()),
        (Some(e), Some(f)) => assert_eq!(e, &f.to_string()),
        (e, f) => panic!("decoder ended with {e:?}, reader with {f:?}"),
    }
});
