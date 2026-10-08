//! Session plane: frame codec, replay rule, frame types, hello checks, pairing and limits.
//!
//! Implements `docs/protocol.md` sections 4, 5 and 6 and the security requirements SR-6,
//! SR-7, SR-8, SR-9, SR-13, SR-14, SR-15 and SR-16 from `docs/threat-model.md`.
//!
//! The module is transport-agnostic: [`frame`] works on byte buffers so the HTTP/2 layer can
//! feed request body chunks in and get `Bytes` out, with an `AsyncRead`/`AsyncWrite` adapter
//! on top. Identities are passed in as plain values (Device ID hex strings, SPKI DER bytes,
//! 32-byte SPKI hashes and the 32-byte TLS exporter value) so this module does not depend on
//! the identity or TLS modules.

pub mod frame;
pub mod hello;
pub mod limits;
pub mod pairing;
pub mod types;

mod util;

pub use frame::{
    Envelope, FrameDecoder, FrameError, FrameReader, FrameWriter, Inbound, InboundDecoder,
    OutboundEncoder, SeqAssigner, SeqTracker, SeqVerdict,
};
pub use hello::{
    may_send, may_send_frame, sanitize_name, validate_hello, HelloError, TrustCheck, ValidatedHello,
};
pub use limits::{
    ConnectionCounter, ConnectionPermit, ConnectionRefused, HandshakeGate, Limits, ResetRateCounter,
};
pub use pairing::{
    AddDeviceWindow, BeginRefusal, Nonce, PairRequestLimiter, PairingContext, PairingError,
    PairingEvent, PairingOutput, PairingSession, Party, QrOffer, QrPayload, QrSecret, Role, Sas,
};
pub use types::{Capability, ErrorCode, Frame, GrantKind, RejectReason};

impl From<FrameError> for crate::Error {
    fn from(e: FrameError) -> Self {
        match e {
            FrameError::Io(io) => crate::Error::Io(io),
            other => crate::Error::Protocol(other.to_string()),
        }
    }
}

impl From<PairingError> for crate::Error {
    fn from(e: PairingError) -> Self {
        crate::Error::Protocol(e.to_string())
    }
}
