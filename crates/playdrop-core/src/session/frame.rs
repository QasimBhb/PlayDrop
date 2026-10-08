//! Session frame codec and replay rule, protocol.md 5.2 (SR-15, SR-16).
//!
//! Wire format: `[length: u32 BE][JSON object of length bytes]`, at most 64 KiB of JSON.
//!
//! The primary API works on byte buffers so the HTTP/2 layer can push `h2` body chunks into a
//! `BytesMut` and take encoded `Bytes` out:
//!
//! - [`FrameDecoder`] splits length-prefixed payloads out of a `BytesMut`.
//! - [`InboundDecoder`] adds header parsing, the [`SeqTracker`] replay rule, frame version
//!   checks and body parsing, yielding [`Inbound`] items.
//! - [`OutboundEncoder`] assigns `seq` with a [`SeqAssigner`] and appends encoded frames to a
//!   `BytesMut`, so several frames can be batched into one TLS write (protocol.md section 10).
//!
//! [`FrameReader`] and [`FrameWriter`] adapt these to `AsyncRead` and `AsyncWrite`.
//!
//! Every [`FrameError`] is fatal: the caller closes the connection without sending an error
//! frame (protocol.md section 9).

use std::borrow::Cow;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::types::{ErrorCode, Frame, FRAME_VERSION};
use crate::constants::MAX_FRAME_LEN;

/// Size of the big-endian length prefix in bytes.
pub const LENGTH_PREFIX_LEN: usize = 4;

/// A fatal session-plane protocol error (protocol.md 5.2). The connection must be closed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FrameError {
    /// The length prefix exceeds the 64 KiB maximum (SR-15).
    #[error("frame length {len} exceeds maximum {max}")]
    TooLarge {
        /// Declared or encoded length.
        len: usize,
        /// The limit.
        max: usize,
    },
    /// Malformed JSON, a non-object, a bad field type or a failed structural check.
    #[error("malformed frame: {0}")]
    Malformed(String),
    /// `t`, `v` or `seq` is missing.
    #[error("frame missing `{0}`")]
    MissingField(&'static str),
    /// `seq` skipped ahead (SR-16).
    #[error("seq gap: expected {expected}, got {got}")]
    SeqGap {
        /// The only acceptable next value.
        expected: u64,
        /// What arrived.
        got: u64,
    },
    /// The stream ended inside a frame.
    #[error("stream ended inside a frame")]
    Truncated,
    /// A local frame could not be encoded (unknown type, failed validation, serde error).
    #[error("cannot encode frame: {0}")]
    Encode(String),
    /// Transport failure in the async adapters.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A decoded frame with its envelope fields (protocol.md 5.2).
#[derive(Debug, Clone, PartialEq)]
pub struct Envelope {
    /// Frame version, at most [`FRAME_VERSION`] for frames that reach the caller.
    pub v: u32,
    /// Sequence number, per connection and direction (SR-16).
    pub seq: u64,
    /// The `seq` this frame answers, if it is a reply.
    pub re: Option<u64>,
    /// The frame body.
    pub frame: Frame,
}

/// One inbound item from [`InboundDecoder`].
#[allow(
    clippy::large_enum_variant,
    reason = "accepted frames are the common case; boxing would allocate per frame"
)]
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// An accepted frame. `Frame::Unknown` is passed through and must be ignored.
    Frame(Envelope),
    /// A duplicate (`seq` not above the last accepted); dropped with no effect (SR-16).
    Duplicate {
        /// The repeated `seq`.
        seq: u64,
    },
    /// A known type with a `v` newer than supported. Its `seq` was accepted but the frame is
    /// ignored; send [`Inbound::reply`] (protocol.md 5.2).
    UnsupportedVersion {
        /// The frame's `seq`, used as `re` in the reply.
        seq: u64,
        /// The frame's type.
        t: String,
        /// The version it claimed.
        v: u64,
    },
}

impl Inbound {
    /// The reply the receiver should send, as `(frame, re)`: `error`
    /// `unsupported_frame_version` for [`Inbound::UnsupportedVersion`]. `error` frames are
    /// never answered, so two hosts cannot bounce errors at each other.
    pub fn reply(&self) -> Option<(Frame, u64)> {
        match self {
            Inbound::UnsupportedVersion { seq, t, .. } if t != "error" => {
                Some((Frame::error(ErrorCode::UnsupportedFrameVersion, None), *seq))
            }
            _ => None,
        }
    }
}

/// Splits length-prefixed payloads out of a byte buffer (protocol.md 5.2).
#[derive(Debug, Clone)]
pub struct FrameDecoder {
    max_len: usize,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self {
            max_len: MAX_FRAME_LEN,
        }
    }
}

impl FrameDecoder {
    /// A decoder with the protocol maximum of 64 KiB.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the next complete JSON payload out of `buf`, or `None` if more bytes are needed.
    /// A declared length over the maximum fails as soon as the prefix is seen, before the body
    /// is buffered (SR-15).
    pub fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Bytes>, FrameError> {
        if buf.len() < LENGTH_PREFIX_LEN {
            buf.reserve(LENGTH_PREFIX_LEN - buf.len());
            return Ok(None);
        }
        let declared = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
        let len = usize::try_from(declared).unwrap_or(usize::MAX);
        if len > self.max_len {
            return Err(FrameError::TooLarge {
                len,
                max: self.max_len,
            });
        }
        let total = LENGTH_PREFIX_LEN + len;
        if buf.len() < total {
            buf.reserve(total - buf.len());
            return Ok(None);
        }
        buf.advance(LENGTH_PREFIX_LEN);
        Ok(Some(buf.split_to(len).freeze()))
    }
}

/// Inbound replay check, one per connection (SR-16). `seq` starts at 1 and must rise by
/// exactly 1.
#[derive(Debug, Clone, Default)]
pub struct SeqTracker {
    last: u64,
}

/// Outcome of [`SeqTracker::check`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqVerdict {
    /// The next expected `seq`; now recorded.
    Accept,
    /// Not above the last accepted; drop with no effect.
    Duplicate,
}

impl SeqTracker {
    /// A tracker for a new connection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last accepted `seq`, 0 before the first frame.
    pub fn last_accepted(&self) -> u64 {
        self.last
    }

    /// Applies the replay rule. `seq` 0 is malformed (numbering starts at 1); a gap is fatal.
    pub fn check(&mut self, seq: u64) -> Result<SeqVerdict, FrameError> {
        if seq == 0 {
            return Err(FrameError::Malformed("seq 0".into()));
        }
        if seq <= self.last {
            return Ok(SeqVerdict::Duplicate);
        }
        let expected = self.last + 1;
        if seq != expected {
            return Err(FrameError::SeqGap { expected, got: seq });
        }
        self.last = seq;
        Ok(SeqVerdict::Accept)
    }
}

/// Outbound `seq` numbering, one per connection (SR-16).
#[derive(Debug, Clone)]
pub struct SeqAssigner {
    next: u64,
}

impl Default for SeqAssigner {
    fn default() -> Self {
        Self { next: 1 }
    }
}

impl SeqAssigner {
    /// An assigner for a new connection; the first `seq` is 1.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `seq` the next frame will get.
    pub fn peek(&self) -> u64 {
        self.next
    }

    /// Returns the next `seq` and advances.
    pub fn assign(&mut self) -> u64 {
        let seq = self.next;
        self.next += 1;
        seq
    }
}

/// The envelope fields, parsed before the body so duplicates and unknown types cost little.
#[derive(Deserialize)]
struct Header<'a> {
    #[serde(borrow)]
    t: Option<Cow<'a, str>>,
    v: Option<u64>,
    seq: Option<u64>,
    re: Option<u64>,
}

/// Serialised form: body fields (with `t`) followed by the envelope fields.
#[derive(Serialize)]
struct OutEnvelope<'a> {
    #[serde(flatten)]
    frame: &'a Frame,
    v: u32,
    seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    re: Option<u64>,
}

/// Full inbound pipeline for one connection: framing, envelope, replay rule, version check,
/// body parsing and validation (protocol.md 5.2, SR-16).
#[derive(Debug, Clone, Default)]
pub struct InboundDecoder {
    framer: FrameDecoder,
    seq: SeqTracker,
}

impl InboundDecoder {
    /// A decoder for a new connection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last accepted inbound `seq`.
    pub fn last_seq(&self) -> u64 {
        self.seq.last_accepted()
    }

    /// Decodes the next item from `buf`, or `None` if more bytes are needed.
    pub fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Inbound>, FrameError> {
        match self.framer.decode(buf)? {
            Some(payload) => self.accept_payload(&payload).map(Some),
            None => Ok(None),
        }
    }

    /// Processes one JSON payload (without its length prefix).
    pub fn accept_payload(&mut self, payload: &[u8]) -> Result<Inbound, FrameError> {
        let header: Header<'_> =
            serde_json::from_slice(payload).map_err(|e| FrameError::Malformed(e.to_string()))?;
        let t = header.t.ok_or(FrameError::MissingField("t"))?;
        let v = header.v.ok_or(FrameError::MissingField("v"))?;
        let seq = header.seq.ok_or(FrameError::MissingField("seq"))?;
        if v == 0 {
            return Err(FrameError::Malformed("v 0".into()));
        }
        if self.seq.check(seq)? == SeqVerdict::Duplicate {
            return Ok(Inbound::Duplicate { seq });
        }
        let Some(supported) = Frame::supported_version(&t) else {
            return Ok(Inbound::Frame(Envelope {
                v: u32::try_from(v).unwrap_or(u32::MAX),
                seq,
                re: header.re,
                frame: Frame::Unknown { t: t.into_owned() },
            }));
        };
        if v > u64::from(supported) {
            return Ok(Inbound::UnsupportedVersion {
                seq,
                t: t.into_owned(),
                v,
            });
        }
        let frame: Frame =
            serde_json::from_slice(payload).map_err(|e| FrameError::Malformed(e.to_string()))?;
        frame
            .validate()
            .map_err(|e| FrameError::Malformed(e.into()))?;
        Ok(Inbound::Frame(Envelope {
            // Checked above: v <= supported, which is a u32.
            v: u32::try_from(v).unwrap_or(supported),
            seq,
            re: header.re,
            frame,
        }))
    }
}

/// Outbound pipeline for one connection: assigns `seq` and encodes length-prefixed frames
/// (protocol.md 5.2).
#[derive(Debug, Clone)]
pub struct OutboundEncoder {
    seq: SeqAssigner,
    max_len: usize,
}

impl Default for OutboundEncoder {
    fn default() -> Self {
        Self {
            seq: SeqAssigner::new(),
            max_len: MAX_FRAME_LEN,
        }
    }
}

impl OutboundEncoder {
    /// An encoder for a new connection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The `seq` the next frame will get.
    pub fn next_seq(&self) -> u64 {
        self.seq.peek()
    }

    /// Appends one encoded frame to `dst` and returns its `seq`. On error `dst` is left as it
    /// was and no `seq` is consumed, so a refused frame never creates a gap.
    pub fn encode_into(
        &mut self,
        frame: &Frame,
        re: Option<u64>,
        dst: &mut BytesMut,
    ) -> Result<u64, FrameError> {
        if let Frame::Unknown { t } = frame {
            return Err(FrameError::Encode(format!("unknown frame type {t}")));
        }
        frame.validate().map_err(|e| FrameError::Encode(e.into()))?;
        let seq = self.seq.peek();
        let start = dst.len();
        dst.put_u32(0);
        let out = OutEnvelope {
            frame,
            v: FRAME_VERSION,
            seq,
            re,
        };
        if let Err(e) = serde_json::to_writer((&mut *dst).writer(), &out) {
            dst.truncate(start);
            return Err(FrameError::Encode(e.to_string()));
        }
        let len = dst.len() - start - LENGTH_PREFIX_LEN;
        let prefix = match u32::try_from(len) {
            Ok(p) if len <= self.max_len => p,
            _ => {
                dst.truncate(start);
                return Err(FrameError::TooLarge {
                    len,
                    max: self.max_len,
                });
            }
        };
        dst[start..start + LENGTH_PREFIX_LEN].copy_from_slice(&prefix.to_be_bytes());
        self.seq.assign();
        Ok(seq)
    }

    /// Encodes one frame into its own buffer, returning `(seq, bytes)`.
    pub fn encode(&mut self, frame: &Frame, re: Option<u64>) -> Result<(u64, Bytes), FrameError> {
        let mut dst = BytesMut::with_capacity(256);
        let seq = self.encode_into(frame, re, &mut dst)?;
        Ok((seq, dst.freeze()))
    }
}

/// `AsyncRead` adapter over [`InboundDecoder`].
#[derive(Debug)]
pub struct FrameReader<R> {
    inner: R,
    buf: BytesMut,
    decoder: InboundDecoder,
}

impl<R: AsyncRead + Unpin> FrameReader<R> {
    /// Wraps a reader for a new connection.
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            buf: BytesMut::with_capacity(8 * 1024),
            decoder: InboundDecoder::new(),
        }
    }

    /// Reads the next item. `Ok(None)` is a clean end of stream at a frame boundary; an end
    /// inside a frame is [`FrameError::Truncated`]. Cancel-safe: buffered bytes are kept.
    pub async fn read_frame(&mut self) -> Result<Option<Inbound>, FrameError> {
        loop {
            if let Some(item) = self.decoder.decode(&mut self.buf)? {
                return Ok(Some(item));
            }
            if self.inner.read_buf(&mut self.buf).await? == 0 {
                return if self.buf.is_empty() {
                    Ok(None)
                } else {
                    Err(FrameError::Truncated)
                };
            }
        }
    }

    /// Returns the inner reader, dropping any buffered bytes.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

/// `AsyncWrite` adapter over [`OutboundEncoder`] with write batching.
#[derive(Debug)]
pub struct FrameWriter<W> {
    inner: W,
    buf: BytesMut,
    encoder: OutboundEncoder,
}

impl<W: AsyncWrite + Unpin> FrameWriter<W> {
    /// Wraps a writer for a new connection.
    pub fn new(inner: W) -> Self {
        Self {
            inner,
            buf: BytesMut::with_capacity(8 * 1024),
            encoder: OutboundEncoder::new(),
        }
    }

    /// Encodes a frame into the write buffer without writing; returns its `seq`.
    pub fn queue(&mut self, frame: &Frame, re: Option<u64>) -> Result<u64, FrameError> {
        self.encoder.encode_into(frame, re, &mut self.buf)
    }

    /// Writes every queued frame and flushes. Not cancel-safe: a cancelled flush may leave a
    /// partial frame on the wire, so the connection must then be closed.
    pub async fn flush(&mut self) -> Result<(), FrameError> {
        if !self.buf.is_empty() {
            self.inner.write_all(&self.buf).await?;
            self.buf.clear();
        }
        self.inner.flush().await?;
        Ok(())
    }

    /// Queues one frame and flushes; returns its `seq`.
    pub async fn send(&mut self, frame: &Frame, re: Option<u64>) -> Result<u64, FrameError> {
        let seq = self.queue(frame, re)?;
        self.flush().await?;
        Ok(seq)
    }

    /// Returns the inner writer, dropping any unflushed frames.
    pub fn into_inner(self) -> W {
        self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::{Ping, PING_MESSAGE_MAX};
    use serde_json::Value;

    const SPEC_EXAMPLES: &[&str] = crate::session::types::tests::SPEC_EXAMPLES;

    fn prefixed(json: &[u8]) -> BytesMut {
        let mut b = BytesMut::new();
        b.put_u32(u32::try_from(json.len()).unwrap());
        b.extend_from_slice(json);
        b
    }

    fn with_seq(example: &str, seq: u64) -> Vec<u8> {
        let mut v: Value = serde_json::from_str(example).unwrap();
        v["seq"] = seq.into();
        serde_json::to_vec(&v).unwrap()
    }

    fn one(json: &[u8]) -> Result<Option<Inbound>, FrameError> {
        InboundDecoder::new().decode(&mut prefixed(json))
    }

    #[test]
    fn spec_examples_decode_with_envelope() {
        for ex in SPEC_EXAMPLES {
            let raw: Value = serde_json::from_str(ex).unwrap();
            let expected: Frame = serde_json::from_str(ex).unwrap();
            let got = one(&with_seq(ex, 1)).unwrap().unwrap();
            let Inbound::Frame(env) = got else {
                panic!("{ex}: {got:?}")
            };
            assert_eq!(env.frame, expected, "{ex}");
            assert_eq!(env.seq, 1);
            assert_eq!(env.v, 1);
            assert_eq!(env.re, raw.get("re").and_then(Value::as_u64), "{ex}");
        }
    }

    #[test]
    fn spec_examples_with_seq_decode_verbatim() {
        // hello carries seq 1 in the spec, verbatim.
        let got = one(SPEC_EXAMPLES[0].as_bytes()).unwrap().unwrap();
        assert!(matches!(
            got,
            Inbound::Frame(Envelope {
                seq: 1,
                frame: Frame::Hello(_),
                ..
            })
        ));
    }

    #[test]
    fn encode_decode_round_trip_every_type() {
        let mut enc = OutboundEncoder::new();
        let mut dec = InboundDecoder::new();
        let mut wire = BytesMut::new();
        let mut expected = Vec::new();
        for ex in SPEC_EXAMPLES {
            let raw: Value = serde_json::from_str(ex).unwrap();
            let frame: Frame = serde_json::from_str(ex).unwrap();
            let re = raw.get("re").and_then(Value::as_u64);
            let seq = enc.encode_into(&frame, re, &mut wire).unwrap();
            expected.push(Envelope {
                v: 1,
                seq,
                re,
                frame,
            });
        }
        // Feed one byte at a time to exercise partial reads.
        let mut feed = BytesMut::new();
        let mut got = Vec::new();
        for b in wire {
            feed.put_u8(b);
            while let Some(item) = dec.decode(&mut feed).unwrap() {
                let Inbound::Frame(env) = item else { panic!() };
                got.push(env);
            }
        }
        assert_eq!(got, expected);
        assert_eq!(dec.last_seq(), u64::try_from(SPEC_EXAMPLES.len()).unwrap());
    }

    #[test]
    fn oversize_length_is_fatal_before_body() {
        let mut b = BytesMut::new();
        b.put_u32(u32::try_from(MAX_FRAME_LEN + 1).unwrap());
        assert!(matches!(
            InboundDecoder::new().decode(&mut b),
            Err(FrameError::TooLarge { .. })
        ));
        let mut b = BytesMut::new();
        b.put_u32(u32::MAX);
        assert!(matches!(
            FrameDecoder::new().decode(&mut b),
            Err(FrameError::TooLarge { .. })
        ));
        // Exactly the maximum is allowed and just waits for the body.
        let mut b = BytesMut::new();
        b.put_u32(u32::try_from(MAX_FRAME_LEN).unwrap());
        assert!(FrameDecoder::new().decode(&mut b).unwrap().is_none());
    }

    #[test]
    fn encoder_refuses_oversize_without_consuming_seq() {
        let mut enc = OutboundEncoder::new();
        let mut dst = BytesMut::new();
        let huge = Frame::Ping(Ping {
            message: Some("x".repeat(MAX_FRAME_LEN)),
        });
        assert!(matches!(
            enc.encode_into(&huge, None, &mut dst),
            Err(FrameError::TooLarge { .. })
        ));
        assert!(dst.is_empty());
        let ok = Frame::Ping(Ping::new(Some(&"y".repeat(PING_MESSAGE_MAX * 2))));
        assert_eq!(enc.encode_into(&ok, None, &mut dst).unwrap(), 1);
        assert!(enc
            .encode_into(&Frame::Unknown { t: "x".into() }, None, &mut dst)
            .is_err());
        assert_eq!(enc.next_seq(), 2);
    }

    #[test]
    fn duplicate_dropped_gap_fatal() {
        let mut dec = InboundDecoder::new();
        let ping = |seq: u64| prefixed(format!(r#"{{"t":"ping","v":1,"seq":{seq}}}"#).as_bytes());
        assert!(matches!(
            dec.decode(&mut ping(1)).unwrap(),
            Some(Inbound::Frame(_))
        ));
        assert!(matches!(
            dec.decode(&mut ping(2)).unwrap(),
            Some(Inbound::Frame(_))
        ));
        assert_eq!(
            dec.decode(&mut ping(2)).unwrap(),
            Some(Inbound::Duplicate { seq: 2 })
        );
        assert_eq!(
            dec.decode(&mut ping(1)).unwrap(),
            Some(Inbound::Duplicate { seq: 1 })
        );
        assert!(matches!(
            dec.decode(&mut ping(3)).unwrap(),
            Some(Inbound::Frame(_))
        ));
        assert!(matches!(
            dec.decode(&mut ping(5)),
            Err(FrameError::SeqGap {
                expected: 4,
                got: 5
            })
        ));
    }

    #[test]
    fn first_frame_must_be_seq_one() {
        assert!(matches!(
            one(br#"{"t":"ping","v":1,"seq":2}"#),
            Err(FrameError::SeqGap {
                expected: 1,
                got: 2
            })
        ));
        assert!(matches!(
            one(br#"{"t":"ping","v":1,"seq":0}"#),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn duplicate_clipboard_has_no_effect() {
        // SR-16 test: a duplicated clipboard frame is not delivered twice.
        let clip = with_seq(SPEC_EXAMPLES[5], 1);
        let mut wire = prefixed(&clip);
        wire.extend_from_slice(&prefixed(&clip));
        let mut dec = InboundDecoder::new();
        assert!(matches!(
            dec.decode(&mut wire).unwrap(),
            Some(Inbound::Frame(Envelope {
                frame: Frame::Clipboard(_),
                ..
            }))
        ));
        assert_eq!(
            dec.decode(&mut wire).unwrap(),
            Some(Inbound::Duplicate { seq: 1 })
        );
    }

    #[test]
    fn malformed_and_missing_fields_are_fatal() {
        let cases: &[&[u8]] = &[
            b"",
            b"{",
            b"[1,2]",
            b"\"ping\"",
            br#"{"t":"ping","v":1,"seq":1} trailing"#,
            br#"{"v":1,"seq":1}"#,
            br#"{"t":"ping","seq":1}"#,
            br#"{"t":"ping","v":1}"#,
            br#"{"t":null,"v":1,"seq":1}"#,
            br#"{"t":"ping","v":"1","seq":1}"#,
            br#"{"t":"ping","v":0,"seq":1}"#,
            br#"{"t":"ping","v":1,"seq":-1}"#,
            br#"{"t":"battery","v":1,"seq":1,"level":"high"}"#,
            br#"{"t":"clipboard","v":1,"seq":1,"kind":"text","mime":"m","hash":"h"}"#,
        ];
        for c in cases {
            assert!(one(c).is_err(), "{}", String::from_utf8_lossy(c));
        }
    }

    #[test]
    fn unknown_type_passes_through_and_counts() {
        let mut dec = InboundDecoder::new();
        let got = dec
            .decode(&mut prefixed(br#"{"t":"hologram","v":9,"seq":1,"x":[1]}"#))
            .unwrap()
            .unwrap();
        assert_eq!(
            got,
            Inbound::Frame(Envelope {
                v: 9,
                seq: 1,
                re: None,
                frame: Frame::Unknown {
                    t: "hologram".into()
                }
            })
        );
        assert!(got.reply().is_none());
        assert!(dec
            .decode(&mut prefixed(br#"{"t":"pong","v":1,"seq":2,"re":1}"#))
            .unwrap()
            .is_some());
    }

    #[test]
    fn newer_version_gets_error_reply_and_is_ignored() {
        let mut dec = InboundDecoder::new();
        // The body would be malformed under v1, which proves it is not parsed.
        let got = dec
            .decode(&mut prefixed(
                br#"{"t":"battery","v":2,"seq":1,"level":"high"}"#,
            ))
            .unwrap()
            .unwrap();
        assert_eq!(
            got,
            Inbound::UnsupportedVersion {
                seq: 1,
                t: "battery".into(),
                v: 2
            }
        );
        let (reply, re) = got.reply().unwrap();
        assert_eq!(re, 1);
        assert_eq!(
            reply,
            Frame::error(ErrorCode::UnsupportedFrameVersion, None)
        );
        assert_eq!(dec.last_seq(), 1);
        let err = dec
            .decode(&mut prefixed(br#"{"t":"error","v":2,"seq":2,"code":"x"}"#))
            .unwrap()
            .unwrap();
        assert!(err.reply().is_none());
    }

    #[test]
    fn encoded_json_has_envelope_fields() {
        let mut enc = OutboundEncoder::new();
        let (seq, bytes) = enc.encode(&Frame::Pong, Some(41)).unwrap();
        assert_eq!(seq, 1);
        let v: Value = serde_json::from_slice(&bytes[LENGTH_PREFIX_LEN..]).unwrap();
        assert_eq!(v, serde_json::json!({"t":"pong","v":1,"seq":1,"re":41}));
        let len = u32::from_be_bytes(bytes[..4].try_into().unwrap());
        assert_eq!(
            usize::try_from(len).unwrap(),
            bytes.len() - LENGTH_PREFIX_LEN
        );
    }

    #[tokio::test]
    async fn async_adapters_round_trip() {
        let (a, b) = tokio::io::duplex(64);
        let mut w = FrameWriter::new(a);
        let mut r = FrameReader::new(b);
        let writer = tokio::spawn(async move {
            w.queue(&Frame::Ping(Ping::new(Some("hi"))), None).unwrap();
            w.queue(&Frame::Pong, Some(7)).unwrap();
            w.flush().await.unwrap();
            assert_eq!(w.send(&Frame::Unpair, None).await.unwrap(), 3);
        });
        let mut seqs = Vec::new();
        while let Some(item) = r.read_frame().await.unwrap() {
            let Inbound::Frame(env) = item else { panic!() };
            seqs.push((env.seq, env.frame.type_name().to_owned()));
        }
        writer.await.unwrap();
        assert_eq!(
            seqs,
            vec![(1, "ping".into()), (2, "pong".into()), (3, "unpair".into())]
        );
    }

    #[tokio::test]
    async fn async_reader_truncated_is_error() {
        let (mut a, b) = tokio::io::duplex(64);
        a.write_all(&[0, 0, 0, 10, b'{']).await.unwrap();
        drop(a);
        let mut r = FrameReader::new(b);
        assert!(matches!(r.read_frame().await, Err(FrameError::Truncated)));
    }
}
