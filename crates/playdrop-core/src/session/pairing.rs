//! Pairing, protocol.md section 6 (SR-6, SR-7, SR-8, SR-9).
//!
//! Pure functions and a state machine, no I/O:
//!
//! - [`sas`], [`commit`] and [`verify_commit`]: SAS pairing math (6.2).
//! - [`QrPayload`], [`proof_a`], [`proof_b`]: QR pairing (6.3).
//! - [`PairingSession`]: one attempt for either role and either method. Feed it frames, user
//!   actions and the clock; it returns frames to send and UI events.
//! - [`AddDeviceWindow`] and [`PairRequestLimiter`]: the 6.1 counters the host wires up.
//!
//! Time comes in as `Instant` arguments so tests can drive it. Nonces and secrets come in as
//! arguments ([`Nonce::random`] and [`QrSecret::random`] make fresh ones) so the state machine
//! is deterministic. Secrets, nonces and the SAS have redacting `Debug` impls (SR-32).

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use rand::Rng as _;
use sha2::{Digest, Sha256};

use super::frame::Envelope;
use super::hello::sanitize_name;
use super::types::{
    Frame, PairConfirm, PairMethod, PairReject, PairRequest, PairResponse, PairReveal, RejectReason,
};
use super::util::{b64_decode, b64_encode, hex_decode, hex_encode, is_device_id};
use crate::constants::{ADD_DEVICE_WINDOW_SECS, PAIRING_TIMEOUT_SECS};

/// Length of a DER `SubjectPublicKeyInfo` for Ed25519 (protocol.md 6.2).
pub const SPKI_DER_LEN: usize = 44;

/// Failed attempts that close the "Add device" window (protocol.md 6.1).
pub const MAX_FAILED_ATTEMPTS: u8 = 3;

/// Longest QR URI accepted by [`QrPayload::parse`].
pub const QR_URI_MAX_LEN: usize = 2048;

/// Most `addr` entries accepted by [`QrPayload::parse`].
pub const QR_MAX_ADDRS: usize = 16;

const QR_PREFIX: &str = "playdrop://pair?";
const QR_LABEL_A: &[u8] = b"pd-qr-a-v1";
const QR_LABEL_B: &[u8] = b"pd-qr-b-v1";

/// Errors from pairing inputs. Protocol failures during an attempt are reported as
/// [`PairingEvent`]s instead.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PairingError {
    /// Not a 64-character lowercase hex Device ID.
    #[error("invalid device id")]
    InvalidDeviceId,
    /// The SPKI is not 44 bytes of Ed25519 DER.
    #[error("invalid spki length")]
    InvalidSpki,
    /// SHA-256 of the SPKI is not the Device ID.
    #[error("device id does not match spki")]
    IdentityMismatch,
    /// Both parties have the same Device ID.
    #[error("cannot pair a device with itself")]
    SameDevice,
    /// SAS inputs not sorted as lowercase hex strings.
    #[error("sas device ids not sorted")]
    NotSorted,
    /// The QR URI is malformed.
    #[error("malformed qr payload: {0}")]
    QrMalformed(&'static str),
    /// The TLS peer is not the device in the QR code; abort before sending anything (6.3).
    #[error("peer certificate does not match the qr code")]
    QrPeerMismatch,
    /// "Add device" cannot open while the desktop is locked (6.1).
    #[error("desktop is locked")]
    Locked,
}

macro_rules! secret_bytes {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq)]
        pub struct $name([u8; 16]);

        impl $name {
            /// 16 fresh bytes from the OS-seeded thread CSPRNG.
            pub fn random() -> Self {
                let mut b = [0u8; 16];
                rand::rng().fill_bytes(&mut b);
                Self(b)
            }

            /// Wraps raw bytes.
            pub fn from_bytes(b: [u8; 16]) -> Self {
                Self(b)
            }

            /// The raw bytes.
            pub fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            /// Base64url without padding, as on the wire.
            pub fn to_wire(&self) -> String {
                b64_encode(&self.0)
            }

            /// Parses the wire form; `None` unless it is exactly 16 bytes.
            pub fn from_wire(s: &str) -> Option<Self> {
                b64_decode::<16>(s).map(Self)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "(redacted)"))
            }
        }
    };
}

secret_bytes! {
    /// A 16-byte pairing nonce, `nonce_i` or `nonce_r` (protocol.md 6.2). Fresh per attempt.
    Nonce
}

secret_bytes! {
    /// The 16-byte one-time QR secret (protocol.md 6.3, SR-9).
    QrSecret
}

/// A six-digit short authentication string (protocol.md 6.2). `Debug` is redacted (SR-32).
#[derive(Clone, PartialEq, Eq)]
pub struct Sas(String);

impl Sas {
    /// The six digits, for display.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Sas {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Sas(redacted)")
    }
}

/// `commit = hex BLAKE3(nonce_i)` (protocol.md 6.2).
pub fn commit(nonce_i: &Nonce) -> String {
    hex_encode(blake3::hash(nonce_i.as_bytes()).as_bytes())
}

/// Checks a revealed `nonce_i` against a commit, in constant time. A commit that is not
/// 64 hex characters never matches.
pub fn verify_commit(commit_hex: &str, nonce_i: &Nonce) -> bool {
    match hex_decode::<32>(commit_hex) {
        // blake3::Hash equality is constant-time.
        Some(c) => blake3::Hash::from_bytes(c) == blake3::hash(nonce_i.as_bytes()),
        None => false,
    }
}

/// The SAS formula of protocol.md 6.2, over Device IDs already sorted as lowercase hex
/// strings (`lo < hi`) and the SPKI DER of the device with each ID.
///
/// `h = BLAKE3(lo || hi || spki_lo || spki_hi || nonce_i || nonce_r || ekm)`, then the first
/// 20 bits of `h` big-endian, mod 1,000,000, as 6 zero-padded digits. Inputs are fixed length
/// (64, 64, 44, 44, 16, 16, 32 bytes) so no separators are used.
pub fn sas(
    lo_id_hex: &str,
    hi_id_hex: &str,
    spki_lo_der: &[u8],
    spki_hi_der: &[u8],
    nonce_i: &Nonce,
    nonce_r: &Nonce,
    ekm: &[u8; 32],
) -> Result<Sas, PairingError> {
    if !is_device_id(lo_id_hex) || !is_device_id(hi_id_hex) {
        return Err(PairingError::InvalidDeviceId);
    }
    if lo_id_hex >= hi_id_hex {
        return Err(if lo_id_hex == hi_id_hex {
            PairingError::SameDevice
        } else {
            PairingError::NotSorted
        });
    }
    if spki_lo_der.len() != SPKI_DER_LEN || spki_hi_der.len() != SPKI_DER_LEN {
        return Err(PairingError::InvalidSpki);
    }
    let mut h = blake3::Hasher::new();
    h.update(lo_id_hex.as_bytes());
    h.update(hi_id_hex.as_bytes());
    h.update(spki_lo_der);
    h.update(spki_hi_der);
    h.update(nonce_i.as_bytes());
    h.update(nonce_r.as_bytes());
    h.update(ekm);
    let h = h.finalize();
    let h = h.as_bytes();
    let x = (u32::from(h[0]) << 12) | (u32::from(h[1]) << 4) | (u32::from(h[2]) >> 4);
    Ok(Sas(format!("{:06}", x % 1_000_000)))
}

/// [`sas`] for two parties in any order: sorts by Device ID and pairs each SPKI with its ID.
pub fn sas_for(
    a: &Party,
    b: &Party,
    nonce_i: &Nonce,
    nonce_r: &Nonce,
    ekm: &[u8; 32],
) -> Result<Sas, PairingError> {
    let (lo, hi) = if a.device_id <= b.device_id {
        (a, b)
    } else {
        (b, a)
    };
    sas(
        &lo.device_id,
        &hi.device_id,
        &lo.spki_der,
        &hi.spki_der,
        nonce_i,
        nonce_r,
        ekm,
    )
}

type HmacSha256 = Hmac<Sha256>;

fn qr_mac(
    secret: &QrSecret,
    label: &[u8],
    first: &[u8; 32],
    second: &[u8; 32],
    ekm: &[u8; 32],
) -> HmacSha256 {
    let mut m = <HmacSha256 as KeyInit>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts keys of any length");
    m.update(label);
    m.update(first);
    m.update(second);
    m.update(ekm);
    m
}

fn mac_bytes(m: HmacSha256) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&m.finalize().into_bytes());
    out
}

/// `proof_b = HMAC-SHA256(secret, "pd-qr-b-v1" || spkihash_A || spkihash_B || ekm)`, sent by
/// the scanner B (protocol.md 6.3). A is the displayer.
pub fn proof_b(
    secret: &QrSecret,
    spkihash_a: &[u8; 32],
    spkihash_b: &[u8; 32],
    ekm: &[u8; 32],
) -> [u8; 32] {
    mac_bytes(qr_mac(secret, QR_LABEL_B, spkihash_a, spkihash_b, ekm))
}

/// `proof_a = HMAC-SHA256(secret, "pd-qr-a-v1" || spkihash_B || spkihash_A || ekm)`, sent by
/// the displayer A (protocol.md 6.3).
pub fn proof_a(
    secret: &QrSecret,
    spkihash_a: &[u8; 32],
    spkihash_b: &[u8; 32],
    ekm: &[u8; 32],
) -> [u8; 32] {
    mac_bytes(qr_mac(secret, QR_LABEL_A, spkihash_b, spkihash_a, ekm))
}

/// Verifies `proof_b` in constant time.
pub fn verify_proof_b(
    secret: &QrSecret,
    spkihash_a: &[u8; 32],
    spkihash_b: &[u8; 32],
    ekm: &[u8; 32],
    proof: &[u8],
) -> bool {
    qr_mac(secret, QR_LABEL_B, spkihash_a, spkihash_b, ekm)
        .verify_slice(proof)
        .is_ok()
}

/// Verifies `proof_a` in constant time.
pub fn verify_proof_a(
    secret: &QrSecret,
    spkihash_a: &[u8; 32],
    spkihash_b: &[u8; 32],
    ekm: &[u8; 32],
    proof: &[u8],
) -> bool {
    qr_mac(secret, QR_LABEL_A, spkihash_b, spkihash_a, ekm)
        .verify_slice(proof)
        .is_ok()
}

/// The QR pairing payload, protocol.md 6.3:
/// `playdrop://pair?id=&fp=&addr=...&secret=&exp=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPayload {
    /// Displayer's Device ID.
    pub device_id: String,
    /// Displayer's certificate fingerprint, lowercase hex SHA-256 of the DER certificate.
    pub fingerprint: String,
    /// Addresses to dial; IPv6 is bracketed on the wire.
    pub addrs: Vec<SocketAddr>,
    /// The one-time secret.
    pub secret: QrSecret,
    /// Expiry, Unix seconds, at most 120 s after display.
    pub exp: u64,
}

fn is_unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// RFC 3986 percent-encoding for query values. `:` is a valid query character and is kept
/// raw; `[` and `]` are not and are encoded.
fn percent_encode(s: &str, out: &mut String) {
    for &b in s.as_bytes() {
        if is_unreserved(b) || b == b':' {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push_str(&hex_encode(&[b]).to_ascii_uppercase());
        }
    }
}

fn percent_decode(s: &str) -> Result<String, PairingError> {
    let raw = s.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' {
            let hex = s
                .get(i + 1..i + 3)
                .ok_or(PairingError::QrMalformed("bad escape"))?;
            let [b] = hex_decode::<1>(hex).ok_or(PairingError::QrMalformed("bad escape"))?;
            out.push(b);
            i += 3;
        } else {
            out.push(raw[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| PairingError::QrMalformed("not utf-8"))
}

fn is_lower_hex64(s: &str) -> bool {
    is_device_id(s)
}

impl QrPayload {
    /// Builds the URI with percent-encoded values.
    pub fn to_uri(&self) -> String {
        let mut s = String::with_capacity(256);
        s.push_str(QR_PREFIX);
        s.push_str("id=");
        percent_encode(&self.device_id, &mut s);
        s.push_str("&fp=");
        percent_encode(&self.fingerprint, &mut s);
        for a in &self.addrs {
            s.push_str("&addr=");
            percent_encode(&a.to_string(), &mut s);
        }
        s.push_str("&secret=");
        percent_encode(&self.secret.to_wire(), &mut s);
        s.push_str("&exp=");
        s.push_str(&self.exp.to_string());
        s
    }

    /// Parses a scanned URI. Values may be percent-encoded or raw (raw brackets around IPv6
    /// are accepted, as in the spec example). Unknown keys are ignored; repeated single keys
    /// and a missing key, address or valid secret are errors.
    pub fn parse(uri: &str) -> Result<Self, PairingError> {
        if uri.len() > QR_URI_MAX_LEN {
            return Err(PairingError::QrMalformed("too long"));
        }
        let scheme_ok = uri
            .get(..QR_PREFIX.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(QR_PREFIX));
        if !scheme_ok {
            return Err(PairingError::QrMalformed("not a playdrop pair uri"));
        }
        let query = &uri[QR_PREFIX.len()..];
        if query.contains('#') {
            return Err(PairingError::QrMalformed("fragment"));
        }
        let (mut id, mut fp, mut secret, mut exp) = (None, None, None, None);
        let mut addrs = Vec::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (k, v) = pair
                .split_once('=')
                .ok_or(PairingError::QrMalformed("pair without ="))?;
            let v = percent_decode(v)?;
            let slot = match k {
                "id" => &mut id,
                "fp" => &mut fp,
                "secret" => &mut secret,
                "exp" => &mut exp,
                "addr" => {
                    if addrs.len() == QR_MAX_ADDRS {
                        return Err(PairingError::QrMalformed("too many addresses"));
                    }
                    let a: SocketAddr = v
                        .parse()
                        .map_err(|_| PairingError::QrMalformed("bad addr"))?;
                    addrs.push(a);
                    continue;
                }
                _ => continue,
            };
            if slot.replace(v).is_some() {
                return Err(PairingError::QrMalformed("repeated key"));
            }
        }
        let device_id = id.ok_or(PairingError::QrMalformed("missing id"))?;
        let fingerprint = fp.ok_or(PairingError::QrMalformed("missing fp"))?;
        if !is_device_id(&device_id) {
            return Err(PairingError::QrMalformed("bad id"));
        }
        if !is_lower_hex64(&fingerprint) {
            return Err(PairingError::QrMalformed("bad fp"));
        }
        if addrs.is_empty() {
            return Err(PairingError::QrMalformed("missing addr"));
        }
        let secret = secret
            .as_deref()
            .and_then(QrSecret::from_wire)
            .ok_or(PairingError::QrMalformed("bad secret"))?;
        let exp = exp
            .ok_or(PairingError::QrMalformed("missing exp"))?
            .parse::<u64>()
            .map_err(|_| PairingError::QrMalformed("bad exp"))?;
        Ok(Self {
            device_id,
            fingerprint,
            addrs,
            secret,
            exp,
        })
    }

    /// True if the TLS peer is the device in the code: both the Device ID and the
    /// certificate fingerprint match (protocol.md 6.3, SR-9).
    pub fn matches_peer(&self, peer_device_id: &str, peer_cert_fingerprint: &str) -> bool {
        self.device_id == peer_device_id && self.fingerprint == peer_cert_fingerprint
    }
}

/// One side of a pairing: a Device ID and the SPKI DER seen in this TLS handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Party {
    /// 64-character lowercase hex Device ID.
    pub device_id: String,
    /// DER `SubjectPublicKeyInfo`, 44 bytes for Ed25519.
    pub spki_der: Vec<u8>,
}

impl Party {
    /// Builds a party.
    pub fn new(device_id: impl Into<String>, spki_der: impl Into<Vec<u8>>) -> Self {
        Self {
            device_id: device_id.into(),
            spki_der: spki_der.into(),
        }
    }
}

/// Everything a pairing attempt needs from the connection, checked once. `Debug` leaves out
/// the exporter value.
#[derive(Clone)]
pub struct PairingContext {
    local: Party,
    peer: Party,
    local_hash: [u8; 32],
    peer_hash: [u8; 32],
    ekm: [u8; 32],
    peer_name: String,
    peer_short_id: String,
}

fn spki_hash(p: &Party) -> Result<[u8; 32], PairingError> {
    if !is_device_id(&p.device_id) {
        return Err(PairingError::InvalidDeviceId);
    }
    if p.spki_der.len() != SPKI_DER_LEN {
        return Err(PairingError::InvalidSpki);
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(&Sha256::digest(&p.spki_der));
    if hex_encode(&h) != p.device_id {
        return Err(PairingError::IdentityMismatch);
    }
    Ok(h)
}

impl fmt::Debug for PairingContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingContext")
            .field("local", &self.local.device_id)
            .field("peer", &self.peer.device_id)
            .finish_non_exhaustive()
    }
}

impl PairingContext {
    /// Checks both parties (Device ID = SHA-256 of the SPKI, protocol.md section 2) and stores
    /// the TLS exporter value `EXPORTER-playdrop-pair` (6.2). `peer_name` is the peer's
    /// `hello.name`, sanitised here (SR-8); `peer_short_id` is the peer's Short ID.
    pub fn new(
        local: Party,
        peer: Party,
        ekm: [u8; 32],
        peer_name: &str,
        peer_short_id: impl Into<String>,
    ) -> Result<Self, PairingError> {
        let local_hash = spki_hash(&local)?;
        let peer_hash = spki_hash(&peer)?;
        if local.device_id == peer.device_id {
            return Err(PairingError::SameDevice);
        }
        Ok(Self {
            local,
            peer,
            local_hash,
            peer_hash,
            ekm,
            peer_name: sanitize_name(peer_name),
            peer_short_id: peer_short_id.into(),
        })
    }

    /// The peer's Device ID.
    pub fn peer_id(&self) -> &str {
        &self.peer.device_id
    }
}

/// A live QR offer held by the displayer: the secret and when it dies. Not `Clone`: handing
/// it to [`PairingSession::responder`] consumes it, so the first attempt uses it up (SR-9).
#[derive(Debug, PartialEq, Eq)]
pub struct QrOffer {
    secret: QrSecret,
    expires_at: Instant,
}

impl QrOffer {
    /// An offer for `secret` that dies at `expires_at` (at most 120 s after display).
    pub fn new(secret: QrSecret, expires_at: Instant) -> Self {
        Self { secret, expires_at }
    }
}

/// Which side of the attempt this host is (protocol.md 6.1). The initiator sends
/// `pair_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Picked the peer from its list (SAS) or scanned the code (QR).
    Initiator,
    /// Received the `pair_request`.
    Responder,
}

/// Something the host must do, in order.
#[derive(Debug, Clone, PartialEq)]
pub enum PairingOutput {
    /// Send this frame on the session stream, with `re` if set.
    Send {
        /// The frame body.
        frame: Frame,
        /// The `seq` it answers.
        re: Option<u64>,
    },
    /// Tell the UI.
    Event(PairingEvent),
}

/// UI and host events from a [`PairingSession`].
#[derive(Debug, Clone, PartialEq)]
pub enum PairingEvent {
    /// Show the SAS with the sanitised name and Short ID and two buttons, neither focused
    /// (6.2, SR-8). Call [`PairingSession::confirm`] or [`PairingSession::reject`].
    ShowSas {
        /// Six digits.
        sas: Sas,
        /// Sanitised peer name.
        name: String,
        /// Peer Short ID.
        short_id: String,
    },
    /// QR displayer: ask "`Add <name> (<Short ID>)?`" (6.3). Pinning waits for one tap.
    AskConfirmScanner {
        /// Sanitised peer name.
        name: String,
        /// Peer Short ID.
        short_id: String,
    },
    /// Pin the peer now with default grants, then send `capabilities` and `announce_key`
    /// (6.1). Counts as success for [`AddDeviceWindow::finish`].
    Paired {
        /// Peer Device ID.
        peer_id: String,
        /// Peer SPKI hash.
        peer_spki_hash: [u8; 32],
        /// How it was paired.
        method: PairMethod,
    },
    /// The attempt ended with `pair_reject`. Flush pending frames, then close the connection.
    Rejected {
        /// Why.
        reason: RejectReason,
        /// True if the peer sent the reject.
        by_peer: bool,
    },
    /// The peer broke the pairing protocol. Close the connection without a `pair_reject`.
    Aborted {
        /// What went wrong, for logs.
        detail: &'static str,
    },
}

#[derive(Debug)]
enum State {
    SasAwaitResponse {
        nonce_i: Nonce,
    },
    AwaitRequest {
        nonce_r: Nonce,
        qr: Option<QrOffer>,
    },
    SasAwaitReveal {
        commit: String,
        nonce_r: Nonce,
    },
    SasConfirming {
        local_confirmed: bool,
        peer_confirmed: bool,
    },
    QrAwaitConfirm {
        secret: QrSecret,
    },
    QrAwaitUser {
        secret: QrSecret,
        request_seq: u64,
    },
    Done,
}

/// One pairing attempt, either role, either method (protocol.md 6.2, 6.3).
///
/// The host creates one per attempt, at most one at a time ([`AddDeviceWindow::try_begin`]),
/// routes every `pair_*` frame on the connection to [`PairingSession::handle_frame`], calls
/// [`PairingSession::poll`] at [`PairingSession::deadline`], and performs the returned
/// [`PairingOutput`]s in order.
#[derive(Debug)]
pub struct PairingSession {
    ctx: PairingContext,
    role: Role,
    method: PairMethod,
    state: State,
    deadline: Instant,
}

fn send(frame: Frame, re: Option<u64>) -> PairingOutput {
    PairingOutput::Send { frame, re }
}

fn event(e: PairingEvent) -> PairingOutput {
    PairingOutput::Event(e)
}

impl PairingSession {
    fn new(
        ctx: PairingContext,
        role: Role,
        method: PairMethod,
        state: State,
        now: Instant,
    ) -> Self {
        Self {
            ctx,
            role,
            method,
            state,
            deadline: now + Duration::from_secs(PAIRING_TIMEOUT_SECS),
        }
    }

    /// SAS initiator: sends `pair_request` with the commit to `nonce_i`.
    pub fn sas_initiator(
        ctx: PairingContext,
        now: Instant,
        nonce_i: Nonce,
    ) -> (Self, Vec<PairingOutput>) {
        let req = Frame::PairRequest(PairRequest {
            method: PairMethod::Sas,
            commit: Some(commit(&nonce_i)),
            proof: None,
        });
        let s = Self::new(
            ctx,
            Role::Initiator,
            PairMethod::Sas,
            State::SasAwaitResponse { nonce_i },
            now,
        );
        (s, vec![send(req, None)])
    }

    /// QR scanner (B): checks that the TLS peer matches the code before sending anything,
    /// then sends `pair_request` with `proof_b` (6.3).
    pub fn qr_initiator(
        ctx: PairingContext,
        now: Instant,
        payload: &QrPayload,
        peer_cert_fingerprint: &str,
    ) -> Result<(Self, Vec<PairingOutput>), PairingError> {
        if !payload.matches_peer(&ctx.peer.device_id, peer_cert_fingerprint) {
            return Err(PairingError::QrPeerMismatch);
        }
        let secret = payload.secret.clone();
        // A is the peer (displayer), B is us (scanner).
        let proof = proof_b(&secret, &ctx.peer_hash, &ctx.local_hash, &ctx.ekm);
        let req = Frame::PairRequest(PairRequest {
            method: PairMethod::Qr,
            commit: None,
            proof: Some(hex_encode(&proof)),
        });
        let s = Self::new(
            ctx,
            Role::Initiator,
            PairMethod::Qr,
            State::QrAwaitConfirm { secret },
            now,
        );
        Ok((s, vec![send(req, None)]))
    }

    /// Responder: create when a `pair_request` arrives (after the window, busy and rate
    /// checks), then pass that frame to [`PairingSession::handle_frame`]. `qr` is the live QR
    /// offer if this host is showing a code; it is consumed by this attempt either way.
    pub fn responder(
        ctx: PairingContext,
        now: Instant,
        nonce_r: Nonce,
        qr: Option<QrOffer>,
    ) -> Self {
        Self::new(
            ctx,
            Role::Responder,
            PairMethod::Sas,
            State::AwaitRequest { nonce_r, qr },
            now,
        )
    }

    /// This host's role.
    pub fn role(&self) -> Role {
        self.role
    }

    /// The method in use (for a responder, known once the request arrives).
    pub fn method(&self) -> &PairMethod {
        &self.method
    }

    /// When the 30 s attempt timeout fires (6.2).
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    /// True once the attempt has ended (paired, rejected or aborted).
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done)
    }

    /// Fires the timeout if `now` is at or past the deadline: sends `pair_reject` `timeout`.
    pub fn poll(&mut self, now: Instant) -> Vec<PairingOutput> {
        let mut out = Vec::new();
        self.check_timeout(now, &mut out);
        out
    }

    fn check_timeout(&mut self, now: Instant, out: &mut Vec<PairingOutput>) -> bool {
        if matches!(self.state, State::Done) {
            return true;
        }
        if now >= self.deadline {
            self.reject_local(RejectReason::Timeout, out);
            return true;
        }
        false
    }

    fn reject_local(&mut self, reason: RejectReason, out: &mut Vec<PairingOutput>) {
        self.state = State::Done;
        out.push(send(
            Frame::PairReject(PairReject {
                reason: reason.clone(),
            }),
            None,
        ));
        out.push(event(PairingEvent::Rejected {
            reason,
            by_peer: false,
        }));
    }

    fn abort(&mut self, detail: &'static str, out: &mut Vec<PairingOutput>) {
        self.state = State::Done;
        out.push(event(PairingEvent::Aborted { detail }));
    }

    fn paired(&mut self, out: &mut Vec<PairingOutput>) {
        self.state = State::Done;
        out.push(event(PairingEvent::Paired {
            peer_id: self.ctx.peer.device_id.clone(),
            peer_spki_hash: self.ctx.peer_hash,
            method: self.method.clone(),
        }));
    }

    fn compute_sas(&self, nonce_i: &Nonce, nonce_r: &Nonce) -> Result<Sas, PairingError> {
        sas_for(
            &self.ctx.local,
            &self.ctx.peer,
            nonce_i,
            nonce_r,
            &self.ctx.ekm,
        )
    }

    fn show_sas(&mut self, nonce_i: &Nonce, nonce_r: &Nonce, out: &mut Vec<PairingOutput>) {
        match self.compute_sas(nonce_i, nonce_r) {
            Ok(sas) => {
                self.state = State::SasConfirming {
                    local_confirmed: false,
                    peer_confirmed: false,
                };
                out.push(event(PairingEvent::ShowSas {
                    sas,
                    name: self.ctx.peer_name.clone(),
                    short_id: self.ctx.peer_short_id.clone(),
                }));
            }
            // Unreachable: the context was validated in PairingContext::new.
            Err(_) => self.abort("sas inputs invalid", out),
        }
    }

    /// Feeds an inbound frame. Non-pairing frames are ignored. A `pair_reject` from the peer
    /// ends the attempt in any state.
    pub fn handle_frame(&mut self, now: Instant, env: &Envelope) -> Vec<PairingOutput> {
        let mut out = Vec::new();
        let is_pairing = matches!(
            env.frame,
            Frame::PairRequest(_)
                | Frame::PairResponse(_)
                | Frame::PairReveal(_)
                | Frame::PairConfirm(_)
                | Frame::PairReject(_)
        );
        if !is_pairing || self.check_timeout(now, &mut out) {
            return out;
        }
        if let Frame::PairReject(r) = &env.frame {
            self.state = State::Done;
            out.push(event(PairingEvent::Rejected {
                reason: r.reason.clone(),
                by_peer: true,
            }));
            return out;
        }
        let state = std::mem::replace(&mut self.state, State::Done);
        match (state, &env.frame) {
            (State::SasAwaitResponse { nonce_i }, Frame::PairResponse(r)) => {
                let Some(nonce_r) = Nonce::from_wire(&r.nonce) else {
                    self.abort("bad nonce_r", &mut out);
                    return out;
                };
                out.push(send(
                    Frame::PairReveal(PairReveal {
                        nonce: nonce_i.to_wire(),
                    }),
                    None,
                ));
                self.show_sas(&nonce_i, &nonce_r, &mut out);
            }
            (State::AwaitRequest { nonce_r, qr }, Frame::PairRequest(req)) => {
                self.method = req.method.clone();
                self.handle_request(now, env.seq, req, nonce_r, qr, &mut out);
            }
            (State::SasAwaitReveal { commit, nonce_r }, Frame::PairReveal(r)) => {
                let Some(nonce_i) = Nonce::from_wire(&r.nonce) else {
                    self.abort("bad nonce_i", &mut out);
                    return out;
                };
                if verify_commit(&commit, &nonce_i) {
                    self.show_sas(&nonce_i, &nonce_r, &mut out);
                } else {
                    self.reject_local(RejectReason::CommitMismatch, &mut out);
                }
            }
            (
                State::SasConfirming {
                    local_confirmed,
                    peer_confirmed: false,
                },
                Frame::PairConfirm(_),
            ) => {
                if local_confirmed {
                    self.paired(&mut out);
                } else {
                    self.state = State::SasConfirming {
                        local_confirmed,
                        peer_confirmed: true,
                    };
                }
            }
            (State::QrAwaitConfirm { secret }, Frame::PairConfirm(c)) => {
                let valid = c
                    .proof
                    .as_deref()
                    .and_then(hex_decode::<32>)
                    .is_some_and(|p| {
                        // A is the peer (displayer), B is us (scanner).
                        verify_proof_a(
                            &secret,
                            &self.ctx.peer_hash,
                            &self.ctx.local_hash,
                            &self.ctx.ekm,
                            &p,
                        )
                    });
                if valid {
                    self.paired(&mut out);
                } else {
                    self.reject_local(RejectReason::QrProofInvalid, &mut out);
                }
            }
            _ => self.abort("unexpected pairing frame", &mut out),
        }
        out
    }

    fn handle_request(
        &mut self,
        now: Instant,
        seq: u64,
        req: &PairRequest,
        nonce_r: Nonce,
        qr: Option<QrOffer>,
        out: &mut Vec<PairingOutput>,
    ) {
        match req.method {
            PairMethod::Sas => {
                let Some(commit) = req.commit.clone().filter(|c| hex_decode::<32>(c).is_some())
                else {
                    self.abort("bad commit", out);
                    return;
                };
                out.push(send(
                    Frame::PairResponse(PairResponse {
                        nonce: nonce_r.to_wire(),
                    }),
                    Some(seq),
                ));
                self.state = State::SasAwaitReveal { commit, nonce_r };
            }
            PairMethod::Qr => {
                // The offer is consumed whatever happens next (SR-9).
                let Some(offer) = qr.filter(|o| now < o.expires_at) else {
                    self.reject_local(RejectReason::QrExpired, out);
                    return;
                };
                let valid = req
                    .proof
                    .as_deref()
                    .and_then(hex_decode::<32>)
                    .is_some_and(|p| {
                        // A is us (displayer), B is the peer (scanner).
                        verify_proof_b(
                            &offer.secret,
                            &self.ctx.local_hash,
                            &self.ctx.peer_hash,
                            &self.ctx.ekm,
                            &p,
                        )
                    });
                if !valid {
                    self.reject_local(RejectReason::QrProofInvalid, out);
                    return;
                }
                self.state = State::QrAwaitUser {
                    secret: offer.secret,
                    request_seq: seq,
                };
                out.push(event(PairingEvent::AskConfirmScanner {
                    name: self.ctx.peer_name.clone(),
                    short_id: self.ctx.peer_short_id.clone(),
                }));
            }
            PairMethod::Unknown(_) => self.abort("unknown pairing method", out),
        }
    }

    /// The local user tapped "Pair" (SAS) or "Add" (QR displayer). Ignored in states where
    /// there is nothing to confirm.
    pub fn confirm(&mut self, now: Instant) -> Vec<PairingOutput> {
        let mut out = Vec::new();
        if self.check_timeout(now, &mut out) {
            return out;
        }
        match std::mem::replace(&mut self.state, State::Done) {
            State::SasConfirming {
                local_confirmed: false,
                peer_confirmed,
            } => {
                out.push(send(Frame::PairConfirm(PairConfirm::default()), None));
                if peer_confirmed {
                    self.paired(&mut out);
                } else {
                    self.state = State::SasConfirming {
                        local_confirmed: true,
                        peer_confirmed,
                    };
                }
            }
            State::QrAwaitUser {
                secret,
                request_seq,
            } => {
                // A is us (displayer), B is the peer (scanner).
                let proof = proof_a(
                    &secret,
                    &self.ctx.local_hash,
                    &self.ctx.peer_hash,
                    &self.ctx.ekm,
                );
                out.push(send(
                    Frame::PairConfirm(PairConfirm {
                        proof: Some(hex_encode(&proof)),
                    }),
                    Some(request_seq),
                ));
                self.paired(&mut out);
            }
            other => self.state = other,
        }
        out
    }

    /// The local user declined or closed the prompt: sends `pair_reject` `user`.
    pub fn reject(&mut self, now: Instant) -> Vec<PairingOutput> {
        let mut out = Vec::new();
        if !self.check_timeout(now, &mut out) {
            self.reject_local(RejectReason::User, &mut out);
        }
        out
    }
}

/// Why [`AddDeviceWindow::try_begin`] refused a `pair_request`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BeginRefusal {
    /// No window is open: close the connection with no UI (6.1, SR-6).
    Closed,
    /// Another attempt is in progress: send `pair_reject` `busy`.
    Busy,
}

#[derive(Debug, Clone)]
struct WindowState {
    opened_at: Instant,
    failures: u8,
    attempt_active: bool,
}

/// The "Add device" window (protocol.md 6.1, SR-6): open for 120 s, closed on success or after
/// 3 failed attempts, never opened while the desktop is locked, one attempt at a time.
#[derive(Debug, Clone, Default)]
pub struct AddDeviceWindow {
    state: Option<WindowState>,
}

impl AddDeviceWindow {
    /// A closed window.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens (or reopens) the window at the user's request.
    pub fn open(&mut self, now: Instant, desktop_locked: bool) -> Result<(), PairingError> {
        if desktop_locked {
            return Err(PairingError::Locked);
        }
        self.state = Some(WindowState {
            opened_at: now,
            failures: 0,
            attempt_active: false,
        });
        Ok(())
    }

    /// Closes the window (user closed it or the desktop locked).
    pub fn close(&mut self) {
        self.state = None;
    }

    /// True while unpinned certificates may enter unpaired mode. An attempt already running
    /// may finish after expiry; its own 30 s timeout bounds it.
    pub fn is_open(&self, now: Instant) -> bool {
        self.state.as_ref().is_some_and(|s| {
            s.failures < MAX_FAILED_ATTEMPTS
                && now.saturating_duration_since(s.opened_at)
                    < Duration::from_secs(ADD_DEVICE_WINDOW_SECS)
        })
    }

    /// Admits one pairing attempt.
    pub fn try_begin(&mut self, now: Instant) -> Result<(), BeginRefusal> {
        if !self.is_open(now) {
            return Err(BeginRefusal::Closed);
        }
        let s = self.state.as_mut().ok_or(BeginRefusal::Closed)?;
        if s.attempt_active {
            return Err(BeginRefusal::Busy);
        }
        s.attempt_active = true;
        Ok(())
    }

    /// Ends the current attempt. Success closes the window; a failure (any rejection, timeout
    /// or abort) counts toward the limit of 3.
    pub fn finish(&mut self, success: bool) {
        if success {
            self.state = None;
            return;
        }
        if let Some(s) = self.state.as_mut() {
            s.attempt_active = false;
            s.failures = s.failures.saturating_add(1);
            if s.failures >= MAX_FAILED_ATTEMPTS {
                self.state = None;
            }
        }
    }
}

/// At most 5 `pair_request` per minute per source IP (protocol.md 6.1); excess gets
/// `pair_reject` `rate_limited`. IPv4-mapped IPv6 counts as IPv4. Memory is bounded: each
/// address keeps at most `max` timestamps and idle addresses are pruned.
#[derive(Debug, Clone)]
pub struct PairRequestLimiter {
    max: usize,
    window: Duration,
    per_ip: HashMap<IpAddr, VecDeque<Instant>>,
}

impl Default for PairRequestLimiter {
    fn default() -> Self {
        Self::new(5)
    }
}

impl PairRequestLimiter {
    /// Number of tracked addresses above which idle entries are pruned.
    const PRUNE_AT: usize = 256;

    /// A limiter admitting `max_per_minute` requests per address.
    pub fn new(max_per_minute: usize) -> Self {
        Self {
            max: max_per_minute,
            window: Duration::from_secs(60),
            per_ip: HashMap::new(),
        }
    }

    /// Records a request from `ip` at `now`; false means refuse with `rate_limited`.
    pub fn check(&mut self, ip: IpAddr, now: Instant) -> bool {
        let window = self.window;
        let fresh = |t: &Instant| now.saturating_duration_since(*t) < window;
        if self.per_ip.len() >= Self::PRUNE_AT {
            self.per_ip.retain(|_, q| q.back().is_some_and(fresh));
        }
        let q = self.per_ip.entry(ip.to_canonical()).or_default();
        while q.front().is_some_and(|t| !fresh(t)) {
            q.pop_front();
        }
        if q.len() >= self.max {
            return false;
        }
        q.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPKI_PREFIX: [u8; 12] = [
        0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
    ];

    fn spki(fill: u8) -> Vec<u8> {
        let mut v = SPKI_PREFIX.to_vec();
        v.extend_from_slice(&[fill; 32]);
        v
    }

    fn party(fill: u8) -> Party {
        let der = spki(fill);
        Party::new(hex_encode(&Sha256::digest(&der)), der)
    }

    fn nonce(fill: u8) -> Nonce {
        Nonce::from_bytes([fill; 16])
    }

    const EKM: [u8; 32] = [0x33; 32];

    const NOTHING: [PairingOutput; 0] = [];

    fn ctx(local: u8, peer: u8, ekm: [u8; 32]) -> PairingContext {
        PairingContext::new(
            party(local),
            party(peer),
            ekm,
            "Peer\u{202E}name",
            "abcd-efgh-ijkl-mnop",
        )
        .unwrap()
    }

    fn env(seq: u64, re: Option<u64>, frame: Frame) -> Envelope {
        Envelope {
            v: 1,
            seq,
            re,
            frame,
        }
    }

    /// Delivers `outputs` to `to`, returning what `to` produced. Assigns `seq` per sender.
    fn deliver(
        outputs: &[PairingOutput],
        next_seq: &mut u64,
        to: &mut PairingSession,
        now: Instant,
    ) -> Vec<PairingOutput> {
        let mut produced = Vec::new();
        for o in outputs {
            if let PairingOutput::Send { frame, re } = o {
                produced.extend(to.handle_frame(now, &env(*next_seq, *re, frame.clone())));
                *next_seq += 1;
            }
        }
        produced
    }

    fn events(outputs: &[PairingOutput]) -> Vec<&PairingEvent> {
        outputs
            .iter()
            .filter_map(|o| match o {
                PairingOutput::Event(e) => Some(e),
                PairingOutput::Send { .. } => None,
            })
            .collect()
    }

    fn shown_sas(outputs: &[PairingOutput]) -> Sas {
        events(outputs)
            .into_iter()
            .find_map(|e| match e {
                PairingEvent::ShowSas { sas, .. } => Some(sas.clone()),
                _ => None,
            })
            .expect("ShowSas")
    }

    fn is_paired(outputs: &[PairingOutput]) -> bool {
        events(outputs)
            .iter()
            .any(|e| matches!(e, PairingEvent::Paired { .. }))
    }

    fn rejected(outputs: &[PairingOutput]) -> Option<(RejectReason, bool)> {
        events(outputs).into_iter().find_map(|e| match e {
            PairingEvent::Rejected { reason, by_peer } => Some((reason.clone(), *by_peer)),
            _ => None,
        })
    }

    /// Frozen SAS test vector. Inputs:
    /// - SPKI A = `302a300506032b6570032100` followed by 32 bytes of 0x11
    /// - SPKI B = `302a300506032b6570032100` followed by 32 bytes of 0x22
    /// - Device IDs = lowercase hex SHA-256 of each SPKI
    /// - `nonce_i` = 16 bytes of 0x01, `nonce_r` = 16 bytes of 0x02, `ekm` = 32 bytes of 0x33
    #[test]
    fn sas_test_vector() {
        let (a, b) = (party(0x11), party(0x22));
        assert_eq!(a.device_id, SAS_VECTOR_ID_A);
        assert_eq!(b.device_id, SAS_VECTOR_ID_B);
        let s = sas_for(&a, &b, &nonce(1), &nonce(2), &EKM).unwrap();
        assert_eq!(s.as_str(), SAS_VECTOR);
        // Order of the parties does not matter; the IDs are sorted.
        assert_eq!(sas_for(&b, &a, &nonce(1), &nonce(2), &EKM).unwrap(), s);
        // Any input change changes the result (for this vector).
        assert_ne!(sas_for(&a, &b, &nonce(2), &nonce(1), &EKM).unwrap(), s);
        assert_ne!(
            sas_for(&a, &b, &nonce(1), &nonce(2), &[0x34; 32]).unwrap(),
            s
        );
        assert_eq!(s.as_str().len(), 6);
        assert_eq!(format!("{s:?}"), "Sas(redacted)");
    }

    const SAS_VECTOR_ID_A: &str =
        "9067da37b36d6e9d9a6b86e593983cf05cd3e37f51c14b3ad7258c06cccdb3ce";
    const SAS_VECTOR_ID_B: &str =
        "c44dea2fa63bf6d10d96dea207d031a967d84f58bc1280bcec54776682874bdb";
    /// Cross-checked with an independent pure-Python BLAKE3.
    const SAS_VECTOR: &str = "982332";

    #[test]
    fn sas_rejects_bad_inputs() {
        let (a, b) = (party(0x11), party(0x22));
        let (lo, hi) = if a.device_id < b.device_id {
            (&a, &b)
        } else {
            (&b, &a)
        };
        let n = nonce(1);
        assert_eq!(
            sas(
                &hi.device_id,
                &lo.device_id,
                &hi.spki_der,
                &lo.spki_der,
                &n,
                &n,
                &EKM
            ),
            Err(PairingError::NotSorted)
        );
        assert_eq!(
            sas(
                &lo.device_id,
                &lo.device_id,
                &lo.spki_der,
                &lo.spki_der,
                &n,
                &n,
                &EKM
            ),
            Err(PairingError::SameDevice)
        );
        assert_eq!(
            sas(
                &lo.device_id,
                &hi.device_id,
                &lo.spki_der[1..],
                &hi.spki_der,
                &n,
                &n,
                &EKM
            ),
            Err(PairingError::InvalidSpki)
        );
        let mut forged = party(0x22);
        forged.spki_der = spki(0x23);
        assert_eq!(
            PairingContext::new(party(0x11), forged, EKM, "x", "y").unwrap_err(),
            PairingError::IdentityMismatch
        );
    }

    #[test]
    fn sas_bit_extraction() {
        // (h[0] << 12) | (h[1] << 4) | (h[2] >> 4) for h = ff ff ff is 0xfffff = 1048575,
        // which mod 10^6 is 048575.
        let x = (0xffu32 << 12) | (0xffu32 << 4) | (0xffu32 >> 4);
        assert_eq!(x, 1_048_575);
        assert_eq!(format!("{:06}", x % 1_000_000), "048575");
    }

    #[test]
    fn commit_and_mismatch() {
        let n = nonce(7);
        let c = commit(&n);
        assert_eq!(c.len(), 64);
        assert_eq!(c, blake3::hash(&[7u8; 16]).to_hex().as_str());
        assert!(verify_commit(&c, &n));
        assert!(verify_commit(&c.to_uppercase(), &n));
        assert!(!verify_commit(&c, &nonce(8)));
        assert!(!verify_commit("zz", &n));
    }

    #[test]
    fn nonce_wire_form() {
        let n = Nonce::random();
        let w = n.to_wire();
        assert_eq!(w.len(), 22);
        assert!(!w.contains('='));
        assert_eq!(Nonce::from_wire(&w), Some(n));
        assert!(Nonce::from_wire("q8pZ2mVt4kQ1xR7bN0cWfA").is_some());
        assert_eq!(format!("{:?}", Nonce::random()), "Nonce(redacted)");
        assert_ne!(Nonce::random(), Nonce::random());
    }

    fn sample_qr() -> QrPayload {
        QrPayload {
            device_id: party(0x11).device_id,
            fingerprint: hex_encode(&[0xa8; 32]),
            addrs: vec![
                "192.168.1.20:42187".parse().unwrap(),
                "[fd00::20]:42187".parse().unwrap(),
            ],
            secret: QrSecret::from_bytes([5; 16]),
            exp: 1_791_460_920,
        }
    }

    #[test]
    fn qr_round_trip_with_ipv6() {
        let q = sample_qr();
        let uri = q.to_uri();
        assert!(uri.starts_with("playdrop://pair?id="));
        assert!(uri.contains("&addr=192.168.1.20:42187&addr=%5Bfd00::20%5D:42187&"));
        assert_eq!(QrPayload::parse(&uri).unwrap(), q);
        // Raw brackets as in the spec example parse too.
        let raw = uri.replace("%5B", "[").replace("%5D", "]");
        assert_eq!(QrPayload::parse(&raw).unwrap(), q);
        // Unknown keys are ignored.
        assert_eq!(QrPayload::parse(&format!("{uri}&future=1")).unwrap(), q);
    }

    #[test]
    fn qr_parse_rejects_bad_input() {
        let uri = sample_qr().to_uri();
        let bad = [
            uri.replace("playdrop://pair?", "https://pair?"),
            uri.replace("&exp=", "&exp=x"),
            format!("{uri}&id={}", party(0x22).device_id),
            uri.replace("192.168.1.20:42187", "nonsense"),
            uri.replace("secret=", "secret=A"),
            format!("{uri}#frag"),
            uri.replace("%5B", "%5"),
            "playdrop://pair?id=x".to_owned(),
            format!("{uri}{}", "&addr=1.2.3.4:1".repeat(20)),
        ];
        for b in bad {
            assert!(QrPayload::parse(&b).is_err(), "{b}");
        }
        let no_addr = format!(
            "playdrop://pair?id={}&fp={}&secret={}&exp=1",
            party(0x11).device_id,
            hex_encode(&[0xa8; 32]),
            QrSecret::from_bytes([5; 16]).to_wire()
        );
        assert_eq!(
            QrPayload::parse(&no_addr),
            Err(PairingError::QrMalformed("missing addr"))
        );
    }

    #[test]
    fn qr_proofs() {
        let s = QrSecret::from_bytes([5; 16]);
        let (ha, hb) = ([0xaa; 32], [0xbb; 32]);
        let pb = proof_b(&s, &ha, &hb, &EKM);
        let pa = proof_a(&s, &ha, &hb, &EKM);
        assert_ne!(pa, pb);
        assert!(verify_proof_b(&s, &ha, &hb, &EKM, &pb));
        assert!(verify_proof_a(&s, &ha, &hb, &EKM, &pa));
        // Wrong secret, swapped roles, wrong exporter, truncated proof.
        assert!(!verify_proof_b(
            &QrSecret::from_bytes([6; 16]),
            &ha,
            &hb,
            &EKM,
            &pb
        ));
        assert!(!verify_proof_b(&s, &hb, &ha, &EKM, &pb));
        assert!(!verify_proof_a(&s, &ha, &hb, &EKM, &pb));
        assert!(!verify_proof_b(&s, &ha, &hb, &[0; 32], &pb));
        assert!(!verify_proof_b(&s, &ha, &hb, &EKM, &pb[..31]));
        // proof_a is proof_b's construction with the other label and swapped hashes.
        let manual = mac_bytes(qr_mac(&s, b"pd-qr-a-v1", &hb, &ha, &EKM));
        assert_eq!(manual, pa);
    }

    #[test]
    fn full_sas_pairing() {
        let t0 = Instant::now();
        let (mut i, out_i) = PairingSession::sas_initiator(ctx(0x11, 0x22, EKM), t0, nonce(1));
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        let (mut seq_i, mut seq_r) = (1, 1);
        let out_r = deliver(&out_i, &mut seq_i, &mut r, t0); // request -> response
        assert!(matches!(
            out_r[0],
            PairingOutput::Send {
                frame: Frame::PairResponse(_),
                re: Some(1)
            }
        ));
        let out_i = deliver(&out_r, &mut seq_r, &mut i, t0); // response -> reveal + SAS
        let sas_i = shown_sas(&out_i);
        let out_r = deliver(&out_i, &mut seq_i, &mut r, t0); // reveal -> SAS
        let sas_r = shown_sas(&out_r);
        assert_eq!(sas_i, sas_r);
        let Some(PairingEvent::ShowSas { name, short_id, .. }) = events(&out_r).pop().cloned()
        else {
            panic!()
        };
        assert_eq!(name, "Peername");
        assert_eq!(short_id, "abcd-efgh-ijkl-mnop");
        // Responder confirms first, then the initiator.
        let conf_r = r.confirm(t0);
        assert!(!is_paired(&conf_r));
        let after_i = deliver(&conf_r, &mut seq_r, &mut i, t0);
        assert_eq!(after_i, NOTHING);
        let conf_i = i.confirm(t0);
        assert!(is_paired(&conf_i));
        let after_r = deliver(&conf_i, &mut seq_i, &mut r, t0);
        assert!(is_paired(&after_r));
        assert!(i.is_finished() && r.is_finished());
        let Some(PairingEvent::Paired {
            peer_id, method, ..
        }) = events(&after_r).pop().cloned()
        else {
            panic!()
        };
        assert_eq!(peer_id, party(0x11).device_id);
        assert_eq!(method, PairMethod::Sas);
    }

    #[test]
    fn mitm_sees_different_sas() {
        // Two TLS sessions with different exporter values give different SAS (SR-7 test).
        let t0 = Instant::now();
        let (mut i, out_i) = PairingSession::sas_initiator(ctx(0x11, 0x22, EKM), t0, nonce(1));
        let mut r = PairingSession::responder(ctx(0x22, 0x11, [0x44; 32]), t0, nonce(2), None);
        let mut s = 1;
        let out_r = deliver(&out_i, &mut s, &mut r, t0);
        let out_i = deliver(&out_r, &mut s, &mut i, t0);
        let out_r = deliver(&out_i, &mut s, &mut r, t0);
        assert_ne!(shown_sas(&out_i), shown_sas(&out_r));
    }

    #[test]
    fn wrong_reveal_is_commit_mismatch() {
        let t0 = Instant::now();
        let (_i, out_i) = PairingSession::sas_initiator(ctx(0x11, 0x22, EKM), t0, nonce(1));
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        let mut s = 1;
        deliver(&out_i, &mut s, &mut r, t0);
        let reveal = Frame::PairReveal(PairReveal {
            nonce: nonce(9).to_wire(),
        });
        let out = r.handle_frame(t0, &env(2, None, reveal));
        assert_eq!(rejected(&out), Some((RejectReason::CommitMismatch, false)));
        assert!(matches!(
            &out[0],
            PairingOutput::Send {
                frame: Frame::PairReject(PairReject {
                    reason: RejectReason::CommitMismatch
                }),
                ..
            }
        ));
        assert!(r.is_finished());
    }

    #[test]
    fn timeout_path() {
        let t0 = Instant::now();
        let (mut i, _) = PairingSession::sas_initiator(ctx(0x11, 0x22, EKM), t0, nonce(1));
        assert_eq!(i.deadline(), t0 + Duration::from_secs(30));
        assert_eq!(i.poll(t0 + Duration::from_secs(29)), NOTHING);
        let out = i.poll(t0 + Duration::from_secs(30));
        assert_eq!(rejected(&out), Some((RejectReason::Timeout, false)));
        assert!(i.is_finished());
        assert_eq!(i.poll(t0 + Duration::from_secs(31)), NOTHING);
        // A late frame after the deadline also times out instead of progressing.
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        let req = Frame::PairRequest(PairRequest {
            method: PairMethod::Sas,
            commit: Some(commit(&nonce(1))),
            proof: None,
        });
        let out = r.handle_frame(t0 + Duration::from_secs(31), &env(1, None, req));
        assert_eq!(rejected(&out), Some((RejectReason::Timeout, false)));
    }

    #[test]
    fn user_and_peer_reject() {
        let t0 = Instant::now();
        let (mut i, out_i) = PairingSession::sas_initiator(ctx(0x11, 0x22, EKM), t0, nonce(1));
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        let mut s = 1;
        let out_r = deliver(&out_i, &mut s, &mut r, t0);
        let out_i = deliver(&out_r, &mut s, &mut i, t0);
        deliver(&out_i, &mut s, &mut r, t0);
        let rej = r.reject(t0);
        assert_eq!(rejected(&rej), Some((RejectReason::User, false)));
        let got = deliver(&rej, &mut s, &mut i, t0);
        assert_eq!(rejected(&got), Some((RejectReason::User, true)));
    }

    #[test]
    fn unexpected_frame_aborts() {
        let t0 = Instant::now();
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        let out = r.handle_frame(
            t0,
            &env(1, None, Frame::PairConfirm(PairConfirm::default())),
        );
        assert!(matches!(events(&out)[0], PairingEvent::Aborted { .. }));
        // Non-pairing frames are ignored.
        let mut r = PairingSession::responder(ctx(0x22, 0x11, EKM), t0, nonce(2), None);
        assert_eq!(r.handle_frame(t0, &env(1, None, Frame::Pong)), NOTHING);
        assert!(!r.is_finished());
    }

    fn qr_pair(
        offer_secret: QrSecret,
        scanned_secret: QrSecret,
        expires_in: Duration,
    ) -> (PairingSession, PairingSession, Vec<PairingOutput>) {
        let t0 = Instant::now();
        // A = 0x11 displays, B = 0x22 scans.
        let mut payload = sample_qr();
        payload.secret = scanned_secret;
        let fp = payload.fingerprint.clone();
        let (b, out_b) =
            PairingSession::qr_initiator(ctx(0x22, 0x11, EKM), t0, &payload, &fp).unwrap();
        let mut a = PairingSession::responder(
            ctx(0x11, 0x22, EKM),
            t0,
            nonce(2),
            Some(QrOffer::new(offer_secret, t0 + expires_in)),
        );
        let mut s = 1;
        let out_a = deliver(&out_b, &mut s, &mut a, t0);
        (a, b, out_a)
    }

    #[test]
    fn full_qr_pairing() {
        let secret = QrSecret::from_bytes([5; 16]);
        let (mut a, mut b, out_a) = qr_pair(secret.clone(), secret, Duration::from_secs(120));
        assert!(matches!(
            events(&out_a)[0],
            PairingEvent::AskConfirmScanner { .. }
        ));
        assert!(!a.is_finished());
        let t = Instant::now();
        let conf = a.confirm(t);
        assert!(matches!(
            &conf[0],
            PairingOutput::Send {
                frame: Frame::PairConfirm(PairConfirm { proof: Some(_) }),
                re: Some(1)
            }
        ));
        assert!(is_paired(&conf));
        let mut s = 1;
        let out_b = deliver(&conf, &mut s, &mut b, t);
        assert!(is_paired(&out_b));
        assert!(a.is_finished() && b.is_finished());
    }

    #[test]
    fn qr_wrong_secret_and_expiry() {
        let (a, _b, out_a) = qr_pair(
            QrSecret::from_bytes([5; 16]),
            QrSecret::from_bytes([6; 16]),
            Duration::from_secs(120),
        );
        assert_eq!(
            rejected(&out_a),
            Some((RejectReason::QrProofInvalid, false))
        );
        assert!(a.is_finished());
        let secret = QrSecret::from_bytes([5; 16]);
        let (_, _, out_a) = qr_pair(secret.clone(), secret, Duration::ZERO);
        assert_eq!(rejected(&out_a), Some((RejectReason::QrExpired, false)));
    }

    #[test]
    fn qr_scanner_rejects_bad_proof_a_and_wrong_peer() {
        let t0 = Instant::now();
        let payload = sample_qr();
        let fp = payload.fingerprint.clone();
        assert_eq!(
            PairingSession::qr_initiator(ctx(0x22, 0x11, EKM), t0, &payload, &hex_encode(&[0; 32]))
                .unwrap_err(),
            PairingError::QrPeerMismatch
        );
        assert_eq!(
            PairingSession::qr_initiator(ctx(0x22, 0x33, EKM), t0, &payload, &fp).unwrap_err(),
            PairingError::QrPeerMismatch
        );
        let (mut b, _) =
            PairingSession::qr_initiator(ctx(0x22, 0x11, EKM), t0, &payload, &fp).unwrap();
        let bad = Frame::PairConfirm(PairConfirm {
            proof: Some(hex_encode(&[0; 32])),
        });
        let out = b.handle_frame(t0, &env(1, Some(1), bad));
        assert_eq!(rejected(&out), Some((RejectReason::QrProofInvalid, false)));
    }

    #[test]
    fn qr_request_without_offer_is_expired() {
        let t0 = Instant::now();
        let mut a = PairingSession::responder(ctx(0x11, 0x22, EKM), t0, nonce(2), None);
        let req = Frame::PairRequest(PairRequest {
            method: PairMethod::Qr,
            commit: None,
            proof: Some(hex_encode(&[0; 32])),
        });
        let out = a.handle_frame(t0, &env(1, None, req));
        assert_eq!(rejected(&out), Some((RejectReason::QrExpired, false)));
    }

    #[test]
    fn add_device_window_rules() {
        let t0 = Instant::now();
        let mut w = AddDeviceWindow::new();
        assert_eq!(w.try_begin(t0), Err(BeginRefusal::Closed));
        assert_eq!(w.open(t0, true), Err(PairingError::Locked));
        w.open(t0, false).unwrap();
        w.try_begin(t0).unwrap();
        assert_eq!(w.try_begin(t0), Err(BeginRefusal::Busy));
        w.finish(false);
        w.try_begin(t0).unwrap();
        w.finish(false);
        w.try_begin(t0).unwrap();
        w.finish(false);
        assert!(!w.is_open(t0), "3 failures close the window");
        w.open(t0, false).unwrap();
        w.try_begin(t0).unwrap();
        w.finish(true);
        assert!(!w.is_open(t0), "success closes the window");
        w.open(t0, false).unwrap();
        assert!(w.is_open(t0 + Duration::from_secs(119)));
        assert!(!w.is_open(t0 + Duration::from_secs(120)));
    }

    #[test]
    fn pair_request_rate_limit() {
        let t0 = Instant::now();
        let mut l = PairRequestLimiter::default();
        let ip: IpAddr = "192.168.1.9".parse().unwrap();
        let mapped: IpAddr = "::ffff:192.168.1.9".parse().unwrap();
        for _ in 0..5 {
            assert!(l.check(ip, t0));
        }
        assert!(!l.check(mapped, t0 + Duration::from_secs(59)));
        assert!(l.check("192.168.1.10".parse().unwrap(), t0));
        assert!(l.check(ip, t0 + Duration::from_secs(60)));
        // Idle addresses are pruned, so memory stays bounded.
        for n in 0..1000u32 {
            l.check(IpAddr::from(n.to_be_bytes()), t0 + Duration::from_secs(200));
        }
        l.check(ip, t0 + Duration::from_secs(300));
        assert_eq!(l.per_ip.len(), 1);
    }
}
