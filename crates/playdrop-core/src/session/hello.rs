//! `hello` validation and capability negotiation, protocol.md section 4 and 5.4 (SR-8,
//! SR-12).

use super::types::{CapRequirement, Capability, Caps, DeviceType, ErrorCode, Frame, Hello, Trust};
use super::util::is_device_id;

/// Maximum display name length in characters after sanitising (SR-8).
pub const NAME_MAX_CHARS: usize = 64;

/// Strips C0 and C1 controls (and DEL), bidi embeddings, overrides, isolates and marks, and
/// line and paragraph separators, trims surrounding whitespace and cuts to 64 characters
/// (protocol.md 5.4, SR-8). The result is display-only and never feeds a trust decision.
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name.chars().filter(|&c| !is_stripped(c)).collect();
    cleaned.trim().chars().take(NAME_MAX_CHARS).collect()
}

fn is_stripped(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{061C}'
                | '\u{200E}'
                | '\u{200F}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
        )
}

/// Why a `hello` was refused. Both close the connection after sending the error frame.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelloError {
    /// `deviceId` is not the TLS peer's Device ID, or not a Device ID at all.
    #[error("hello deviceId does not match the TLS peer")]
    IdentityMismatch,
    /// `protocolVersion` is not the ALPN-selected version.
    #[error("hello protocolVersion {got} differs from ALPN version {alpn}")]
    UnsupportedVersion {
        /// The version in `hello`.
        got: u32,
        /// The version ALPN chose.
        alpn: u32,
    },
}

impl HelloError {
    /// The section 9 code to send in the `error` frame before closing.
    pub fn code(&self) -> ErrorCode {
        match self {
            HelloError::IdentityMismatch => ErrorCode::IdentityMismatch,
            HelloError::UnsupportedVersion { .. } => ErrorCode::UnsupportedVersion,
        }
    }

    /// The `error` frame to send before closing.
    pub fn error_frame(&self) -> Frame {
        Frame::error(self.code(), None)
    }
}

/// How the two sides' views of the pairing compare (protocol.md 5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustCheck {
    /// Both sides have each other pinned.
    BothPaired,
    /// Neither side has a pin (an "Add device" connection).
    BothUnpaired,
    /// We have the peer pinned but it has no pin for us: it unpaired while we were offline.
    /// Mark the pin "removed by peer" and ask the user to pair again.
    RemovedByPeer,
    /// The peer has us pinned but we have no pin for it: we unpaired. The peer will see
    /// [`TrustCheck::RemovedByPeer`] on its side; nothing to do here.
    PeerHasStalePin,
    /// The peer sent a `trust` value this version does not know; take no action.
    Unrecognized,
}

impl TrustCheck {
    /// Compares the peer's `trust` with whether we have the peer pinned.
    pub fn compare(peer_trust: &Trust, we_pinned_peer: bool) -> Self {
        match (peer_trust, we_pinned_peer) {
            (Trust::Paired, true) => TrustCheck::BothPaired,
            (Trust::Unpaired, false) => TrustCheck::BothUnpaired,
            (Trust::Unpaired, true) => TrustCheck::RemovedByPeer,
            (Trust::Paired, false) => TrustCheck::PeerHasStalePin,
            (Trust::Unknown(_), _) => TrustCheck::Unrecognized,
        }
    }
}

/// A `hello` that passed [`validate_hello`], with display fields sanitised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedHello {
    /// Sanitised display name (SR-8).
    pub name: String,
    /// Device form factor.
    pub device_type: DeviceType,
    /// Peer's listening port.
    pub port: u16,
    /// Sanitised application string, for diagnostics.
    pub app: String,
    /// Peer's keep-alive interval in seconds (protocol.md 3.4).
    pub keep_alive: u32,
    /// Trust comparison for the host to act on.
    pub trust: TrustCheck,
    /// Peer's capability lists.
    pub caps: Caps,
}

/// Validates a peer's `hello` (protocol.md 5.4).
///
/// - `expected_device_id`: the Device ID of the TLS peer certificate.
/// - `alpn_version`: the version ALPN selected (protocol.md section 4, SR-12).
/// - `we_pinned_peer`: whether the local pin store has the peer.
pub fn validate_hello(
    hello: &Hello,
    expected_device_id: &str,
    alpn_version: u32,
    we_pinned_peer: bool,
) -> Result<ValidatedHello, HelloError> {
    if !is_device_id(&hello.device_id) || hello.device_id != expected_device_id {
        return Err(HelloError::IdentityMismatch);
    }
    if hello.protocol_version != alpn_version {
        return Err(HelloError::UnsupportedVersion {
            got: hello.protocol_version,
            alpn: alpn_version,
        });
    }
    Ok(ValidatedHello {
        name: sanitize_name(&hello.name),
        device_type: hello.device_type.clone(),
        port: hello.port,
        app: sanitize_name(&hello.app),
        keep_alive: hello.keep_alive,
        trust: TrustCheck::compare(&hello.trust, we_pinned_peer),
        caps: hello.caps.clone(),
    })
}

/// True if we may send frames needing `cap`: it is in our `out` list and the peer's `in`
/// list (protocol.md section 4: a host MUST NOT send anything the peer's `in` lacks).
pub fn may_send(local_out: &[Capability], peer_in: &[Capability], cap: &Capability) -> bool {
    cap.is_known() && local_out.contains(cap) && peer_in.contains(cap)
}

/// True if `frame` may be sent given both capability lists, using
/// [`Frame::required_capabilities`]. Frames with no capability (hello, pairing, error and so
/// on) are always allowed here; grants and paired state are checked separately.
pub fn may_send_frame(frame: &Frame, local_out: &[Capability], peer_in: &[Capability]) -> bool {
    match frame.required_capabilities() {
        CapRequirement::None => true,
        CapRequirement::AllOf(caps) => caps.iter().all(|c| may_send(local_out, peer_in, c)),
        CapRequirement::AnyOf(caps) => caps.iter().any(|c| may_send(local_out, peer_in, c)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::types::{Clipboard, ClipboardKind, Input, SessionOp};

    const ID: &str = "3f9ac21e7b04a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a";

    fn hello() -> Hello {
        Hello {
            protocol_version: 1,
            device_id: ID.into(),
            name: "Pixel 9".into(),
            device_type: DeviceType::Phone,
            port: 42187,
            app: "playdrop-android/0.1.0".into(),
            keep_alive: 300,
            trust: Trust::Paired,
            caps: Caps::default(),
        }
    }

    #[test]
    fn sanitises_names() {
        assert_eq!(sanitize_name("Qasim\u{202E}gpj.exe"), "Qasimgpj.exe");
        assert_eq!(sanitize_name("a\u{0}b\u{1b}[31mc\u{85}d\u{7f}"), "ab[31mcd");
        assert_eq!(sanitize_name("\u{2067}x\u{2069}\u{200F}"), "x");
        assert_eq!(sanitize_name("  Pixel\n9 "), "Pixel9");
        let long = "é".repeat(100);
        assert_eq!(sanitize_name(&long).chars().count(), NAME_MAX_CHARS);
        assert_eq!(sanitize_name("Pixel 9"), "Pixel 9");
    }

    #[test]
    fn identity_and_version_checks() {
        let ok = validate_hello(&hello(), ID, 1, true).unwrap();
        assert_eq!(ok.trust, TrustCheck::BothPaired);
        let other = ID.replace('3', "4");
        assert_eq!(
            validate_hello(&hello(), &other, 1, true),
            Err(HelloError::IdentityMismatch)
        );
        let mut upper = hello();
        upper.device_id = ID.to_uppercase();
        assert_eq!(
            validate_hello(&upper, &ID.to_uppercase(), 1, true),
            Err(HelloError::IdentityMismatch)
        );
        let e = validate_hello(&hello(), ID, 2, true).unwrap_err();
        assert_eq!(e, HelloError::UnsupportedVersion { got: 1, alpn: 2 });
        assert_eq!(e.code(), ErrorCode::UnsupportedVersion);
        assert_eq!(
            HelloError::IdentityMismatch.code().as_str(),
            "identity_mismatch"
        );
    }

    #[test]
    fn trust_mismatch() {
        assert_eq!(
            TrustCheck::compare(&Trust::Unpaired, true),
            TrustCheck::RemovedByPeer
        );
        assert_eq!(
            TrustCheck::compare(&Trust::Paired, false),
            TrustCheck::PeerHasStalePin
        );
        assert_eq!(
            TrustCheck::compare(&Trust::Unpaired, false),
            TrustCheck::BothUnpaired
        );
        assert_eq!(
            TrustCheck::compare(&Trust::Unknown("x".into()), true),
            TrustCheck::Unrecognized
        );
    }

    #[test]
    fn capability_negotiation() {
        use Capability as C;
        let out = [C::Ping, C::Clipboard, C::InputKey];
        let peer_in = [C::Ping, C::Clipboard, C::InputKey, C::Find];
        assert!(may_send(&out, &peer_in, &C::Ping));
        assert!(!may_send(&out, &peer_in, &C::Find));
        assert!(!may_send(
            &[C::Unknown("x".into())],
            &[C::Unknown("x".into())],
            &C::Unknown("x".into())
        ));
        let text = Frame::Clipboard(Clipboard {
            kind: ClipboardKind::Text,
            mime: "text/plain".into(),
            hash: "h".into(),
            text: Some("x".into()),
            blob: None,
            sensitive: false,
        });
        assert!(may_send_frame(&text, &out, &peer_in));
        let Frame::Clipboard(mut img) = text else {
            unreachable!()
        };
        img.kind = ClipboardKind::Image;
        assert!(!may_send_frame(&Frame::Clipboard(img), &out, &peer_in));
        let session = Frame::Input(Input::Session {
            op: SessionOp::Start,
        });
        assert!(may_send_frame(&session, &out, &peer_in));
        assert!(!may_send_frame(&session, &[C::Ping], &peer_in));
        assert!(may_send_frame(&Frame::Unpair, &[], &[]));
    }
}
