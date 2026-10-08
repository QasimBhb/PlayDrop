//! TLS 1.3 profile: Ed25519 only, mutual auth, pin-only verifier, ALPN, no resumption.
//! Implements `docs/protocol.md` 3.1 and section 4, SR-11, SR-12.
//!
//! Both ends present their self-signed identity certificate. There is no CA store: the
//! verifiers hash the presented SPKI and accept only a pinned hash, except that the server
//! side also accepts any valid self-signed Ed25519 certificate while the local "Add
//! device" window is open (unpaired mode, protocol.md 6.1). Validity dates are ignored
//! (protocol.md section 2).

use std::collections::{BTreeSet, HashSet};
use std::fmt;
use std::hash::BuildHasher;
use std::net::IpAddr;
use std::ops::Deref;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::Resumption;
use rustls::crypto::{ring, CryptoProvider};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::NoServerSessionStorage;
use rustls::sign::SingleCertAndKey;
use rustls::{
    CertificateError, ClientConfig, ConnectionCommon, DigitallySignedStruct, DistinguishedName,
    ServerConfig, SignatureScheme, SupportedCipherSuite,
};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::constants::ALPN_V1;
use crate::identity::{inspect_certificate, CertInfo, DeviceId, DeviceIdentity};
use crate::{Error, Result};

/// Exporter label for the pairing binding value `ekm` (protocol.md 6.2 and 6.3).
pub const EXPORTER_LABEL: &[u8] = b"EXPORTER-playdrop-pair";

/// Length of the exporter value in bytes (protocol.md 6.2).
pub const EXPORTER_LEN: usize = 32;

/// Handshake timeout (SR-15, protocol.md section 10).
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Answers whether an SPKI hash belongs to a paired device (protocol.md 3.1, SR-11).
///
/// Called synchronously from inside the TLS handshake, so implementations must not block
/// on async locks.
pub trait PinLookup: Send + Sync {
    /// The Device ID pinned for `spki_hash`, or `None` if it is not paired.
    fn spki_is_pinned(&self, spki_hash: &[u8; 32]) -> Option<DeviceId>;
}

impl<T: PinLookup> PinLookup for RwLock<T> {
    /// A poisoned lock fails closed: nothing is pinned.
    fn spki_is_pinned(&self, spki_hash: &[u8; 32]) -> Option<DeviceId> {
        self.read().ok()?.spki_is_pinned(spki_hash)
    }
}

impl<S: BuildHasher + Send + Sync> PinLookup for HashSet<DeviceId, S> {
    fn spki_is_pinned(&self, spki_hash: &[u8; 32]) -> Option<DeviceId> {
        let id = DeviceId::from_spki_hash(*spki_hash);
        self.contains(&id).then_some(id)
    }
}

impl PinLookup for BTreeSet<DeviceId> {
    fn spki_is_pinned(&self, spki_hash: &[u8; 32]) -> Option<DeviceId> {
        let id = DeviceId::from_spki_hash(*spki_hash);
        self.contains(&id).then_some(id)
    }
}

/// How the peer of a finished handshake is trusted (protocol.md 3.1 and 6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerTrust {
    /// The peer's SPKI hash is pinned.
    Pinned,
    /// Not pinned; admitted only because the "Add device" window was open (server side),
    /// or dialled from the "Add device" list (client side). Only pairing is allowed
    /// (protocol.md 6.1).
    Unpaired,
}

/// The authenticated peer of a TLS connection (protocol.md section 2 and 3.1).
#[derive(Clone, PartialEq, Eq)]
pub struct PeerIdentity {
    /// Device ID from the peer certificate's SPKI.
    pub device_id: DeviceId,
    /// Raw SHA-256 of the peer's DER SPKI.
    pub spki_hash: [u8; 32],
    /// The peer's DER SPKI, a SAS input (protocol.md 6.2).
    pub spki_der: Vec<u8>,
    /// The peer's DER certificate, stored in the pin on pairing.
    pub cert_der: Vec<u8>,
    /// Whether the peer is pinned or in unpaired mode.
    pub trust: PeerTrust,
}

impl fmt::Debug for PeerIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PeerIdentity")
            .field("device_id", &self.device_id)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// Whether this CPU has AES instructions, which makes AES-GCM much faster than
/// ChaCha20-Poly1305 (protocol.md section 10).
fn has_aes_hardware() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        std::arch::is_x86_feature_detected!("aes")
            && std::arch::is_x86_feature_detected!("pclmulqdq")
    }
    #[cfg(target_arch = "aarch64")]
    {
        std::arch::is_aarch64_feature_detected!("aes")
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
    {
        false
    }
}

/// The ring provider cut down to the profile of protocol.md 3.1: TLS 1.3 suites
/// AES-128-GCM and ChaCha20-Poly1305 only, key exchange X25519 then P-256.
///
/// `chacha_first` lists ChaCha20-Poly1305 first; clients without AES hardware do this and
/// servers honour the client's order (protocol.md section 10).
fn provider(chacha_first: bool) -> Arc<CryptoProvider> {
    let aes: SupportedCipherSuite = ring::cipher_suite::TLS13_AES_128_GCM_SHA256;
    let chacha: SupportedCipherSuite = ring::cipher_suite::TLS13_CHACHA20_POLY1305_SHA256;
    let cipher_suites = if chacha_first {
        vec![chacha, aes]
    } else {
        vec![aes, chacha]
    };
    Arc::new(CryptoProvider {
        cipher_suites,
        kx_groups: vec![ring::kx_group::X25519, ring::kx_group::SECP256R1],
        ..ring::default_provider()
    })
}

/// Maps a certificate check failure to the rustls error that aborts the handshake.
fn reject(err: &Error) -> rustls::Error {
    tracing::debug!(error = %err, "TLS peer certificate rejected");
    rustls::Error::InvalidCertificate(CertificateError::ApplicationVerificationFailure)
}

/// Checks the presented chain is exactly one valid self-signed Ed25519 certificate.
fn inspect_chain(
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
) -> std::result::Result<CertInfo, rustls::Error> {
    if !intermediates.is_empty() {
        return Err(rustls::Error::InvalidCertificate(
            CertificateError::ApplicationVerificationFailure,
        ));
    }
    inspect_certificate(end_entity).map_err(|e| reject(&e))
}

/// `CertificateVerify` check shared by both verifiers: Ed25519 only (protocol.md 3.1).
fn verify_ed25519_tls13(
    provider: &CryptoProvider,
    message: &[u8],
    cert: &CertificateDer<'_>,
    dss: &DigitallySignedStruct,
) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(rustls::Error::PeerMisbehaved(
            rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
        ));
    }
    rustls::crypto::verify_tls13_signature(
        message,
        cert,
        dss,
        &provider.signature_verification_algorithms,
    )
}

/// TLS 1.2 is never negotiated (protocol.md 3.1); refuse its signatures outright.
fn refuse_tls12() -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
    Err(rustls::Error::PeerIncompatible(
        rustls::PeerIncompatible::Tls12NotOffered,
    ))
}

/// Client-side verifier of the server certificate (protocol.md 3.1, SR-11).
///
/// With an expected SPKI hash it accepts only that pin. Without one (dialling a device
/// picked from the "Add device" list, protocol.md 6.1) it accepts any valid self-signed
/// Ed25519 certificate; the connection is then unpaired and pairing (6.2, 6.3)
/// authenticates the peer.
#[derive(Debug)]
struct PinnedServerVerifier {
    expected: Option<[u8; 32]>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for PinnedServerVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let info = inspect_chain(end_entity, intermediates)?;
        match self.expected {
            Some(expected) if expected != info.spki_hash => Err(reject(&Error::NotPaired)),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        refuse_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_ed25519_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Server-side verifier of the client certificate (protocol.md 3.1 and 6.1, SR-11).
///
/// A client certificate is mandatory. A pinned SPKI hash is accepted; an unpinned valid
/// self-signed Ed25519 certificate is accepted only while the "Add device" window is open.
struct PinnedClientVerifier {
    pins: Arc<dyn PinLookup>,
    add_device_window: Arc<AtomicBool>,
    provider: Arc<CryptoProvider>,
}

impl fmt::Debug for PinnedClientVerifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PinnedClientVerifier")
            .field("add_device_window", &self.add_device_window)
            .finish_non_exhaustive()
    }
}

impl ClientCertVerifier for PinnedClientVerifier {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> std::result::Result<ClientCertVerified, rustls::Error> {
        let info = inspect_chain(end_entity, intermediates)?;
        if self.pins.spki_is_pinned(&info.spki_hash).is_some()
            || self.add_device_window.load(Ordering::SeqCst)
        {
            Ok(ClientCertVerified::assertion())
        } else {
            Err(reject(&Error::NotPaired))
        }
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        refuse_tls12()
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_ed25519_tls13(&self.provider, message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        vec![SignatureScheme::ED25519]
    }
}

/// Builds the server configuration of protocol.md 3.1 and section 4 (SR-11, SR-12): TLS
/// 1.3 only, Ed25519 only, client certificate required and checked against `pins` (or
/// admitted in unpaired mode while `add_device_window` is true), ALPN `playdrop/1`, no
/// session tickets, no session cache, no 0-RTT, and the client's cipher order honoured.
///
/// # Panics
///
/// Only if the fixed provider of this module rejects TLS 1.3, which the tests rule out.
pub fn build_server_config(
    identity: &DeviceIdentity,
    pins: Arc<dyn PinLookup>,
    add_device_window: Arc<AtomicBool>,
) -> Arc<ServerConfig> {
    let provider = provider(false);
    let verifier = Arc::new(PinnedClientVerifier {
        pins,
        add_device_window,
        provider: Arc::clone(&provider),
    });
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("ring provider supports TLS 1.3")
        .with_client_cert_verifier(verifier)
        .with_cert_resolver(Arc::new(SingleCertAndKey::from(identity.certified_key())));
    config.alpn_protocols = vec![ALPN_V1.to_vec()];
    config.ignore_client_order = false;
    config.send_tls13_tickets = 0;
    config.session_storage = Arc::new(NoServerSessionStorage {});
    config.max_early_data_size = 0;
    config.send_half_rtt_data = false;
    Arc::new(config)
}

fn client_config(identity: &DeviceIdentity, expected: Option<[u8; 32]>) -> Arc<ClientConfig> {
    let provider = provider(!has_aes_hardware());
    let verifier = Arc::new(PinnedServerVerifier {
        expected,
        provider: Arc::clone(&provider),
    });
    let mut config = ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("ring provider supports TLS 1.3")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_cert_resolver(Arc::new(SingleCertAndKey::from(identity.certified_key())));
    config.alpn_protocols = vec![ALPN_V1.to_vec()];
    config.resumption = Resumption::disabled();
    config.enable_early_data = false;
    config.enable_sni = false;
    Arc::new(config)
}

/// Builds the client configuration for dialling a pinned peer (protocol.md 3.1, 3.2,
/// SR-11): TLS 1.3 only, Ed25519 only, this identity as client certificate, only a server
/// whose SPKI hash equals `expected_spki` is accepted, ALPN `playdrop/1`, no resumption,
/// no 0-RTT and no SNI. ChaCha20-Poly1305 is listed first on CPUs without AES instructions
/// (protocol.md section 10).
///
/// # Panics
///
/// Only if the fixed provider of this module rejects TLS 1.3, which the tests rule out.
pub fn build_client_config(
    identity: &DeviceIdentity,
    expected_spki: [u8; 32],
) -> Arc<ClientConfig> {
    client_config(identity, Some(expected_spki))
}

/// Builds the client configuration for dialling a device picked from the "Add device"
/// list, whose key is not yet known (protocol.md 6.1). Same profile as
/// [`build_client_config`], but any valid self-signed Ed25519 server certificate is
/// accepted and the connection is unpaired until pairing (6.2, 6.3) completes. Never use
/// it to dial a pinned peer.
///
/// # Panics
///
/// Only if the fixed provider of this module rejects TLS 1.3, which the tests rule out.
pub fn build_client_config_unpaired(identity: &DeviceIdentity) -> Arc<ClientConfig> {
    client_config(identity, None)
}

/// The RFC 8446 section 7.5 exporter value `ekm` for pairing (protocol.md 6.2 and 6.3):
/// label `EXPORTER-playdrop-pair`, empty context, 32 bytes.
///
/// Takes a `ServerConnection` or `ClientConnection`, for example
/// `exporter(stream.get_ref().1)` on a tokio-rustls stream.
pub fn exporter<C, D>(conn: &C) -> Result<[u8; EXPORTER_LEN]>
where
    C: Deref<Target = ConnectionCommon<D>>,
{
    conn.export_keying_material([0u8; EXPORTER_LEN], EXPORTER_LABEL, Some(&[]))
        .map_err(|e| Error::Tls(format!("exporter: {e}")))
}

/// Post-handshake checks common to both sides: TLS 1.3 and ALPN exactly `playdrop/1`
/// (protocol.md 3.1 and section 4, SR-12). A peer that offered no ALPN completes the TLS
/// handshake in rustls, so this check is what closes it.
fn check_negotiated<D>(conn: &ConnectionCommon<D>) -> Result<CertInfoWithDer> {
    if conn.protocol_version() != Some(rustls::ProtocolVersion::TLSv1_3) {
        return Err(Error::Tls("negotiated version is not TLS 1.3".into()));
    }
    if conn.alpn_protocol() != Some(ALPN_V1) {
        return Err(Error::Tls("peer did not negotiate ALPN playdrop/1".into()));
    }
    let certs = conn
        .peer_certificates()
        .ok_or_else(|| Error::Tls("peer sent no certificate".into()))?;
    let [cert] = certs else {
        return Err(Error::Tls("peer sent more than one certificate".into()));
    };
    let info = inspect_certificate(cert)?;
    Ok(CertInfoWithDer {
        info,
        cert_der: cert.to_vec(),
    })
}

struct CertInfoWithDer {
    info: CertInfo,
    cert_der: Vec<u8>,
}

impl CertInfoWithDer {
    fn into_peer(self, trust: PeerTrust) -> PeerIdentity {
        PeerIdentity {
            device_id: self.info.device_id,
            spki_hash: self.info.spki_hash,
            spki_der: self.info.spki_der,
            cert_der: self.cert_der,
            trust,
        }
    }
}

/// Runs the server handshake on `io` with a [`HANDSHAKE_TIMEOUT`] (SR-15) and returns the
/// stream and the authenticated peer (protocol.md 3.1, 6.1).
///
/// `pins` and `add_device_window` must be the ones `config` was built with. The peer is
/// classified again after the handshake: pinned, else unpaired if the window is still
/// open, else the connection is closed with [`Error::NotPaired`] (protocol.md 6.1, SR-15).
pub async fn accept<IO>(
    config: Arc<ServerConfig>,
    pins: &dyn PinLookup,
    add_device_window: &AtomicBool,
    io: IO,
) -> Result<(tokio_rustls::server::TlsStream<IO>, PeerIdentity)>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(io))
        .await
        .map_err(|_| Error::Tls("handshake timed out".into()))?
        .map_err(|e| Error::Tls(format!("handshake failed: {e}")))?;
    let peer = check_negotiated(stream.get_ref().1)?;
    let trust = classify_client(&peer.info, pins, add_device_window)?;
    Ok((stream, peer.into_peer(trust)))
}

/// Server-side trust decision after the handshake (protocol.md 6.1): pinned, else
/// unpaired while the window is open, else [`Error::NotPaired`].
fn classify_client(
    info: &CertInfo,
    pins: &dyn PinLookup,
    add_device_window: &AtomicBool,
) -> Result<PeerTrust> {
    match pins.spki_is_pinned(&info.spki_hash) {
        Some(id) if id == info.device_id => Ok(PeerTrust::Pinned),
        Some(_) => Err(Error::Tls("pin lookup returned a different device".into())),
        None if add_device_window.load(Ordering::SeqCst) => Ok(PeerTrust::Unpaired),
        None => Err(Error::NotPaired),
    }
}

/// Runs the client handshake on `io` towards `peer_ip` with a [`HANDSHAKE_TIMEOUT`]
/// (SR-15). The IP literal is the server name, so no SNI is sent (protocol.md 3.1).
///
/// `expected_spki` must match how `config` was built: `Some` for
/// [`build_client_config`], `None` for [`build_client_config_unpaired`]. The peer is
/// checked against it again after the handshake.
pub async fn connect<IO>(
    config: Arc<ClientConfig>,
    expected_spki: Option<[u8; 32]>,
    peer_ip: IpAddr,
    io: IO,
) -> Result<(tokio_rustls::client::TlsStream<IO>, PeerIdentity)>
where
    IO: AsyncRead + AsyncWrite + Unpin,
{
    let connector = tokio_rustls::TlsConnector::from(config);
    let name = ServerName::IpAddress(peer_ip.into());
    let stream = tokio::time::timeout(HANDSHAKE_TIMEOUT, connector.connect(name, io))
        .await
        .map_err(|_| Error::Tls("handshake timed out".into()))?
        .map_err(|e| Error::Tls(format!("handshake failed: {e}")))?;
    let peer = check_negotiated(stream.get_ref().1)?;
    let trust = match expected_spki {
        Some(expected) if expected == peer.info.spki_hash => PeerTrust::Pinned,
        Some(_) => return Err(Error::NotPaired),
        None => PeerTrust::Unpaired,
    };
    Ok((stream, peer.into_peer(trust)))
}

/// [`connect`] over an already connected TCP stream, using its peer address as the
/// server name (protocol.md 3.1 and 3.2).
pub async fn connect_tcp(
    config: Arc<ClientConfig>,
    expected_spki: Option<[u8; 32]>,
    tcp: TcpStream,
) -> Result<(tokio_rustls::client::TlsStream<TcpStream>, PeerIdentity)> {
    let ip = tcp.peer_addr()?.ip();
    connect(config, expected_spki, ip, tcp).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const LOCAL: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    struct Server {
        identity: DeviceIdentity,
        pins: Arc<RwLock<HashSet<DeviceId>>>,
        window: Arc<AtomicBool>,
        config: Arc<ServerConfig>,
    }

    impl Server {
        fn new() -> Self {
            let identity = DeviceIdentity::generate().unwrap();
            let pins = Arc::new(RwLock::new(HashSet::new()));
            let window = Arc::new(AtomicBool::new(false));
            let config = build_server_config(
                &identity,
                Arc::clone(&pins) as Arc<dyn PinLookup>,
                Arc::clone(&window),
            );
            Self {
                identity,
                pins,
                window,
                config,
            }
        }

        fn pin(&self, id: &DeviceIdentity) {
            self.pins.write().unwrap().insert(*id.device_id());
        }
    }

    type ServerOut = Result<(
        tokio_rustls::server::TlsStream<tokio::io::DuplexStream>,
        PeerIdentity,
    )>;
    type ClientOut = Result<(
        tokio_rustls::client::TlsStream<tokio::io::DuplexStream>,
        PeerIdentity,
    )>;

    async fn run(
        server: &Server,
        client_config: Arc<ClientConfig>,
        expected: Option<[u8; 32]>,
    ) -> (ServerOut, ClientOut) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        let pins = Arc::clone(&server.pins);
        tokio::join!(
            accept(Arc::clone(&server.config), &*pins, &server.window, a),
            connect(client_config, expected, LOCAL, b),
        )
    }

    #[tokio::test]
    async fn mutual_pinning_handshake() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let cfg = build_client_config(&client, server.identity.spki_hash());
        let (s, c) = run(&server, cfg, Some(server.identity.spki_hash())).await;
        let (mut s_stream, s_peer) = s.unwrap();
        let (mut c_stream, c_peer) = c.unwrap();

        assert_eq!(s_peer.device_id, *client.device_id());
        assert_eq!(s_peer.trust, PeerTrust::Pinned);
        assert_eq!(s_peer.spki_der, client.spki_der());
        assert_eq!(s_peer.cert_der, client.certificate_der());
        assert_eq!(c_peer.device_id, *server.identity.device_id());
        assert_eq!(c_peer.trust, PeerTrust::Pinned);

        let s_conn = s_stream.get_ref().1;
        let c_conn = c_stream.get_ref().1;
        assert_eq!(s_conn.alpn_protocol(), Some(ALPN_V1));
        assert_eq!(c_conn.alpn_protocol(), Some(ALPN_V1));
        assert_eq!(
            s_conn.protocol_version(),
            Some(rustls::ProtocolVersion::TLSv1_3)
        );
        assert!(s_conn.server_name().is_none(), "no SNI is sent");
        let suite = s_conn.negotiated_cipher_suite().unwrap().suite();
        assert!(matches!(
            suite,
            rustls::CipherSuite::TLS13_AES_128_GCM_SHA256
                | rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256
        ));
        assert_eq!(
            s_conn.negotiated_key_exchange_group().unwrap().name(),
            rustls::NamedGroup::X25519
        );

        let ekm_s = exporter(s_conn).unwrap();
        let ekm_c = exporter(c_conn).unwrap();
        assert_eq!(ekm_s, ekm_c);
        assert_ne!(ekm_s, [0u8; 32]);

        c_stream.write_all(b"hello").await.unwrap();
        c_stream.flush().await.unwrap();
        let mut buf = [0u8; 5];
        s_stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
    }

    #[tokio::test]
    async fn exporter_differs_between_sessions() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let spki = server.identity.spki_hash();
        let (s1, _c1) = run(&server, build_client_config(&client, spki), Some(spki)).await;
        let (s2, _c2) = run(&server, build_client_config(&client, spki), Some(spki)).await;
        let e1 = exporter(s1.unwrap().0.get_ref().1).unwrap();
        let e2 = exporter(s2.unwrap().0.get_ref().1).unwrap();
        assert_ne!(e1, e2);
    }

    #[tokio::test]
    async fn unpinned_client_rejected_outside_window_accepted_inside() {
        let server = Server::new();
        let stranger = DeviceIdentity::generate().unwrap();
        let spki = server.identity.spki_hash();

        let (s, _c) = run(&server, build_client_config(&stranger, spki), Some(spki)).await;
        assert!(
            s.is_err(),
            "unpinned client must be refused outside the window"
        );

        server.window.store(true, Ordering::SeqCst);
        let (s, c) = run(&server, build_client_config(&stranger, spki), Some(spki)).await;
        let (_, peer) = s.unwrap();
        assert_eq!(peer.trust, PeerTrust::Unpaired);
        assert_eq!(peer.device_id, *stranger.device_id());
        assert_eq!(c.unwrap().1.trust, PeerTrust::Pinned);

        // A pinned client stays pinned while the window is open.
        let friend = DeviceIdentity::generate().unwrap();
        server.pin(&friend);
        let (s, _c) = run(&server, build_client_config(&friend, spki), Some(spki)).await;
        assert_eq!(s.unwrap().1.trust, PeerTrust::Pinned);
    }

    #[tokio::test]
    async fn window_closing_after_verify_still_rejects() {
        // The verifier admitted the peer, but the window is closed by the time accept
        // classifies it: the connection is refused (protocol.md 6.1).
        let server = Server::new();
        let stranger = DeviceIdentity::generate().unwrap();
        let spki = server.identity.spki_hash();
        server.window.store(true, Ordering::SeqCst);
        let (a, b) = tokio::io::duplex(64 * 1024);
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::clone(&server.config));
        let connector = tokio_rustls::TlsConnector::from(build_client_config(&stranger, spki));
        let (s, c) = tokio::join!(
            acceptor.accept(a),
            connector.connect(ServerName::IpAddress(LOCAL.into()), b)
        );
        let _c = c.unwrap();
        let s = s.unwrap();
        server.window.store(false, Ordering::SeqCst);
        let peer = check_negotiated(s.get_ref().1).unwrap();
        let verdict = classify_client(&peer.info, &*server.pins, &server.window);
        assert!(matches!(verdict, Err(Error::NotPaired)));
    }

    #[tokio::test]
    async fn wrong_server_key_rejected_by_client() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let other = DeviceIdentity::generate().unwrap();
        let cfg = build_client_config(&client, other.spki_hash());
        let (s, c) = run(&server, cfg, Some(other.spki_hash())).await;
        assert!(c.is_err());
        assert!(s.is_err());
    }

    #[tokio::test]
    async fn unpaired_client_config_accepts_any_server() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.window.store(true, Ordering::SeqCst);
        let (s, c) = run(&server, build_client_config_unpaired(&client), None).await;
        assert_eq!(s.unwrap().1.trust, PeerTrust::Unpaired);
        let (_, peer) = c.unwrap();
        assert_eq!(peer.trust, PeerTrust::Unpaired);
        assert_eq!(peer.device_id, *server.identity.device_id());
    }

    #[tokio::test]
    async fn over_localhost_tcp() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let listener = tokio::net::TcpListener::bind((LOCAL, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let spki = server.identity.spki_hash();
        let cfg = build_client_config(&client, spki);
        let pins = Arc::clone(&server.pins);
        let (s, c) = tokio::join!(
            async {
                let (tcp, _) = listener.accept().await.unwrap();
                accept(Arc::clone(&server.config), &*pins, &server.window, tcp).await
            },
            async {
                let tcp = TcpStream::connect(addr).await.unwrap();
                connect_tcp(cfg, Some(spki), tcp).await
            }
        );
        assert_eq!(s.unwrap().1.device_id, *client.device_id());
        assert_eq!(c.unwrap().1.device_id, *server.identity.device_id());
    }

    #[tokio::test]
    async fn missing_alpn_refused() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let spki = server.identity.spki_hash();
        let mut cfg = (*build_client_config(&client, spki)).clone();
        cfg.alpn_protocols.clear();
        let (s, c) = run(&server, Arc::new(cfg), Some(spki)).await;
        assert!(s.is_err());
        assert!(c.is_err());
    }

    #[tokio::test]
    async fn wrong_alpn_refused() {
        let server = Server::new();
        let client = DeviceIdentity::generate().unwrap();
        server.pin(&client);
        let spki = server.identity.spki_hash();
        let mut cfg = (*build_client_config(&client, spki)).clone();
        cfg.alpn_protocols = vec![b"playdrop/0".to_vec()];
        let (s, c) = run(&server, Arc::new(cfg), Some(spki)).await;
        assert!(s.is_err());
        assert!(c.is_err());
    }

    #[tokio::test]
    async fn missing_client_certificate_refused() {
        let server = Server::new();
        server.window.store(true, Ordering::SeqCst);
        let p = provider(false);
        let mut cfg = ClientConfig::builder_with_provider(Arc::clone(&p))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier {
                expected: None,
                provider: p,
            }))
            .with_no_client_auth();
        cfg.alpn_protocols = vec![ALPN_V1.to_vec()];
        let (s, _c) = run(&server, Arc::new(cfg), None).await;
        assert!(s.is_err());
    }

    #[tokio::test]
    async fn non_ed25519_client_certificate_refused() {
        let server = Server::new();
        server.window.store(true, Ordering::SeqCst);
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = rcgen::CertificateParams::new(Vec::<String>::new())
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let p = provider(false);
        let certified = rustls::sign::CertifiedKey::from_der(
            vec![cert.der().clone()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
            &p,
        )
        .unwrap();
        let mut cfg = ClientConfig::builder_with_provider(Arc::clone(&p))
            .with_protocol_versions(&[&rustls::version::TLS13])
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinnedServerVerifier {
                expected: None,
                provider: p,
            }))
            .with_client_cert_resolver(Arc::new(SingleCertAndKey::from(certified)));
        cfg.alpn_protocols = vec![ALPN_V1.to_vec()];
        let (s, _c) = run(&server, Arc::new(cfg), None).await;
        assert!(s.is_err());
    }

    /// A minimal TLS 1.2 `ClientHello`: no `supported_versions` extension, one ECDHE-ECDSA
    /// AES-128-GCM suite, X25519, Ed25519 signatures.
    fn tls12_client_hello() -> Vec<u8> {
        let mut body = vec![0x03, 0x03];
        body.extend_from_slice(&[0x11; 32]); // random
        body.push(0x00); // session id
        body.extend_from_slice(&[0x00, 0x02, 0xc0, 0x2b]); // cipher suites
        body.extend_from_slice(&[0x01, 0x00]); // compression: null
        let ext: &[u8] = &[
            0x00, 0x0a, 0x00, 0x04, 0x00, 0x02, 0x00, 0x1d, // supported_groups: x25519
            0x00, 0x0d, 0x00, 0x04, 0x00, 0x02, 0x08, 0x07, // signature_algorithms: ed25519
            0x00, 0x0b, 0x00, 0x02, 0x01, 0x00, // ec_point_formats: uncompressed
            0x00, 0x10, 0x00, 0x0d, 0x00, 0x0b, 0x0a, b'p', b'l', b'a', b'y', b'd', b'r', b'o',
            b'p', b'/', b'1', // ALPN playdrop/1
        ];
        body.extend_from_slice(&u16::try_from(ext.len()).unwrap().to_be_bytes());
        body.extend_from_slice(ext);
        let len = u32::try_from(body.len()).unwrap().to_be_bytes();
        let mut hs = vec![0x01, len[1], len[2], len[3]];
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 0x03, 0x01];
        rec.extend_from_slice(&u16::try_from(hs.len()).unwrap().to_be_bytes());
        rec.extend_from_slice(&hs);
        rec
    }

    #[tokio::test]
    async fn tls12_client_refused() {
        let server = Server::new();
        server.window.store(true, Ordering::SeqCst);
        let (a, mut b) = tokio::io::duplex(64 * 1024);
        let pins = Arc::clone(&server.pins);
        let (s, reply) = tokio::join!(
            accept(Arc::clone(&server.config), &*pins, &server.window, a),
            async {
                b.write_all(&tls12_client_hello()).await.unwrap();
                let mut buf = Vec::new();
                let _ = b.read_to_end(&mut buf).await;
                buf
            }
        );
        assert!(s.is_err());
        // The only reply is a fatal alert record (content type 21), protocol_version (70).
        assert_eq!(reply.first(), Some(&0x15));
        assert_eq!(reply.get(5), Some(&0x02));
        assert_eq!(reply.get(6), Some(&70));
    }

    #[test]
    fn configs_disable_resumption_and_early_data() {
        let id = DeviceIdentity::generate().unwrap();
        let server = build_server_config(
            &id,
            Arc::new(HashSet::<DeviceId>::new()),
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(server.send_tls13_tickets, 0);
        assert_eq!(server.max_early_data_size, 0);
        assert!(!server.ticketer.enabled());
        assert_eq!(server.alpn_protocols, vec![ALPN_V1.to_vec()]);
        assert!(!server.ignore_client_order);

        let client = build_client_config(&id, [0u8; 32]);
        assert!(!client.enable_early_data);
        assert!(!client.enable_sni);
        assert_eq!(client.alpn_protocols, vec![ALPN_V1.to_vec()]);
        let suites: Vec<_> = client
            .crypto_provider()
            .cipher_suites
            .iter()
            .map(SupportedCipherSuite::suite)
            .collect();
        assert_eq!(suites.len(), 2);
        let groups: Vec<_> = client
            .crypto_provider()
            .kx_groups
            .iter()
            .map(|g| g.name())
            .collect();
        assert_eq!(
            groups,
            vec![rustls::NamedGroup::X25519, rustls::NamedGroup::secp256r1]
        );
    }
}
