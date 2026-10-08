//! Crate-wide error type. Variants map to the error codes in `docs/protocol.md` section 9
//! where a wire code exists.

/// Result alias using [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// All errors the core can return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// TLS failure.
    #[error("tls: {0}")]
    Tls(String),

    /// Identity or certificate problem.
    #[error("identity: {0}")]
    Identity(String),

    /// Peer is not paired (wire code `not_paired`).
    #[error("not paired")]
    NotPaired,

    /// Grant missing for this device (wire code `forbidden`).
    #[error("forbidden: {0}")]
    Forbidden(&'static str),

    /// Malformed input (wire code `bad_request`).
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Manifest path breaks protocol.md 7.2 (wire code `path_invalid`).
    #[error("path invalid: {0}")]
    PathInvalid(String),

    /// Range start is neither the verified offset nor 0 (wire code `offset_mismatch`).
    #[error("offset mismatch: expected {expected}")]
    OffsetMismatch {
        /// The receiver's current verified offset.
        expected: u64,
    },

    /// A chunk failed its BLAKE3 check (wire code `chunk_hash_mismatch`).
    #[error("chunk hash mismatch at offset {offset}")]
    ChunkHashMismatch {
        /// File offset of the failed chunk.
        offset: u64,
    },

    /// Whole-file root differs (wire code `root_mismatch`).
    #[error("root hash mismatch")]
    RootMismatch,

    /// Tar member differs from the manifest (wire code `bundle_mismatch`).
    #[error("bundle mismatch: {0}")]
    BundleMismatch(String),

    /// Rate or concurrency limit exceeded (wire code `rate_limited`).
    #[error("rate limited")]
    RateLimited,

    /// Would cross the free-space reserve (wire code `no_space`).
    #[error("no space")]
    NoSpace,

    /// Manifest over limits (wire code `too_large`).
    #[error("too large: {0}")]
    TooLarge(String),

    /// Unknown ID, or an ID bound to another device (wire code `not_found`).
    #[error("not found")]
    NotFound,

    /// Missing or wrong `pd-token` (wire code `bad_token`).
    #[error("bad token")]
    BadToken,

    /// Transfer was cancelled (wire code `cancelled`).
    #[error("cancelled")]
    Cancelled,

    /// A source file changed under the sender (cancel reason `source_changed`).
    #[error("source changed: {0}")]
    SourceChanged(String),

    /// A key or data file has permissions looser than the spec allows (SR-28).
    #[error("insecure permissions on {path}: mode {mode:o}")]
    InsecurePermissions {
        /// Offending path.
        path: std::path::PathBuf,
        /// Observed mode bits.
        mode: u32,
    },

    /// Protocol violation that closes the connection.
    #[error("protocol: {0}")]
    Protocol(String),

    /// Pairing failed (reason as in protocol.md 6.2).
    #[error("pairing rejected: {0}")]
    PairRejected(&'static str),

    /// The listening port is held by another process (SR-31). There is no random-port
    /// fallback (protocol.md section 1).
    #[error("port {port} is held by another process")]
    PortHeld {
        /// The configured port.
        port: u16,
    },

    /// JSON encode or decode failure.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}
