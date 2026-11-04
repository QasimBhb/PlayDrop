//! Reach across networks through iroh: desktop hosts that met on a LAN reach each other
//! through a relay the user named, or a hole-punched path found through it. A spike behind
//! the `iroh` cargo feature (protocol.md section 12, SR-35 to SR-40,
//! `docs/design/iroh-reach.md`); release builds contain none of this.
//!
//! One QUIC bidirectional stream carries exactly what a TCP socket carries, TLS 1.3 and
//! HTTP/2. The Endpoint ID only decides whether to spend effort: the inner TLS against the
//! pin decides trust (SR-37).

pub(crate) mod store;
pub(crate) mod transport;
pub mod wire;

use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::time::Duration;

use iroh::{EndpointId, SecretKey};

use crate::identity::DeviceIdentity;

/// The outer QUIC ALPN (protocol.md 12).
pub const ALPN: &[u8] = b"playdrop/reach/1";

/// The BLAKE3 `derive_key` context of the endpoint key (protocol.md 12).
const KEY_CONTEXT: &str = "playdrop 2026 reach endpoint key v1";

/// Capability string in both `caps` lists (protocol.md 12). Built as
/// `Capability::Unknown`, so the capability enum of a build without the feature is untouched.
pub const CAPABILITY: &str = "reach";

/// What a report shows for a reach connection: the prefix alone, with no tag of the Endpoint ID
/// (SR-40). [`is_reach_addr`] is true of it.
pub const MASKED_ADDR: SocketAddr = SocketAddr::V6(SocketAddrV6::new(
    Ipv6Addr::new(0x0100, 0, 0, 0, 0, 0, 0, 0),
    0,
    0,
    0,
));

/// What the user is told wherever reach is turned on (SR-39). No URL in it (SR-40).
pub const RELAY_NOTICE: &str = "Reach is on: the relay you named can see which devices connect, \
     when, and how much data moves, though not the contents. The device's endpoint ID stays the \
     same until its key changes.";

/// Log targets of iroh and what it brings along. They name the relay at info level (`home is now
/// relay ...`) and put a tag of the Endpoint ID on every line (`endpoint{id=...}`), which SR-40
/// does not allow above debug level, so a host's log setup keeps them off ([`quiet_log_filter`]).
pub const QUIET_LOG_TARGETS: &[&str] = &[
    "iroh",
    "iroh_base",
    "iroh_dns",
    "iroh_relay",
    "n0_dns_resolver",
    "n0_watcher",
    "netwatch",
    "noq",
    "noq_proto",
    "noq_udp",
];

/// `filter` (a `tracing` filter string) with [`QUIET_LOG_TARGETS`] off, unless it names `iroh`
/// itself: someone debugging reach asks for it by name (SR-40).
pub fn quiet_log_filter(filter: &str) -> String {
    if filter.contains("iroh") {
        return filter.to_owned();
    }
    let mut out = filter.to_owned();
    for target in QUIET_LOG_TARGETS {
        if !out.is_empty() {
            out.push(',');
        }
        out.push_str(target);
        out.push_str("=off");
    }
    out
}

/// What the user is told when the grant is given with no relay named (SR-39).
pub const NO_RELAY_NOTICE: &str =
    "No relay is named, so nothing happens yet: start the host with --reach-relay <url>.";

/// How long one reach dial may take. *Choice:* a relay round trip plus a QUIC and a TLS
/// handshake take well under a second on any path worth using, and the dial lock is held
/// meanwhile, so a send waits on this.
pub const DIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Shortest time between two reach dials of one peer (default of
/// [`ReachConfig::retry_every`]). *Choice:* the LAN retry runs every 5 s and costs nothing
/// beyond the LAN; each reach attempt talks to the relay and tells it who is being looked for,
/// and with the 10 s dial timeout a shorter wait would pile dials up behind the dial lock.
pub const RETRY_EVERY: Duration = Duration::from_secs(15);

/// Reach settings of a host. No such value means no endpoint at all (SR-35).
#[derive(Debug, Clone)]
pub struct ReachConfig {
    /// The relay the user named. The only relay this host ever talks to.
    pub relay: String,
    /// Allow a direct path across networks (`--reach-direct`). Off, the endpoint has no IP
    /// socket and everything goes through the relay. *Choice:* off by default: a hole-punching
    /// host answers an unknown endpoint with UDP probes to whatever address that endpoint names,
    /// from this host's own address, before the gate can refuse it (threat-model.md T45, measured
    /// by a test). Only someone who turned this on takes that on.
    pub direct: bool,
    /// Shortest time between two reach dials of one peer; tests shorten it.
    pub retry_every: Duration,
    /// Extra certificate authorities (DER) to trust for the relay, for a self-hosted relay
    /// with a private CA.
    pub extra_ca_der: Vec<Vec<u8>>,
    /// Trust any relay certificate: the in-process test relay signs its own.
    #[cfg(feature = "iroh-test")]
    pub insecure_relay_tls: bool,
    /// Never dial over reach, only accept, so a test knows who dialled.
    #[cfg(feature = "iroh-test")]
    pub never_dial: bool,
    /// How long a revoke waits before it closes reach connections, when a test needs another
    /// value than the normal 1 s.
    #[cfg(feature = "iroh-test")]
    pub revoke_grace: Option<Duration>,
}

impl ReachConfig {
    /// Settings for `relay`, relay only.
    pub fn new(relay: impl Into<String>) -> Self {
        Self {
            relay: relay.into(),
            direct: false,
            retry_every: RETRY_EVERY,
            extra_ca_der: Vec::new(),
            #[cfg(feature = "iroh-test")]
            insecure_relay_tls: false,
            #[cfg(feature = "iroh-test")]
            never_dial: false,
            #[cfg(feature = "iroh-test")]
            revoke_grace: None,
        }
    }

    /// Also trusts the certificates in `pem` for the relay. Fails if there are none or one does
    /// not decode.
    pub fn add_ca_pem(&mut self, pem: &str) -> crate::Result<()> {
        const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
        const END: &str = "-----END CERTIFICATE-----";
        let mut rest = pem;
        let mut added = 0;
        while let Some(start) = rest.find(BEGIN) {
            let body = &rest[start + BEGIN.len()..];
            let end = body
                .find(END)
                .ok_or_else(|| crate::Error::BadRequest("unterminated certificate".into()))?;
            let text: String = body[..end].split_whitespace().collect();
            let der = data_encoding::BASE64
                .decode(text.as_bytes())
                .map_err(|e| crate::Error::BadRequest(format!("bad certificate: {e}")))?;
            self.extra_ca_der.push(der);
            added += 1;
            rest = &body[end + END.len()..];
        }
        if added == 0 {
            return Err(crate::Error::BadRequest(
                "no certificate in the file".into(),
            ));
        }
        Ok(())
    }
}

/// How a live reach connection currently carries its data (SR-39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReachPathKind {
    /// Through the relay.
    Relay,
    /// A direct path across networks, found through the relay.
    Direct,
}

/// A peer currently reached across networks, for `playdrop peers` and `playdrop diagnose`
/// (SR-39, anti-patterns 13 and 18).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachPath {
    /// Short ID of the peer.
    pub short_id: String,
    /// Local label of the peer.
    pub label: String,
    /// How the data goes now.
    pub kind: ReachPathKind,
}

impl ReachPath {
    /// The status line (SR-39). Carries no URL and no Endpoint ID (SR-40).
    pub fn status_line(&self) -> String {
        match self.kind {
            ReachPathKind::Relay => format!(
                "note: {} is on the relay, the slow path: the relay can see that you connect, \
                 when, and how much moves",
                self.label
            ),
            ReachPathKind::Direct => format!(
                "note: {} is on a direct path across networks, the slow path: the relay only \
                 helped set it up",
                self.label
            ),
        }
    }
}

/// The iroh secret key of this device (protocol.md 12). A separate key, so the device key is
/// not used in two protocols (SR-37).
pub fn secret_key_for(identity: &DeviceIdentity) -> SecretKey {
    SecretKey::from_bytes(&blake3::derive_key(KEY_CONTEXT, identity.private_key_der()))
}

/// The synthetic remote address of a reach connection (protocol.md 12): `100::/64` (RFC 6666,
/// discard) plus the first 8 bytes of BLAKE3 of the Endpoint ID, port 0. Never stored as
/// last-known, never dialled, never advertised; it gives the per-address limit of SR-15 a
/// per-peer meaning.
pub fn synthetic_addr(endpoint: &EndpointId) -> SocketAddr {
    let hash = blake3::hash(endpoint.as_bytes());
    let mut octets = [0u8; 16];
    octets[0] = 0x01;
    octets[8..].copy_from_slice(&hash.as_bytes()[..8]);
    SocketAddr::V6(SocketAddrV6::new(Ipv6Addr::from(octets), 0, 0, 0))
}

/// True for an address made by [`synthetic_addr`].
pub fn is_reach_addr(addr: SocketAddr) -> bool {
    match addr {
        SocketAddr::V6(v6) => {
            let s = v6.ip().segments();
            s[0] == 0x0100 && s[1] == 0 && s[2] == 0 && s[3] == 0 && v6.port() == 0
        }
        SocketAddr::V4(_) => false,
    }
}

/// An address fit for an event or a report: a reach connection's becomes [`MASKED_ADDR`], the
/// rest stay as they are.
pub fn masked_addr(addr: SocketAddr) -> SocketAddr {
    if is_reach_addr(addr) {
        MASKED_ADDR
    } else {
        addr
    }
}

/// An address fit for an info line or a report: a reach connection's synthetic address is a
/// stable tag of its Endpoint ID, so it reads "across networks" instead (SR-40).
pub fn display_addr(addr: SocketAddr) -> String {
    if is_reach_addr(addr) {
        "across networks".to_owned()
    } else {
        addr.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthetic_address_is_in_the_discard_prefix_and_stable() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        let a = synthetic_addr(&key);
        assert!(is_reach_addr(a));
        assert_eq!(a, synthetic_addr(&key));
        assert_eq!(a.port(), 0);
        assert!(a.to_string().starts_with("[100::"), "{a}");
        let other = SecretKey::from_bytes(&[8; 32]).public();
        assert_ne!(a, synthetic_addr(&other));
    }

    #[test]
    fn the_masked_address_says_reach_and_nothing_else() {
        let key = SecretKey::from_bytes(&[7; 32]).public();
        assert_eq!(masked_addr(synthetic_addr(&key)), MASKED_ADDR);
        assert!(is_reach_addr(MASKED_ADDR));
        assert_eq!(MASKED_ADDR.to_string(), "[100::]:0");
        let lan = "10.0.0.1:42187".parse().unwrap();
        assert_eq!(masked_addr(lan), lan);
    }

    #[test]
    fn real_addresses_are_not_reach_addresses() {
        for a in [
            "10.0.0.1:42187",
            "[::1]:42187",
            "[fe80::1%3]:42187",
            "[2001:db8::1]:42187",
            "[100:0:0:1::1]:0",
            "[100::1]:42187",
        ] {
            assert!(!is_reach_addr(a.parse().unwrap()), "{a}");
        }
    }

    #[test]
    fn the_endpoint_key_is_not_the_device_key() {
        let identity = DeviceIdentity::generate().unwrap();
        let key = secret_key_for(&identity);
        assert_eq!(
            key.to_bytes(),
            secret_key_for(&identity).to_bytes(),
            "stable until the device key changes"
        );
        let other = DeviceIdentity::generate().unwrap();
        assert_ne!(key.to_bytes(), secret_key_for(&other).to_bytes());
        // Not any 32-byte window of the private key's DER.
        let der = identity.private_key_der();
        assert!(!der.windows(32).any(|w| w == key.to_bytes()));
    }

    /// SR-39 and SR-40: the status lines say so, and what is printed names no URL or ID.
    #[test]
    fn status_lines_say_what_the_path_is_and_that_it_is_slow() {
        let on = |kind| ReachPath {
            short_id: "k3qd".into(),
            label: "Laptop".into(),
            kind,
        };
        let relay = on(ReachPathKind::Relay).status_line();
        assert!(relay.contains("Laptop is on the relay"), "{relay}");
        assert!(relay.contains("slow path"), "{relay}");
        let direct = on(ReachPathKind::Direct).status_line();
        assert!(direct.contains("direct path across networks"), "{direct}");
        assert!(direct.contains("slow path"), "{direct}");
        assert!(RELAY_NOTICE.contains("can see which devices connect"));
        assert!(!RELAY_NOTICE.contains("http"));
    }

    #[test]
    fn iroh_stays_out_of_the_log_unless_asked_for() {
        let f = quiet_log_filter("playdrop_core=warn");
        assert!(f.starts_with("playdrop_core=warn,"), "{f}");
        assert!(f.contains("iroh=off") && f.contains("noq_proto=off"), "{f}");
        assert!(quiet_log_filter("").starts_with("iroh=off"));
        assert_eq!(quiet_log_filter("iroh=debug"), "iroh=debug");
    }

    #[test]
    fn ca_files_parse_or_fail_cleanly() {
        let mut c = ReachConfig::new("https://relay.example.org");
        let pem = "junk\n-----BEGIN CERTIFICATE-----\nAAEC\nAwQ=\n-----END CERTIFICATE-----\n";
        c.add_ca_pem(pem).unwrap();
        assert_eq!(c.extra_ca_der, vec![vec![0, 1, 2, 3, 4]]);
        assert!(c.add_ca_pem("no certificate here").is_err());
        assert!(c.add_ca_pem("-----BEGIN CERTIFICATE-----\nAAEC").is_err());
        assert!(c
            .add_ca_pem("-----BEGIN CERTIFICATE-----\n!!\n-----END CERTIFICATE-----")
            .is_err());
    }

    #[test]
    fn display_addr_hides_only_the_synthetic_address() {
        let key = SecretKey::from_bytes(&[9; 32]).public();
        assert_eq!(display_addr(synthetic_addr(&key)), "across networks");
        assert_eq!(
            display_addr("192.168.1.2:42187".parse().unwrap()),
            "192.168.1.2:42187"
        );
    }
}
