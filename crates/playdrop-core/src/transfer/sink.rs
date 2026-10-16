//! Receiving without paths (`docs/core-api.md` section 2, PLAN.md anti-pattern 20): a
//! mobile host stages and verifies under its private staging directory, then hands each
//! verified file to a [`ReceiveSink`] (an Android `content://` URI, a path, or any string
//! its UI can open). A streamed file reaches the sink only after its root verifies
//! (protocol.md 7.4, SR-22); a bundle member as soon as its own bytes are chunk-verified and
//! durable, before the bundle root (protocol.md 11.1).

use std::fmt;

use serde::{Deserialize, Serialize};

/// Receives verified files in `Staged` mode. Implemented by the host.
pub trait ReceiveSink: Send + Sync + fmt::Debug {
    /// Called once per verified file (and once per empty file), from a blocking thread:
    /// after the root hash check for a streamed file; for a bundle member after the
    /// member's bytes are chunk-verified and durable, before the bundle root (so while the
    /// bundle still streams, from the core's own delivery thread). After a process kill
    /// the one file in flight may be delivered a second time. `file` is the verified
    /// staging file, open read-only at offset 0. The sink copies or moves it to its final
    /// place and returns where it went (an Android `content://` URI, a path, or any string
    /// the UI can open). The core deletes the staging file afterwards whatever the sink
    /// did.
    fn deliver(&self, item: DeliverItem, file: std::fs::File) -> Result<String, SinkError>;

    /// Called once per empty directory in the manifest (`dirs`).
    fn make_dir(&self, path: Vec<String>) -> Result<(), SinkError>;
}

/// What the sink is told about one verified file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliverItem {
    /// Transfer ID.
    pub transfer_id: String,
    /// Sender's Device ID, hex.
    pub peer: String,
    /// Manifest path components, SR-17 validated. Still peer-supplied: the sink must not
    /// treat them as a trusted path.
    pub path: Vec<String>,
    /// Size in bytes.
    pub size: u64,
    /// Modification time from the manifest, Unix ms.
    pub mtime_ms: i64,
    /// MIME hint from the manifest.
    pub mime: Option<String>,
}

/// A sink failure. The transfer fails with this text (SR-22 wording: say what failed, no
/// secrets).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct SinkError {
    /// Shown to the user.
    pub message: String,
}

impl SinkError {
    /// A sink error with this message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Where one received file went, reported in `HostEvent::TransferComplete` in both modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveredFile {
    /// Manifest path, components joined with `/`.
    pub path: String,
    /// The final path in `Directory` mode, the sink's string in `Staged` mode.
    pub location: String,
}
