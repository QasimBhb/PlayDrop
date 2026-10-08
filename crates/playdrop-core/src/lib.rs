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
