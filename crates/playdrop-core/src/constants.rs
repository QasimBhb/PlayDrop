//! Protocol constants from `docs/protocol.md` section 1.

/// Wire protocol version this crate speaks.
pub const PROTOCOL_VERSION: u32 = 1;

/// Default TCP port for TLS 1.3 + HTTP/2 (configurable override).
pub const TCP_PORT: u16 = 42187;

/// UDP port for the multicast beacon.
pub const UDP_PORT: u16 = 42187;

/// IPv4 organization-local multicast group for the beacon.
pub const MULTICAST_GROUP: std::net::Ipv4Addr = std::net::Ipv4Addr::new(239, 255, 42, 187);

/// mDNS service type.
pub const MDNS_SERVICE: &str = "_playdrop._tcp.local.";

/// ALPN protocol identifier for version 1.
pub const ALPN_V1: &[u8] = b"playdrop/1";

/// Default transfer chunk size in bytes (1 MiB).
pub const DEFAULT_CHUNK_SIZE: u32 = 1024 * 1024;

/// Smallest and largest chunk sizes a peer may propose (64 KiB, 16 MiB).
pub const MIN_CHUNK_SIZE: u32 = 64 * 1024;
/// See [`MIN_CHUNK_SIZE`].
pub const MAX_CHUNK_SIZE: u32 = 16 * 1024 * 1024;

/// Maximum session frame length in bytes (64 KiB), protocol.md 5.2.
pub const MAX_FRAME_LEN: usize = 64 * 1024;

/// Pairing attempt timeout, protocol.md 6.2.
pub const PAIRING_TIMEOUT_SECS: u64 = 30;

/// "Add device" window length, protocol.md 6.1.
pub const ADD_DEVICE_WINDOW_SECS: u64 = 120;

/// Ports `PlayDrop` must never bind or announce on, protocol.md section 1.
pub const FORBIDDEN_PORTS: &[std::ops::RangeInclusive<u16>] = &[1714..=1764, 53317..=53317];
