//! Transfer engine: manifest validation, chunk records, streaming BLAKE3, `.part` staging,
//! resume, deterministic tar bundle.
//! Implements `docs/protocol.md` section 7, SR-17 to SR-23.
//!
//! The module is network-agnostic. Senders produce chunk-record bodies as
//! [`tokio::io::AsyncRead`] (or a `Stream` of `Bytes`) through [`chunk::ChunkEncoder`];
//! receivers consume any `AsyncRead` body through [`receiver::TransferReceiver`]. The
//! HTTP/2 wiring of protocol.md 7.1 sits on top of this.

pub mod bundle;
pub mod chunk;
mod fsx;
pub mod manifest;
pub mod receiver;
pub mod sender;
pub mod sink;
pub mod source;

#[cfg(test)]
mod e2e_tests;

pub use bundle::{
    bundle_size, BundleEvent, BundleLayout, BundleReader, BundleWriter, MemberOpener,
};
pub use chunk::{
    expected_body_len, prefix_hash, root_hash, ChunkDecoder, ChunkEncoder, ChunkHash, VerifiedChunk,
};
pub use manifest::{BundleInfo, FileEntry, FileId, Manifest, ManifestBuilder, SourceInfo};
pub use receiver::{
    AcceptOptions, FileState, FreeSpace, PartFile, PutError, PutResponse, SinkBinding, SpaceInfo,
    SyncPolicy, TransferReceiver, TransferState, TransferStateView,
};
pub use sender::{ResumeAction, ResumePlan, TransferSender};
pub use sink::{DeliverItem, DeliveredFile, ReceiveSink, SinkError};
pub use source::{SourceOpener, SourceRef, StreamSource};

/// Encodes a 32-byte hash as lowercase hex, the form roots and prefixes take on the wire
/// (protocol.md 7.4, 7.5).
pub fn hash_to_hex(hash: &ChunkHash) -> String {
    data_encoding::HEXLOWER.encode(hash)
}

/// Parses a lowercase or uppercase 64-character hex hash (protocol.md 7.4 `pd-root`).
pub fn hash_from_hex(hex: &str) -> crate::Result<ChunkHash> {
    let bytes = data_encoding::HEXLOWER_PERMISSIVE
        .decode(hex.as_bytes())
        .map_err(|_| crate::Error::BadRequest("hash is not hex".into()))?;
    bytes
        .try_into()
        .map_err(|_| crate::Error::BadRequest("hash is not 32 bytes".into()))
}
