//! `UniFFI` surface over `playdrop_core::host` for the Android and iOS hosts.
//!
//! The surface binds exactly `docs/core-api.md` (frozen 2026-10-09). Every type that
//! crosses the FFI is a plain record or enum defined in this crate and converted from the
//! core's types in one place, so the core can keep `PathBuf`, `SocketAddr`, `Secret<T>`
//! and friends while the bindings see strings and byte arrays.
//!
//! Threading model: [`host::PlayDropHost`] owns a tokio runtime. Async methods run their
//! work on that runtime and hand the result back through the `UniFFI` future bridge, so the
//! foreign side never needs a Rust executor. Events are delivered on one dedicated thread,
//! in order, through the foreign [`traits::HostListener`].
//!
//! Secrets: the SAS, clipboard text and clipboard images reach the foreign side as plain
//! strings and bytes (SR-32 applies on that side too: show them, never log them).

uniffi::setup_scaffolding!();

pub mod error;
pub mod events;
pub mod host;
pub mod logging;
pub mod traits;
pub mod types;

pub use error::PlayDropError;
pub use events::HostEvent;
pub use host::PlayDropHost;

/// Version of this library, for the app's about screen.
#[uniffi::export]
pub fn core_version() -> String {
    env!("CARGO_PKG_VERSION").to_owned()
}
