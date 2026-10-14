//! `PlayDrop` core library.
//!
//! Owns device identity, the pin store, TLS, discovery, pairing, the session plane and the
//! transfer engine. Hosts (the desktop daemon, the Android foreground service, the iOS app)
//! embed this crate; UIs never own the listening socket.
//!
//! The wire behaviour is specified in `docs/protocol.md` and the security requirements in
//! `docs/threat-model.md`. Module docs cite the section and SR numbers they implement.

pub mod constants;
pub mod discovery;
pub mod error;
pub mod host;
pub mod identity;
pub mod net;
pub mod pin_store;
pub mod session;
pub mod tls;
pub mod transfer;

pub use error::{Error, Result};

/// Entry points for the cargo-fuzz targets in `fuzz/` (threat-model.md SR-33, T39). Built only
/// under `--cfg fuzzing`, which cargo-fuzz sets; not part of the public API.
#[cfg(fuzzing)]
#[doc(hidden)]
pub mod fuzz_api {
    use crate::discovery::Advert;

    /// Decodes raw mDNS TXT RDATA (length-prefixed strings, RFC 6763 section 6) with the same
    /// `mdns-sd` decoder the browser uses, then parses it as an advert with `srv_port`
    /// (protocol.md 8.1).
    pub fn mdns_txt(rdata: &[u8], srv_port: u16) -> Option<Advert> {
        crate::discovery::mdns::txt_advert(&mdns_sd::TxtProperties::from(rdata), srv_port)
    }
}
