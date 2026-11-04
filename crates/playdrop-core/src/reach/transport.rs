//! The iroh endpoint and the byte stream that carries TLS over it (protocol.md 12, SR-35,
//! SR-38).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use iroh::dns::DnsResolver;
use iroh::endpoint::{
    presets, ConnectOptions, Connection as QuicConnection, QuicTransportConfig, RecvStream,
    SendStream, VarInt,
};
use iroh::tls::CaTlsConfig;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl};
use rustls_pki_types::CertificateDer;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::{secret_key_for, wire, ReachConfig, ALPN};
use crate::identity::{DeviceId, DeviceIdentity};
use crate::{Error, Result};

/// Application close code: the gate refused this endpoint (SR-36).
pub const CLOSE_REFUSED: u32 = 0x5244;

/// Application close code: the stream ended and with it the connection.
const CLOSE_DONE: u32 = 0;

/// Flow control window of the one stream. *Choice:* 2 MiB, the most that is safe, for 20 MiB/s at
/// 100 ms where the library's default of 1.25 MiB gives 12. QUIC's stream buffer holds at most
/// 1024 separate pieces and kills the connection beyond that ("too many gaps in stream buffer").
/// Packets of about 1150 bytes with every other one lost make `window / 2300` pieces, and a relay
/// under load does lose many packets: measured in `bench/reach-netns.sh`, an 8 MiB window died
/// this way within a second.
const STREAM_WINDOW: u32 = 2 * 1024 * 1024;

/// The sending side's buffer. *Choice:* the receive window eight times over, the same ratio as
/// the library's default. A smaller one is not needed for a revoke's frame to get out: 20 rounds
/// of the revoke test passed with this value.
const SEND_WINDOW: u64 = 8 * STREAM_WINDOW as u64;

/// How long a dropped [`ReachIo`] keeps its QUIC connection, so bytes already written reach the
/// peer the way a TCP close delivers them. *Choice:* 1 s, the close grace of a replaced TCP
/// connection (protocol.md 3.5). The peer sees the end of the stream at once.
const LINGER: Duration = Duration::from_secs(1);

/// The transport settings of an accepting endpoint: the dialler gets one bidirectional stream
/// and nothing else (SR-38). A second stream gets no credit, so it cannot open, and a peer that
/// ignores that breaks QUIC's limit and loses the connection, while a pinned session on the
/// first stream carries on.
fn accept_transport() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(1))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .datagram_receive_buffer_size(None)
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .send_window(SEND_WINDOW)
        .build()
}

/// The settings of a connection we dial: the peer opens nothing towards us.
fn dial_transport() -> QuicTransportConfig {
    QuicTransportConfig::builder()
        .max_concurrent_bidi_streams(VarInt::from_u32(0))
        .max_concurrent_uni_streams(VarInt::from_u32(0))
        .datagram_receive_buffer_size(None)
        .stream_receive_window(VarInt::from_u32(STREAM_WINDOW))
        .send_window(SEND_WINDOW)
        .build()
}

/// Binds this device's endpoint: `presets::Minimal`, the one relay the user named, no address
/// lookup, no public DNS fallback (SR-35). Called only with a relay named.
pub(crate) async fn bind(
    identity: &DeviceIdentity,
    config: &ReachConfig,
    relay: RelayUrl,
) -> Result<Endpoint> {
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(secret_key_for(identity))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(RelayMode::custom([relay]))
        .clear_address_lookup()
        .transport_config(accept_transport())
        // The default falls back to public resolvers when the system's fail (SR-35): a relay
        // name is asked of the system's resolver and nowhere else.
        .dns_resolver(
            DnsResolver::builder()
                .with_system_defaults()
                .disable_fallback()
                .build(),
        );
    if !config.direct {
        builder = builder.clear_ip_transports();
    }
    let mut ca = CaTlsConfig::default();
    if !config.extra_ca_der.is_empty() {
        ca = ca.with_extra_roots(
            config
                .extra_ca_der
                .iter()
                .map(|der| CertificateDer::from(der.clone())),
        );
    }
    #[cfg(feature = "iroh-test")]
    if config.insecure_relay_tls {
        ca = CaTlsConfig::insecure_skip_verify();
    }
    builder = builder.ca_tls_config(ca);
    builder
        .bind()
        .await
        .map_err(|e| Error::Io(std::io::Error::other(format!("reach endpoint: {e}"))))
}

/// Opens the QUIC connection to `endpoint` through `relay`, with no address lookup (protocol.md
/// 12).
pub(crate) async fn connect(
    ep: &Endpoint,
    endpoint: EndpointId,
    relay: RelayUrl,
) -> Result<QuicConnection> {
    let addr = EndpointAddr::new(endpoint).with_relay_url(relay);
    let opts = ConnectOptions::new().with_transport_config(dial_transport());
    let connecting = ep
        .connect_with_opts(addr, ALPN, opts)
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    connecting
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))
}

/// Opens the one stream of a connection we dialled.
pub(crate) async fn open_stream(conn: &QuicConnection) -> Result<ReachIo> {
    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    Ok(ReachIo::new(send, recv, conn.clone()))
}

/// True if the peer's gate closed this connection (SR-36): it does not grant this host now.
pub(crate) fn was_refused(conn: &QuicConnection) -> bool {
    matches!(
        conn.close_reason(),
        Some(iroh::endpoint::ConnectionError::ApplicationClosed(c))
            if c.error_code == VarInt::from_u32(CLOSE_REFUSED)
    )
}

/// A reach stream on its way to the same TLS and HTTP/2 serving path as TCP (protocol.md 12).
pub struct ReachIncoming {
    /// The stream.
    pub(crate) io: ReachIo,
    /// [`super::synthetic_addr`] of the dialler.
    pub(crate) remote: SocketAddr,
    /// Whether a device may come in over this stream, asked after the inner TLS: it must hold
    /// the `remote` grant and have this Endpoint ID stored, now and not only when the QUIC
    /// connection was accepted (SR-36). The frame that stored an ID came from one paired peer
    /// and must not open the door for another whose grant is off.
    pub(crate) admit: Arc<dyn Fn(&DeviceId) -> bool + Send + Sync>,
    /// The QUIC connection, for the label of the connection in use (SR-39).
    pub(crate) quic: iroh::endpoint::WeakConnectionHandle,
}

impl std::fmt::Debug for ReachIncoming {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReachIncoming").finish_non_exhaustive()
    }
}

/// One QUIC bidirectional stream as a byte stream, exactly what TLS expects of a socket.
///
/// Dropping it ends the stream and, after [`LINGER`], the connection: there is one stream per
/// connection (SR-38).
#[derive(Debug)]
pub struct ReachIo {
    send: SendStream,
    recv: RecvStream,
    conn: Option<QuicConnection>,
}

impl ReachIo {
    pub(crate) fn new(send: SendStream, recv: RecvStream, conn: QuicConnection) -> Self {
        Self {
            send,
            recv,
            conn: Some(conn),
        }
    }
}

impl AsyncRead for ReachIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.recv).poll_read(cx, buf)
    }
}

impl AsyncWrite for ReachIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.send)
            .poll_write(cx, buf)
            .map_err(Into::into)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.send).poll_shutdown(cx)
    }
}

impl Drop for ReachIo {
    fn drop(&mut self) {
        let _ = self.send.finish();
        let Some(conn) = self.conn.take() else {
            return;
        };
        match tokio::runtime::Handle::try_current() {
            Ok(rt) => {
                rt.spawn(async move {
                    tokio::time::sleep(LINGER).await;
                    conn.close(VarInt::from_u32(CLOSE_DONE), b"");
                });
            }
            Err(_) => conn.close(VarInt::from_u32(CLOSE_DONE), b""),
        }
    }
}

/// Closes a connection the gate refused, before any TLS (SR-36).
pub(crate) fn refuse(conn: &QuicConnection) {
    conn.close(VarInt::from_u32(CLOSE_REFUSED), b"");
}

/// Parses the relay the user named, with the frame's rules: the peer has to accept the URL too.
pub(crate) fn parse_named_relay(url: &str) -> Result<RelayUrl> {
    wire::parse_relay(url)
        .map_err(|e| Error::BadRequest(format!("--reach-relay is not usable: {e}")))
}
