//! Sources a sender reads from without a filesystem path (`docs/core-api.md` section 3,
//! PLAN.md anti-pattern 20): a mobile host shares a `content://` item as a reopenable
//! handle, never as a path. Resume checks follow protocol.md 7.6 (SR-21).
//!
//! A [`SourceOpener`] is foreign code on mobile, so every call to it runs on a blocking
//! thread (`spawn_blocking`); the returned `std::fs::File` becomes a `tokio::fs::File`.

use std::fmt;
use std::io::{self, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncSeekExt;

/// Opens one shared item read-only. Implemented by the host (one per shared `content://`
/// item on Android).
pub trait SourceOpener: Send + Sync + fmt::Debug {
    /// Opens the source read-only at offset 0. Called at offer time (metadata), once per
    /// `PUT` (seek to the resume offset) and once per bundle member. Must return a
    /// seekable regular file; a pipe fails the transfer at the first resume.
    fn open(&self) -> io::Result<std::fs::File>;
}

/// One file behind a reopenable handle (`docs/core-api.md` section 3).
#[derive(Debug, Clone)]
pub struct StreamSource {
    /// One path component (SR-17); the manifest path is `[name]`.
    pub name: String,
    /// Size in bytes; must equal what the opened file reports.
    pub size: u64,
    /// Modification time, Unix ms. 0 means unknown: mtime is then not part of the SR-21
    /// resume check.
    pub mtime_ms: i64,
    /// MIME type hint for the manifest.
    pub mime: Option<String>,
    /// Reopens the bytes.
    pub opener: Arc<dyn SourceOpener>,
}

/// Where a sender reads one file's bytes from.
#[derive(Debug, Clone)]
pub enum SourceRef {
    /// A local path (desktop, or a host's private directory).
    Path(PathBuf),
    /// A host-supplied handle (mobile share targets).
    Opener(Arc<dyn SourceOpener>),
}

impl PartialEq for SourceRef {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Path(a), Self::Path(b)) => a == b,
            (Self::Opener(a), Self::Opener(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Eq for SourceRef {}

impl SourceRef {
    /// The local path, for `Path` sources.
    pub fn path(&self) -> Option<&std::path::Path> {
        match self {
            Self::Path(p) => Some(p),
            Self::Opener(_) => None,
        }
    }

    /// True for a host-supplied handle.
    pub fn is_stream(&self) -> bool {
        matches!(self, Self::Opener(_))
    }

    /// Opens the source read-only at offset 0. An opener runs on a blocking thread, since
    /// it may call foreign code.
    pub async fn open(&self) -> io::Result<tokio::fs::File> {
        match self {
            Self::Path(p) => tokio::fs::File::open(p).await,
            Self::Opener(o) => {
                let o = Arc::clone(o);
                let f = tokio::task::spawn_blocking(move || o.open())
                    .await
                    .map_err(io::Error::other)??;
                Ok(tokio::fs::File::from_std(f))
            }
        }
    }

    /// Opens the source and seeks to `offset`.
    pub async fn open_at(&self, offset: u64) -> io::Result<tokio::fs::File> {
        let mut f = self.open().await?;
        if offset > 0 {
            f.seek(SeekFrom::Start(offset)).await?;
        }
        Ok(f)
    }
}
