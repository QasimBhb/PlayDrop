//! The one error type the bindings see. Core errors map onto a small set of variants the
//! app can act on (not paired, forbidden, locked, too large, port held) plus a catch-all
//! with the core's message.

use playdrop_core::Error as CoreError;

/// Error thrown by every fallible call. Kotlin sees `PlayDropException.<Variant>`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, uniffi::Error)]
pub enum PlayDropError {
    /// The peer is not paired, or no session is up.
    #[error("not paired: {detail}")]
    NotPaired {
        /// Detail.
        detail: String,
    },
    /// A grant is missing on this side (SR-14).
    #[error("forbidden: {detail}")]
    Forbidden {
        /// Detail.
        detail: String,
    },
    /// The device is locked; pairing is refused (SR-6).
    #[error("locked: {detail}")]
    Locked {
        /// Detail.
        detail: String,
    },
    /// No peer, transfer or item with that ID.
    #[error("not found: {detail}")]
    NotFound {
        /// Detail.
        detail: String,
    },
    /// The payload exceeds a limit (clipboard, manifest).
    #[error("too large: {detail}")]
    TooLarge {
        /// Detail.
        detail: String,
    },
    /// The listening port is already held (SR-31).
    #[error("port {port} is held by another process")]
    PortHeld {
        /// The port.
        port: u16,
    },
    /// Data dir or key file permissions are too loose (SR-28).
    #[error("insecure permissions: {detail}")]
    InsecurePermissions {
        /// Detail.
        detail: String,
    },
    /// Not enough free space for the transfer (SR-20).
    #[error("no space: {detail}")]
    NoSpace {
        /// Detail.
        detail: String,
    },
    /// A path or name broke the SR-17 rules.
    #[error("invalid path: {detail}")]
    PathInvalid {
        /// Detail.
        detail: String,
    },
    /// Pairing was rejected or failed.
    #[error("pairing rejected: {detail}")]
    PairRejected {
        /// Reason.
        detail: String,
    },
    /// Input or output failure.
    #[error("io: {detail}")]
    Io {
        /// Detail.
        detail: String,
    },
    /// Anything else, with the core's message.
    #[error("{detail}")]
    Other {
        /// Detail.
        detail: String,
    },
}

impl PlayDropError {
    /// A catch-all error with a message.
    pub fn other(message: impl Into<String>) -> Self {
        Self::Other {
            detail: message.into(),
        }
    }

    /// An I/O error with a message.
    pub fn io(message: impl Into<String>) -> Self {
        Self::Io {
            detail: message.into(),
        }
    }
}

impl From<CoreError> for PlayDropError {
    fn from(e: CoreError) -> Self {
        let detail = e.to_string();
        match e {
            CoreError::NotPaired => Self::NotPaired { detail },
            CoreError::Forbidden(_) => Self::Forbidden { detail },
            CoreError::Locked => Self::Locked { detail },
            CoreError::NotFound => Self::NotFound { detail },
            CoreError::TooLarge(_) => Self::TooLarge { detail },
            CoreError::PortHeld { port } => Self::PortHeld { port },
            CoreError::InsecurePermissions { .. } => Self::InsecurePermissions { detail },
            CoreError::NoSpace => Self::NoSpace { detail },
            CoreError::PathInvalid(_) => Self::PathInvalid { detail },
            CoreError::PairRejected(_) => Self::PairRejected { detail },
            CoreError::Io(_) => Self::Io { detail },
            _ => Self::Other { detail },
        }
    }
}

impl From<std::io::Error> for PlayDropError {
    fn from(e: std::io::Error) -> Self {
        Self::Io {
            detail: e.to_string(),
        }
    }
}

/// A foreign implementation panicked or threw something that is not a `PlayDropError`
/// (see the `UniFFI` foreign-trait docs): surfaced as [`PlayDropError::Other`] instead of a
/// Rust panic.
impl From<uniffi::UnexpectedUniFFICallbackError> for PlayDropError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        Self::Other {
            detail: format!("foreign callback failed: {}", e.reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_errors_map_to_actionable_variants() {
        assert!(matches!(
            PlayDropError::from(CoreError::NotPaired),
            PlayDropError::NotPaired { .. }
        ));
        assert!(matches!(
            PlayDropError::from(CoreError::PortHeld { port: 42187 }),
            PlayDropError::PortHeld { port: 42187 }
        ));
        assert!(matches!(
            PlayDropError::from(CoreError::Protocol("x".into())),
            PlayDropError::Other { .. }
        ));
    }
}
