//! Receive side of the transfer plane: `.part` staging, durable verified offsets, tokens,
//! offset rules, bounded retries, bundle extraction, cancel and idle cleanup.
//! Implements protocol.md 7.2 to 7.7 and SR-17 to SR-23.
//!
//! Layout on disk, all beneath the receive root:
//!
//! ```text
//! <root>/.playdrop-partial/<transfer id>/      0700
//!     transfer.json        manifest, tokens, skipped fids, transfer state
//!     <fid>.part           staged bytes of a streamed file (0600, never executable)
//!     <fid>.json           durable verified offset and state for that file
//!     <fid>.hashes         append-only chunk hashes (32 bytes each)
//!     bundle.json, bundle.hashes   the same for the bundle stream
//!     <fid>.part           staged bytes of each bundle member
//! ```
//!
//! A file leaves staging only after its root verifies, through an exclusive create of the
//! final name (with " (n)" on collision) and a rename onto that placeholder (SR-18).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncRead;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

use super::bundle::{BundleEvent, BundleLayout, BundleReader};
use super::chunk::{
    check_range, chunk_count, prefix_hash, root_hash, ChunkDecoder, ChunkHash, VerifiedChunk,
    HASH_LEN,
};
use super::fsx;
use super::hash_to_hex;
use super::manifest::{is_valid_chunk_size, is_valid_transfer_id, Manifest};
use crate::{Error, Result};

/// Name of the staging folder inside the receive root. Manifest paths may not start with
/// it (checked case-insensitively at accept).
pub const STAGING_DIR: &str = ".playdrop-partial";

/// How many times one chunk may be re-sent after failing its hash (SR-22).
pub const MAX_CHUNK_RETRIES: u32 = 3;

/// A `.part` idle this long is deleted and its transfer failed (protocol.md 7.6, SR-20).
pub const IDLE_TIMEOUT: Duration = Duration::from_hours(24);

/// Minimum free-space reserve (2 GiB); the reserve is the larger of this and 5% of the
/// volume (SR-20).
pub const MIN_RESERVE: u64 = 2 * 1024 * 1024 * 1024;

/// Free and total bytes of the volume holding the receive root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceInfo {
    /// Bytes available to this process.
    pub available: u64,
    /// Volume size.
    pub total: u64,
}

impl SpaceInfo {
    /// Reserve that must stay free: `max(2 GiB, 5% of the volume)` (SR-20).
    pub fn reserve(&self) -> u64 {
        MIN_RESERVE.max(self.total / 20)
    }

    /// True when `bytes` more can be written without crossing the reserve.
    pub fn fits(&self, bytes: u64) -> bool {
        self.available
            .checked_sub(self.reserve())
            .is_some_and(|room| room >= bytes)
    }
}

/// Reports free space for a path (SR-20). The host implements it (`host::platform::VolumeSpace`
/// uses `statvfs` on Unix). Called at accept and before every chunk write, so keep it cheap.
pub trait FreeSpace: Send + Sync {
    /// Space on the volume holding `path`.
    fn space(&self, path: &Path) -> io::Result<SpaceInfo>;
}

/// When verified offsets are made durable (protocol.md 7.6): after `bytes` new verified
/// bytes or `interval`, whichever comes first, and always at the end of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncPolicy {
    /// Byte threshold (default 16 MiB).
    pub bytes: u64,
    /// Time threshold (default 1 s).
    pub interval: Duration,
}

impl Default for SyncPolicy {
    fn default() -> Self {
        Self {
            bytes: 16 * 1024 * 1024,
            interval: Duration::from_secs(1),
        }
    }
}

/// File state, protocol.md 7.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileState {
    /// Nothing verified yet.
    Waiting,
    /// Some bytes verified.
    Receiving,
    /// Root verified and file in place.
    Verified,
    /// Deselected by the user; no token.
    Skipped,
    /// Integrity failure; `.part` deleted (SR-22).
    Failed,
}

/// Transfer state, protocol.md 7.5.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransferState {
    /// Waiting for the user.
    Pending,
    /// Accepted, nothing streamed yet.
    Accepted,
    /// Streaming.
    Active,
    /// Every file verified or skipped.
    Complete,
    /// Declined by the user.
    Declined,
    /// Cancelled by either side.
    Cancelled,
    /// A file failed and nothing else is outstanding, or idle cleanup ran.
    Failed,
    /// The offer expired.
    Expired,
}

/// `GET /v1/transfers/{id}` body, protocol.md 7.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferStateView {
    /// Transfer ID.
    pub id: String,
    /// Transfer state.
    pub state: TransferState,
    /// Binding chunk size.
    pub chunk_size: u32,
    /// Manifest `totalBytes`.
    pub total_bytes: u64,
    /// File bytes verified so far, bundle members included.
    pub bytes_verified: u64,
    /// Per-file state.
    pub files: Vec<FileStateView>,
    /// Bundle state, when the manifest has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle: Option<StreamStateView>,
}

/// One entry of [`TransferStateView::files`], protocol.md 7.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStateView {
    /// Manifest fid.
    pub fid: u32,
    /// File state.
    pub state: FileState,
    /// Declared size.
    pub size: u64,
    /// Verified, chunk-aligned offset.
    pub verified_offset: u64,
    /// Hex root, once verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Hex prefix hash, when `0 < verifiedOffset < size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// `pd-token` for this file; absent for skipped and bundled files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// The `bundle` entry of [`TransferStateView`], protocol.md 7.5.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStateView {
    /// Bundle state.
    pub state: FileState,
    /// Exact tar size.
    pub size: u64,
    /// Verified offset in the tar stream.
    pub verified_offset: u64,
    /// Hex root, once verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// Hex prefix hash, when `0 < verifiedOffset < size`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// `pd-token` for the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Reply body of a file or bundle `PUT`, protocol.md 7.4. Also the body extras of a 422.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutResponse {
    /// Fid, absent for the bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fid: Option<u32>,
    /// File or bundle state after the request.
    pub state: FileState,
    /// Verified offset after the request.
    pub verified_offset: u64,
    /// Hex root when verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
}

/// Errors of the `PUT` entry points that have no [`Error`] variant yet (`bad_token` 401 and
/// `cancelled` 410, protocol.md section 9). Everything else is [`PutError::Core`].
#[derive(Debug, thiserror::Error)]
pub enum PutError {
    /// Missing or wrong `pd-token` (wire code `bad_token`, 401).
    #[error("bad token")]
    BadToken,
    /// The transfer was cancelled (wire code `cancelled`, 410).
    #[error("cancelled")]
    Cancelled,
    /// Any other error.
    #[error(transparent)]
    Core(#[from] Error),
}

/// Options for [`TransferReceiver::accept`].
#[derive(Debug, Clone, Default)]
pub struct AcceptOptions {
    /// The receiver's binding chunk size; `None` takes the sender's proposal (7.2).
    pub chunk_size: Option<u32>,
    /// Deselected fids; they become `skipped` with no token (7.3).
    pub skip: Vec<u32>,
    /// Durability batching.
    pub sync: SyncPolicy,
}

// ---------------------------------------------------------------------------------------
// Progress and the durable ledger
// ---------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Progress {
    state: FileState,
    verified_offset: u64,
    hashes: Vec<ChunkHash>,
    root: Option<ChunkHash>,
    /// Offset of the chunk that last failed and how many times in a row.
    retry: Option<(u64, u32)>,
}

impl Progress {
    fn fresh() -> Self {
        Self {
            state: FileState::Waiting,
            verified_offset: 0,
            hashes: Vec::new(),
            root: None,
            retry: None,
        }
    }
}

type SharedProgress = Arc<StdMutex<Progress>>;

fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain data behind; keep going with it.
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LedgerRecord {
    v: u32,
    size: u64,
    chunk_size: u32,
    verified_offset: u64,
    hash_count: u64,
    state: FileState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
    /// Where verified files were placed, relative to the receive root, so a transfer
    /// resumed after a restart can still report every destination.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    saved: Vec<SavedFile>,
}

/// One placed file: its manifest fid and its final path relative to the receive root
/// (after any " (n)" collision rename).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedFile {
    fid: u32,
    path: PathBuf,
}

/// Durable verified offset plus chunk hashes for one stream (a file or the bundle).
///
/// Hashes go to an append-only binary file and a small JSON record names how many of them
/// and which offset are durable, so each checkpoint writes only the new hashes instead of
/// rewriting the whole list (protocol.md 7.6 durability).
#[derive(Debug)]
struct Ledger {
    json: PathBuf,
    hashes_path: PathBuf,
    hashes_file: Option<File>,
    persisted: usize,
    size: u64,
    chunk_size: u32,
    policy: SyncPolicy,
    unsynced: u64,
    last_sync: Instant,
    progress: SharedProgress,
    /// Final destinations of the files this stream placed, written with every record.
    saved: Vec<SavedFile>,
}

impl Ledger {
    fn paths(dir: &Path, stem: &str) -> (PathBuf, PathBuf) {
        (
            dir.join(format!("{stem}.json")),
            dir.join(format!("{stem}.hashes")),
        )
    }

    fn fresh(dir: &Path, stem: &str, size: u64, chunk_size: u32, policy: SyncPolicy) -> Self {
        let (json, hashes_path) = Self::paths(dir, stem);
        Self {
            json,
            hashes_path,
            hashes_file: None,
            persisted: 0,
            size,
            chunk_size,
            policy,
            unsynced: 0,
            last_sync: Instant::now(),
            progress: Arc::new(StdMutex::new(Progress::fresh())),
            saved: Vec::new(),
        }
    }

    /// Loads the durable state, dropping hashes that were never made durable.
    fn load(
        dir: &Path,
        stem: &str,
        size: u64,
        chunk_size: u32,
        policy: SyncPolicy,
    ) -> Result<Self> {
        let mut l = Self::fresh(dir, stem, size, chunk_size, policy);
        let record: Option<LedgerRecord> = match std::fs::read(&l.json) {
            Ok(bytes) => serde_json::from_slice(&bytes).ok(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let Some(rec) = record.filter(|r| r.v == 1 && r.size == size && r.chunk_size == chunk_size)
        else {
            return Ok(l);
        };
        l.saved = rec.saved;
        let mut p = Progress::fresh();
        p.state = rec.state;
        p.root = rec
            .root
            .as_deref()
            .and_then(|h| super::hash_from_hex(h).ok());
        if matches!(rec.state, FileState::Verified | FileState::Failed) {
            p.verified_offset = if rec.state == FileState::Verified {
                size
            } else {
                0
            };
            *lock(&l.progress) = p;
            return Ok(l);
        }
        let cs = u64::from(chunk_size);
        let mut verified = rec.verified_offset.min(size);
        let mut count = rec.hash_count;
        if count != chunk_count(verified, chunk_size)
            || (!verified.is_multiple_of(cs) && verified != size)
        {
            verified = 0;
            count = 0;
        }
        let file = fsx::open_or_create(&l.hashes_path)?;
        let on_disk = file.metadata()?.len() / HASH_LEN as u64;
        if on_disk < count {
            // Should not happen (hashes are synced before the record); fall back safely.
            count = on_disk;
            verified = count * cs;
        }
        let n = usize::try_from(count).map_err(|_| Error::TooLarge("hash count".into()))?;
        let mut raw = vec![0u8; n * HASH_LEN];
        (&file).read_exact(&mut raw)?;
        file.set_len(count * HASH_LEN as u64)?;
        p.hashes = raw.as_chunks::<HASH_LEN>().0.to_vec();
        p.verified_offset = verified;
        p.state = if verified > 0 {
            FileState::Receiving
        } else {
            FileState::Waiting
        };
        l.persisted = n;
        l.hashes_file = Some(file);
        *lock(&l.progress) = p;
        Ok(l)
    }

    fn verified(&self) -> u64 {
        lock(&self.progress).verified_offset
    }

    fn push(&mut self, hash: ChunkHash, len: u64) {
        let mut p = lock(&self.progress);
        p.hashes.push(hash);
        p.verified_offset += len;
        p.state = FileState::Receiving;
        p.retry = None;
        drop(p);
        self.unsynced += len;
    }

    fn due(&self) -> bool {
        self.unsynced > 0
            && (self.unsynced >= self.policy.bytes
                || self.last_sync.elapsed() >= self.policy.interval)
    }

    /// Makes the in-memory verified offset durable. The caller has already synced the data.
    fn persist(&mut self) -> Result<()> {
        let (state, verified, new_hashes, count, root) = {
            let p = lock(&self.progress);
            (
                p.state,
                p.verified_offset,
                p.hashes[self.persisted.min(p.hashes.len())..].concat(),
                p.hashes.len(),
                p.root,
            )
        };
        if !new_hashes.is_empty() {
            if self.hashes_file.is_none() {
                self.hashes_file = Some(fsx::open_or_create(&self.hashes_path)?);
            }
            let f = self.hashes_file.as_mut().expect("opened above");
            f.seek(SeekFrom::Start(self.persisted as u64 * HASH_LEN as u64))?;
            f.write_all(&new_hashes).map_err(fsx::map_io)?;
            f.sync_data().map_err(fsx::map_io)?;
        }
        self.write_record(state, verified, count as u64, root)?;
        self.persisted = count;
        self.unsynced = 0;
        self.last_sync = Instant::now();
        Ok(())
    }

    fn write_record(
        &self,
        state: FileState,
        verified: u64,
        count: u64,
        root: Option<ChunkHash>,
    ) -> Result<()> {
        let rec = LedgerRecord {
            v: 1,
            size: self.size,
            chunk_size: self.chunk_size,
            verified_offset: verified,
            hash_count: count,
            state,
            root: root.as_ref().map(hash_to_hex),
            saved: self.saved.clone(),
        };
        fsx::write_atomic(&self.json, &serde_json::to_vec(&rec)?)
    }

    /// Back to offset 0 (restart, protocol.md 7.6).
    fn reset(&mut self) -> Result<()> {
        *lock(&self.progress) = Progress::fresh();
        if let Some(f) = &self.hashes_file {
            f.set_len(0)?;
        }
        self.persisted = 0;
        self.unsynced = 0;
        self.write_record(FileState::Waiting, 0, 0, None)
    }

    /// Records a terminal state and drops the hash file.
    ///
    /// The record is written before the in-memory state changes: another stream that sees
    /// this one as terminal may complete the transfer and clear the staging folder, so all
    /// of this stream's disk work must already be done.
    fn finish(&mut self, state: FileState, root: Option<ChunkHash>) -> Result<()> {
        let verified = if state == FileState::Verified {
            self.size
        } else {
            0
        };
        self.write_record(state, verified, 0, root)?;
        self.hashes_file = None;
        fsx::remove_if_exists(&self.hashes_path)?;
        let mut p = lock(&self.progress);
        p.state = state;
        p.root = root;
        if state == FileState::Failed {
            p.verified_offset = 0;
            p.hashes.clear();
        }
        Ok(())
    }

    fn root_matches(&self, claimed: &ChunkHash) -> bool {
        let p = lock(&self.progress);
        p.verified_offset == self.size
            && blake3::Hash::from(root_hash(self.size, self.chunk_size, &p.hashes))
                == blake3::Hash::from(*claimed)
    }
}

// ---------------------------------------------------------------------------------------
// PartFile: one streamed file
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
struct PartInner {
    fid: u32,
    ledger: Ledger,
    path: PathBuf,
    file: Option<File>,
    preallocated: bool,
}

impl PartInner {
    fn file(&mut self) -> Result<&mut File> {
        if self.file.is_none() {
            self.file = Some(fsx::open_or_create(&self.path)?);
        }
        Ok(self.file.as_mut().expect("opened above"))
    }

    fn write_chunk(&mut self, chunk: &VerifiedChunk) -> Result<()> {
        let verified = self.ledger.verified();
        if chunk.offset != verified {
            return Err(Error::OffsetMismatch { expected: verified });
        }
        let len = chunk.data.len() as u64;
        if chunk.offset + len > self.ledger.size {
            return Err(Error::BadRequest("bytes past the declared size".into()));
        }
        let size = self.ledger.size;
        let preallocated = self.preallocated;
        let f = self.file()?;
        if !preallocated {
            // Sets the final length up front (sparse on most filesystems).
            // TODO(SR-20, protocol.md 10): reserve real blocks with rustix fallocate on Linux
            // so a full disk fails at the first write, not midway.
            if f.metadata()?.len() < size {
                f.set_len(size).map_err(fsx::map_io)?;
            }
        }
        f.seek(SeekFrom::Start(chunk.offset))?;
        f.write_all(&chunk.data).map_err(fsx::map_io)?;
        self.preallocated = true;
        self.ledger.push(chunk.hash, len);
        if self.ledger.due() {
            self.checkpoint()?;
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<()> {
        if self.ledger.unsynced == 0 {
            return Ok(());
        }
        if let Some(f) = &self.file {
            f.sync_data().map_err(fsx::map_io)?;
        }
        self.ledger.persist()
    }

    fn restart(&mut self) -> Result<()> {
        if let Some(f) = &self.file {
            f.set_len(0)?;
        }
        self.preallocated = false;
        self.ledger.reset()
    }

    fn fail(&mut self) -> Result<()> {
        self.file = None;
        fsx::remove_if_exists(&self.path)?;
        self.ledger.finish(FileState::Failed, None)
    }

    fn finalize(
        &mut self,
        claimed: &ChunkHash,
        dest_root: &Path,
        path: &[String],
    ) -> Result<PathBuf> {
        if self.ledger.verified() != self.ledger.size {
            return Err(Error::BadRequest("file is not fully verified".into()));
        }
        if !self.ledger.root_matches(claimed) {
            self.fail()?;
            return Err(Error::RootMismatch);
        }
        if let Some(f) = &self.file {
            f.sync_data().map_err(fsx::map_io)?;
        } else if self.ledger.size == 0 {
            self.file()?;
        }
        self.file = None;
        let rel = place(&self.path, dest_root, path)?;
        self.ledger.saved = vec![SavedFile {
            fid: self.fid,
            path: rel.clone(),
        }];
        self.ledger.finish(FileState::Verified, Some(*claimed))?;
        Ok(dest_root.join(rel))
    }
}

/// Moves a staged file to its final name beneath `dest_root` without overwriting anything
/// and without following symlinks (SR-18): the folders are walked through directory
/// handles, the final name is created exclusively (with " (n)" on collision) and the staged
/// file is renamed onto that placeholder. Returns the final path relative to `dest_root`.
fn place(staged: &Path, dest_root: &Path, path: &[String]) -> Result<PathBuf> {
    let (staging, staged_name) = staged_parts(staged)?;
    let staging = fsx::open_own_dir(staging)?;
    let root = fsx::open_root(dest_root)?;
    let (name, dirs) = split_path(path)?;
    let dir = fsx::safe_dir(&root, dirs, false)?;
    let final_name = fsx::place_into(&staging, staged_name, &dir, name)?;
    fsx::sync_dir_handle(&dir)?;
    Ok(relative_path(dirs, &final_name))
}

/// Staged file name of a bundle member (or streamed file) in the staging folder.
fn member_name(fid: u32) -> String {
    format!("{fid}.part")
}

/// A staged file's folder and file name.
fn staged_parts(staged: &Path) -> Result<(&Path, &str)> {
    match (staged.parent(), staged.file_name().and_then(|n| n.to_str())) {
        (Some(dir), Some(name)) => Ok((dir, name)),
        _ => Err(Error::PathInvalid(format!(
            "bad staged path {}",
            staged.display()
        ))),
    }
}

/// Splits a manifest path into its file name and its folders.
fn split_path(path: &[String]) -> Result<(&str, &[String])> {
    path.split_last()
        .map(|(n, d)| (n.as_str(), d))
        .ok_or_else(|| Error::PathInvalid("empty path".into()))
}

/// `dirs/.../name` as a relative path.
fn relative_path(dirs: &[String], name: &str) -> PathBuf {
    let mut p: PathBuf = dirs.iter().collect();
    p.push(name);
    p
}

/// Receive state of one streamed file (protocol.md 7.4, 7.6; SR-18, SR-21, SR-22, SR-23).
///
/// Bytes go to `<fid>.part` in the transfer's staging folder, preallocated on first write,
/// written strictly in order. The verified offset and chunk hashes become durable only after
/// `fdatasync`, batched by [`SyncPolicy`]. Reopening truncates the `.part` to the durable
/// offset, so no unverified or unsynced byte is ever trusted.
#[derive(Debug)]
pub struct PartFile {
    inner: Option<Box<PartInner>>,
    progress: SharedProgress,
}

impl PartFile {
    fn fresh(staging: &Path, fid: u32, size: u64, chunk_size: u32, policy: SyncPolicy) -> Self {
        let ledger = Ledger::fresh(staging, &fid.to_string(), size, chunk_size, policy);
        Self::from_ledger(fid, ledger, staging.join(format!("{fid}.part")))
    }

    fn from_ledger(fid: u32, ledger: Ledger, path: PathBuf) -> Self {
        let progress = ledger.progress.clone();
        Self {
            inner: Some(Box::new(PartInner {
                fid,
                ledger,
                path,
                file: None,
                preallocated: false,
            })),
            progress,
        }
    }

    fn open_blocking(
        staging: &Path,
        fid: u32,
        size: u64,
        chunk_size: u32,
        policy: SyncPolicy,
    ) -> Result<Self> {
        let ledger = Ledger::load(staging, &fid.to_string(), size, chunk_size, policy)?;
        let path = staging.join(format!("{fid}.part"));
        let state = lock(&ledger.progress).state;
        let verified = ledger.verified();
        let mut part = Self::from_ledger(fid, ledger, path.clone());
        if matches!(state, FileState::Verified | FileState::Failed) {
            return Ok(part);
        }
        let inner = part.inner.as_mut().expect("just built");
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let f = fsx::open_existing_regular(&path)?;
                if f.metadata()?.len() < verified {
                    // Data shorter than the durable offset: trust nothing.
                    f.set_len(0)?;
                    inner.ledger.reset()?;
                } else {
                    f.set_len(verified)?;
                }
                inner.file = Some(f);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                if verified > 0 {
                    inner.ledger.reset()?;
                }
            }
            Err(e) => return Err(e.into()),
        }
        Ok(part)
    }

    /// Opens (or starts) the staged state of file `fid` in the `staging` folder, truncating
    /// any `.part` to its durable verified offset (protocol.md 7.6).
    pub async fn open(
        staging: &Path,
        fid: u32,
        size: u64,
        chunk_size: u32,
        policy: SyncPolicy,
    ) -> Result<Self> {
        let staging = staging.to_path_buf();
        tokio::task::spawn_blocking(move || {
            Self::open_blocking(&staging, fid, size, chunk_size, policy)
        })
        .await
        .map_err(|e| Error::Io(io::Error::other(e)))?
    }

    async fn run<T, F>(&mut self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut PartInner) -> Result<T> + Send + 'static,
    {
        let mut inner = self
            .inner
            .take()
            .ok_or_else(|| Error::Io(io::Error::other("part file lost after a panic")))?;
        let (inner, out) = tokio::task::spawn_blocking(move || {
            let out = f(&mut inner);
            (inner, out)
        })
        .await
        .map_err(|e| Error::Io(io::Error::other(e)))?;
        self.inner = Some(inner);
        out
    }

    /// Verified, chunk-aligned offset (or the size once complete).
    pub fn verified_offset(&self) -> u64 {
        lock(&self.progress).verified_offset
    }

    /// Current state.
    pub fn state(&self) -> FileState {
        lock(&self.progress).state
    }

    /// Prefix hash of the verified bytes (protocol.md 7.4).
    pub fn prefix(&self, chunk_size: u32) -> ChunkHash {
        let p = lock(&self.progress);
        prefix_hash(p.verified_offset, chunk_size, &p.hashes)
    }

    /// Writes one verified chunk at the verified offset and advances it; checkpoints when
    /// the [`SyncPolicy`] says so.
    pub async fn write_chunk(&mut self, chunk: VerifiedChunk) -> Result<()> {
        self.run(move |p| p.write_chunk(&chunk)).await
    }

    /// Syncs data, then makes the verified offset durable.
    pub async fn checkpoint(&mut self) -> Result<()> {
        self.run(PartInner::checkpoint).await
    }

    /// Discards everything and starts again at offset 0 (protocol.md 7.6).
    pub async fn restart(&mut self) -> Result<()> {
        self.run(PartInner::restart).await
    }

    /// Marks the file failed and deletes its `.part` (SR-22).
    pub async fn fail(&mut self) -> Result<()> {
        self.run(PartInner::fail).await
    }

    /// Verifies the root and moves the file to `dest_root/path...`, never overwriting and
    /// never following symlinks (SR-18). On a root mismatch the `.part` is deleted, the
    /// file is `failed` and [`Error::RootMismatch`] is returned (SR-22).
    pub async fn finalize(
        &mut self,
        root: ChunkHash,
        dest_root: &Path,
        path: &[String],
    ) -> Result<PathBuf> {
        let dest_root = dest_root.to_path_buf();
        let path = path.to_vec();
        self.run(move |p| p.finalize(&root, &dest_root, &path))
            .await
    }
}

// ---------------------------------------------------------------------------------------
// Bundle sink
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
struct BundleInner {
    ledger: Ledger,
    layout: Arc<BundleLayout>,
    staging: PathBuf,
    skipped: Vec<bool>,
    reader: Option<BundleReader>,
    current: Option<(u32, File)>,
    dirty: Vec<u32>,
}

impl BundleInner {
    fn member_path(&self, fid: u32) -> PathBuf {
        self.staging.join(member_name(fid))
    }

    fn is_skipped(&self, fid: u32) -> bool {
        self.skipped.get(fid as usize).copied().unwrap_or(false)
    }

    /// Brings member `.part` files in line with the durable bundle offset.
    fn reconcile(&mut self) -> Result<()> {
        let verified = self.ledger.verified();
        let mut consistent = true;
        for m in self.layout.members() {
            if self.is_skipped(m.fid) {
                continue;
            }
            let path = self.member_path(m.fid);
            if verified >= m.data_offset() {
                let want = m.bytes_below(verified);
                match std::fs::symlink_metadata(&path) {
                    Ok(meta) if meta.len() >= want => {
                        fsx::open_existing_regular(&path)?.set_len(want)?;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound && want == 0 => {
                        fsx::open_or_create(&path)?;
                    }
                    _ => consistent = false,
                }
            } else {
                fsx::remove_if_exists(&path)?;
            }
        }
        if !consistent {
            self.remove_members()?;
            self.ledger.reset()?;
        }
        Ok(())
    }

    fn remove_members(&mut self) -> Result<()> {
        self.current = None;
        self.dirty.clear();
        for m in self.layout.members() {
            fsx::remove_if_exists(&self.member_path(m.fid))?;
        }
        Ok(())
    }

    fn begin(&mut self, start: u64) -> Result<()> {
        if start == 0 && self.ledger.verified() > 0 {
            self.remove_members()?;
            self.ledger.reset()?;
        }
        self.reader = Some(BundleReader::new(self.layout.clone(), start)?);
        Ok(())
    }

    fn close_current(&mut self) {
        if let Some((fid, f)) = self.current.take() {
            drop(f);
            self.dirty.push(fid);
        }
    }

    fn write_chunk(&mut self, chunk: &VerifiedChunk) -> Result<()> {
        let verified = self.ledger.verified();
        if chunk.offset != verified {
            return Err(Error::OffsetMismatch { expected: verified });
        }
        let reader = self
            .reader
            .as_mut()
            .ok_or_else(|| Error::BadRequest("bundle upload not started".into()))?;
        for ev in reader.feed(chunk.data.clone())? {
            match ev {
                BundleEvent::MemberStart { fid } => {
                    self.close_current();
                    if !self.is_skipped(fid) {
                        let f = fsx::open_or_create(&self.member_path(fid))?;
                        f.set_len(0)?;
                        self.current = Some((fid, f));
                    }
                }
                BundleEvent::Data { fid, offset, bytes } => {
                    if self.is_skipped(fid) {
                        continue;
                    }
                    if self.current.as_ref().map(|c| c.0) != Some(fid) {
                        self.close_current();
                        let f = fsx::open_existing_regular(&self.member_path(fid))?;
                        self.current = Some((fid, f));
                    }
                    let f = &mut self.current.as_mut().expect("set above").1;
                    f.seek(SeekFrom::Start(offset))?;
                    f.write_all(&bytes).map_err(fsx::map_io)?;
                }
                BundleEvent::MemberEnd { .. } => self.close_current(),
            }
        }
        self.ledger.push(chunk.hash, chunk.data.len() as u64);
        if self.ledger.due() {
            self.checkpoint()?;
        }
        Ok(())
    }

    fn checkpoint(&mut self) -> Result<()> {
        if self.ledger.unsynced == 0 {
            return Ok(());
        }
        if let Some((_, f)) = &self.current {
            f.sync_data().map_err(fsx::map_io)?;
        }
        // Closed members written since the last checkpoint, flushed together.
        let dirty: Vec<PathBuf> = std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|fid| self.member_path(fid))
            .collect();
        fsx::sync_files(&self.staging, &dirty)?;
        self.ledger.persist()
    }

    fn fail(&mut self) -> Result<()> {
        self.remove_members()?;
        self.ledger.finish(FileState::Failed, None)
    }

    fn finalize(
        &mut self,
        claimed: &ChunkHash,
        dest_root: &Path,
        manifest: &Manifest,
    ) -> Result<Vec<SavedFile>> {
        if self.ledger.verified() != self.ledger.size {
            return Err(Error::BadRequest("bundle is not fully verified".into()));
        }
        if !self.ledger.root_matches(claimed) {
            self.fail()?;
            return Err(Error::RootMismatch);
        }
        self.close_current();
        let dirty: Vec<PathBuf> = std::mem::take(&mut self.dirty)
            .into_iter()
            .map(|fid| self.member_path(fid))
            .collect();
        fsx::sync_files(&self.staging, &dirty)?;
        // One rename per member, then one sync per destination folder instead of one per
        // file: thousands of small files must not cost thousands of journal commits. Each
        // folder is walked once (SR-18) and its handle reused for every member in it.
        let staging = fsx::open_own_dir(&self.staging)?;
        let root = fsx::open_root(dest_root)?;
        let mut dirs: HashMap<&[String], cap_std::fs::Dir> = HashMap::new();
        let mut saved = Vec::with_capacity(self.layout.members().len());
        for m in self.layout.members() {
            if self.is_skipped(m.fid) {
                continue;
            }
            let (name, parents) = split_path(&manifest.files[m.fid as usize].path)?;
            let dir = match dirs.entry(parents) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(fsx::safe_dir(&root, parents, false)?)
                }
            };
            let final_name = fsx::place_into(&staging, &member_name(m.fid), dir, name)?;
            saved.push(SavedFile {
                fid: m.fid,
                path: relative_path(parents, &final_name),
            });
        }
        for dir in dirs.values() {
            fsx::sync_dir_handle(dir)?;
        }
        self.ledger.saved.clone_from(&saved);
        self.ledger.finish(FileState::Verified, Some(*claimed))?;
        Ok(saved)
    }
}

// ---------------------------------------------------------------------------------------
// TransferReceiver
// ---------------------------------------------------------------------------------------

#[derive(Debug)]
enum SlotIo {
    File(PartFile),
    Bundle(Box<BundleInner>),
    /// Skipped or zero-length: nothing to stream.
    None,
}

#[derive(Debug)]
struct Slot {
    progress: SharedProgress,
    io: Arc<AsyncMutex<SlotIo>>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferRecord {
    v: u32,
    manifest: Manifest,
    chunk_size: u32,
    tokens: Vec<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bundle_token: Option<String>,
    skip: Vec<u32>,
    state: TransferState,
}

struct Shared {
    manifest: Manifest,
    root: PathBuf,
    staging: PathBuf,
    chunk_size: u32,
    layout: Option<Arc<BundleLayout>>,
    free_space: Arc<dyn FreeSpace>,
    tokens: Vec<Option<String>>,
    bundle_token: Option<String>,
    skip: Vec<u32>,
    skipped: Vec<bool>,
    files: Vec<Slot>,
    bundle: Option<Slot>,
    status: StdMutex<(TransferState, SystemTime)>,
    /// Final destination of each fid relative to the receive root, once placed.
    saved: StdMutex<Vec<Option<PathBuf>>>,
    /// Serialises [`Shared::settle_blocking`]: concurrent `PUT`s that finish together
    /// must not both complete the transfer.
    settle_gate: StdMutex<()>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferReceiver")
            .field("id", &self.manifest.id)
            .field("state", &lock(&self.status).0)
            .finish_non_exhaustive()
    }
}

/// Receive side of one transfer (protocol.md 7.2 to 7.7).
///
/// Cheap to clone; clones share state, so many file `PUT`s can run in parallel. Each file
/// (and the bundle) is guarded by its own async lock, so a retried `PUT` for the same file
/// waits for the previous one to finish before its offset is checked.
#[derive(Debug, Clone)]
pub struct TransferReceiver {
    shared: Arc<Shared>,
}

fn new_token() -> String {
    use base64::Engine as _;
    use rand::Rng as _;
    let mut b = [0u8; 16];
    rand::rng().fill_bytes(&mut b);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
}

fn token_matches(expected: Option<&String>, given: &str) -> bool {
    // Compare hashes so the comparison time does not depend on where they differ.
    expected.is_some_and(|e| blake3::hash(e.as_bytes()) == blake3::hash(given.as_bytes()))
}

fn join_err(e: tokio::task::JoinError) -> Error {
    Error::Io(io::Error::other(e))
}

impl TransferReceiver {
    /// Accepts a manifest (protocol.md 7.2, 7.3): validates it (SR-17, SR-20), checks free
    /// space minus the reserve (SR-20), creates the 0700 receive root and staging folder
    /// (SR-23), issues tokens (SR-13) and persists the transfer record.
    pub async fn accept(
        root: &Path,
        manifest: Manifest,
        free_space: Arc<dyn FreeSpace>,
        opts: AcceptOptions,
    ) -> Result<Self> {
        manifest.validate()?;
        let chunk_size = opts.chunk_size.unwrap_or(manifest.chunk_size);
        if !is_valid_chunk_size(chunk_size) {
            return Err(Error::BadRequest(format!("chunk size {chunk_size}")));
        }
        let staging_key = STAGING_DIR.to_lowercase();
        let first_component =
            |p: &[String]| p.first().is_some_and(|c| c.to_lowercase() == staging_key);
        if manifest.files.iter().any(|f| first_component(&f.path))
            || manifest.dirs.iter().any(|d| first_component(d))
        {
            return Err(Error::PathInvalid(format!("{STAGING_DIR} is reserved")));
        }
        let mut opts = opts;
        opts.skip.sort_unstable();
        opts.skip.dedup();
        if let Some(&bad) = opts
            .skip
            .iter()
            .find(|&&f| f as usize >= manifest.files.len())
        {
            return Err(Error::BadRequest(format!("skip names unknown fid {bad}")));
        }
        let skipped = skip_mask(&opts.skip, manifest.files.len());
        let needed: u64 = manifest
            .files
            .iter()
            .filter(|f| !skipped[f.fid as usize])
            .map(|f| f.size)
            .sum();
        let root_buf = root.to_path_buf();
        let fs = free_space.clone();
        let space = tokio::task::spawn_blocking(move || -> Result<SpaceInfo> {
            fsx::ensure_root(&root_buf)?;
            Ok(fs.space(&root_buf)?)
        })
        .await
        .map_err(join_err)??;
        if !space.fits(needed) {
            return Err(Error::NoSpace);
        }

        let tokens: Vec<Option<String>> = manifest
            .files
            .iter()
            .map(|f| (!f.in_bundle && f.size > 0 && !skipped[f.fid as usize]).then(new_token))
            .collect();
        let bundle_token = manifest.bundle.is_some().then(new_token);
        let record = TransferRecord {
            v: 1,
            manifest,
            chunk_size,
            tokens,
            bundle_token,
            skip: opts.skip,
            state: TransferState::Accepted,
        };
        let root = root.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let root_dir = fsx::open_root(&root)?;
            let parent = fsx::safe_dir(&root_dir, &[STAGING_DIR.to_owned()], true)?;
            let staging = root.join(STAGING_DIR).join(&record.manifest.id);
            match parent.create_dir(&record.manifest.id) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    return Err(Error::BadRequest(
                        "transfer id already known; use load".into(),
                    ))
                }
                Err(e) => return Err(e.into()),
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
            }
            fsx::write_atomic(
                &staging.join("transfer.json"),
                &serde_json::to_vec(&record)?,
            )?;
            Self::build(root, staging, record, free_space, opts.sync, false)
        })
        .await
        .map_err(join_err)?
    }

    /// Reopens a transfer after a restart (protocol.md 7.6): reloads the record and every
    /// file's durable state, truncating `.part` files to their verified offsets.
    pub async fn load(
        root: &Path,
        id: &str,
        free_space: Arc<dyn FreeSpace>,
        policy: SyncPolicy,
    ) -> Result<Self> {
        if !is_valid_transfer_id(id) {
            return Err(Error::NotFound);
        }
        let root = root.to_path_buf();
        let staging = root.join(STAGING_DIR).join(id);
        tokio::task::spawn_blocking(move || {
            let bytes = match std::fs::read(staging.join("transfer.json")) {
                Ok(b) => b,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(Error::NotFound),
                Err(e) => return Err(e.into()),
            };
            let record: TransferRecord = serde_json::from_slice(&bytes)?;
            record.manifest.validate()?;
            Self::build(root, staging, record, free_space, policy, true)
        })
        .await
        .map_err(join_err)?
    }

    fn build(
        root: PathBuf,
        staging: PathBuf,
        rec: TransferRecord,
        free_space: Arc<dyn FreeSpace>,
        policy: SyncPolicy,
        from_disk: bool,
    ) -> Result<Self> {
        let m = &rec.manifest;
        let cs = rec.chunk_size;
        let mask = skip_mask(&rec.skip, m.files.len());
        let mut files = Vec::with_capacity(m.files.len());
        for f in &m.files {
            let skipped = mask[f.fid as usize];
            let (io, progress) = if skipped || f.in_bundle || f.size == 0 {
                let mut p = Progress::fresh();
                if skipped {
                    p.state = FileState::Skipped;
                } else if f.size == 0 && !f.in_bundle {
                    // Never streamed; created on completion (protocol.md 7.2).
                    p.state = FileState::Verified;
                    p.root = Some(root_hash(0, cs, &[]));
                }
                (SlotIo::None, Arc::new(StdMutex::new(p)))
            } else {
                let part = if from_disk {
                    PartFile::open_blocking(&staging, f.fid, f.size, cs, policy)?
                } else {
                    PartFile::fresh(&staging, f.fid, f.size, cs, policy)
                };
                let progress = part.progress.clone();
                (SlotIo::File(part), progress)
            };
            files.push(Slot {
                progress,
                io: Arc::new(AsyncMutex::new(io)),
            });
        }
        let layout = match &m.bundle {
            Some(_) => Some(Arc::new(BundleLayout::new(m)?)),
            None => None,
        };
        let bundle = match (&layout, &m.bundle) {
            (Some(layout), Some(info)) => {
                let ledger = if from_disk {
                    Ledger::load(&staging, "bundle", info.size, cs, policy)?
                } else {
                    Ledger::fresh(&staging, "bundle", info.size, cs, policy)
                };
                let progress = ledger.progress.clone();
                let mut inner = BundleInner {
                    ledger,
                    layout: layout.clone(),
                    staging: staging.clone(),
                    skipped: mask.clone(),
                    reader: None,
                    current: None,
                    dirty: Vec::new(),
                };
                if from_disk
                    && !matches!(
                        lock(&progress).state,
                        FileState::Verified | FileState::Failed
                    )
                {
                    inner.reconcile()?;
                }
                Some(Slot {
                    progress,
                    io: Arc::new(AsyncMutex::new(SlotIo::Bundle(Box::new(inner)))),
                })
            }
            _ => None,
        };
        let saved = saved_from_ledgers(&files, bundle.as_ref());
        let receiver = Self {
            shared: Arc::new(Shared {
                manifest: rec.manifest,
                root,
                staging,
                chunk_size: cs,
                layout,
                free_space,
                tokens: rec.tokens,
                bundle_token: rec.bundle_token,
                skip: rec.skip,
                skipped: mask,
                files,
                bundle,
                status: StdMutex::new((rec.state, SystemTime::now())),
                saved: StdMutex::new(saved),
                settle_gate: StdMutex::new(()),
            }),
        };
        if matches!(rec.state, TransferState::Accepted | TransferState::Active) {
            receiver.shared.settle_blocking()?;
        }
        Ok(receiver)
    }

    /// Transfer ID.
    pub fn id(&self) -> &str {
        &self.shared.manifest.id
    }

    /// The accepted manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.shared.manifest
    }

    /// Binding chunk size.
    pub fn chunk_size(&self) -> u32 {
        self.shared.chunk_size
    }

    /// Current transfer state.
    pub fn transfer_state(&self) -> TransferState {
        lock(&self.shared.status).0
    }

    /// Time of the last accepted byte or state change, for idle cleanup (SR-20).
    pub fn last_activity(&self) -> SystemTime {
        lock(&self.shared.status).1
    }

    /// Path of this transfer's staging folder.
    pub fn staging_dir(&self) -> &Path {
        &self.shared.staging
    }

    /// The receive root every file of this transfer is placed beneath.
    pub fn receive_root(&self) -> &Path {
        &self.shared.root
    }

    /// Final paths of the files placed so far, in manifest order, after any " (n)"
    /// collision rename (SR-18). Skipped files and files not yet verified are absent, so
    /// once the transfer is `complete` this lists every received file.
    pub fn saved_paths(&self) -> Vec<PathBuf> {
        lock(&self.shared.saved)
            .iter()
            .flatten()
            .map(|rel| self.shared.root.join(rel))
            .collect()
    }

    /// The `GET /v1/transfers/{id}` body (protocol.md 7.5).
    pub fn state(&self) -> TransferStateView {
        let s = &self.shared;
        let cs = s.chunk_size;
        let bundle_progress = s.bundle.as_ref().map(|b| lock(&b.progress).clone());
        let bundle_offset = bundle_progress.as_ref().map_or(0, |p| p.verified_offset);
        let mut bytes_verified = 0u64;
        let mut files = Vec::with_capacity(s.files.len());
        for (f, slot) in s.manifest.files.iter().zip(&s.files) {
            let view = if f.in_bundle && !s.is_skipped(f.fid) {
                let bp = bundle_progress
                    .as_ref()
                    .expect("bundle exists when files are inBundle");
                let member = s.layout.as_ref().and_then(|l| l.member(f.fid).copied());
                let done = member.map_or(0, |m| m.bytes_below(bundle_offset));
                let state = match bp.state {
                    FileState::Verified => FileState::Verified,
                    FileState::Failed => FileState::Failed,
                    _ if done > 0 => FileState::Receiving,
                    _ => FileState::Waiting,
                };
                FileStateView {
                    fid: f.fid,
                    state,
                    size: f.size,
                    verified_offset: done,
                    root: None,
                    prefix: None,
                    token: None,
                }
            } else {
                let p = lock(&slot.progress);
                let prefix = (p.verified_offset > 0 && p.verified_offset < f.size)
                    .then(|| hash_to_hex(&prefix_hash(p.verified_offset, cs, &p.hashes)));
                FileStateView {
                    fid: f.fid,
                    state: p.state,
                    size: f.size,
                    verified_offset: p.verified_offset,
                    root: (p.state == FileState::Verified)
                        .then(|| p.root.as_ref().map(hash_to_hex))
                        .flatten(),
                    prefix,
                    token: s.tokens.get(f.fid as usize).cloned().flatten(),
                }
            };
            if !f.in_bundle {
                bytes_verified += view.verified_offset;
            }
            files.push(view);
        }
        if let Some(layout) = &s.layout {
            bytes_verified += layout
                .members()
                .iter()
                .filter(|m| !s.is_skipped(m.fid))
                .map(|m| m.bytes_below(bundle_offset))
                .sum::<u64>();
        }
        let bundle = bundle_progress.map(|p| StreamStateView {
            state: p.state,
            size: s.layout.as_ref().map_or(0, |l| l.total()),
            verified_offset: p.verified_offset,
            root: (p.state == FileState::Verified)
                .then(|| p.root.as_ref().map(hash_to_hex))
                .flatten(),
            prefix: (p.verified_offset > 0
                && Some(p.verified_offset) != s.layout.as_ref().map(|l| l.total()))
            .then(|| hash_to_hex(&prefix_hash(p.verified_offset, cs, &p.hashes))),
            token: s.bundle_token.clone(),
        });
        TransferStateView {
            id: s.manifest.id.clone(),
            state: lock(&s.status).0,
            chunk_size: cs,
            total_bytes: s.manifest.total_bytes,
            bytes_verified,
            files,
            bundle,
        }
    }

    fn check_open(&self) -> std::result::Result<(), PutError> {
        match lock(&self.shared.status).0 {
            TransferState::Cancelled => Err(PutError::Cancelled),
            TransferState::Accepted | TransferState::Active => Ok(()),
            other => Err(PutError::Core(Error::BadRequest(format!(
                "transfer is {other:?}"
            )))),
        }
    }

    /// Starts a file `PUT` (protocol.md 7.4): checks the token (SR-13), the range, and that
    /// `start` is the verified offset or 0 (SR-21), else [`Error::OffsetMismatch`]. A start
    /// of 0 over a non-zero offset discards the `.part` (restart, 7.6).
    pub async fn begin_file(
        &self,
        fid: u32,
        token: &str,
        start: u64,
        end_inclusive: u64,
    ) -> std::result::Result<Upload, PutError> {
        self.check_open()?;
        let s = &self.shared;
        let entry = s.manifest.files.get(fid as usize).ok_or(Error::NotFound)?;
        if !token_matches(s.tokens.get(fid as usize).and_then(Option::as_ref), token) {
            return Err(PutError::BadToken);
        }
        check_range(start, end_inclusive, entry.size, s.chunk_size)?;
        let slot = &s.files[fid as usize];
        let mut guard = slot.io.clone().lock_owned().await;
        self.check_open()?;
        let SlotIo::File(part) = &mut *guard else {
            return Err(PutError::BadToken);
        };
        match part.state() {
            FileState::Verified => {
                return Err(Error::BadRequest("file already verified".into()).into())
            }
            FileState::Failed => return Err(Error::BadRequest("file failed".into()).into()),
            _ => {}
        }
        let verified = part.verified_offset();
        if start != verified && start != 0 {
            return Err(Error::OffsetMismatch { expected: verified }.into());
        }
        if start == 0 && verified > 0 {
            part.restart().await?;
        }
        self.mark_active();
        Ok(Upload {
            receiver: self.clone(),
            target: Target::File(fid),
            guard,
            start,
            end_inclusive,
        })
    }

    /// Starts the bundle `PUT` (protocol.md 7.7), with the same token and offset rules as a
    /// file.
    pub async fn begin_bundle(
        &self,
        token: &str,
        start: u64,
        end_inclusive: u64,
    ) -> std::result::Result<Upload, PutError> {
        self.check_open()?;
        let s = &self.shared;
        let (Some(slot), Some(layout)) = (&s.bundle, &s.layout) else {
            return Err(Error::NotFound.into());
        };
        if !token_matches(s.bundle_token.as_ref(), token) {
            return Err(PutError::BadToken);
        }
        check_range(start, end_inclusive, layout.total(), s.chunk_size)?;
        let mut guard = slot.io.clone().lock_owned().await;
        self.check_open()?;
        let SlotIo::Bundle(inner) = &mut *guard else {
            return Err(Error::NotFound.into());
        };
        match lock(&slot.progress).state {
            FileState::Verified => {
                return Err(Error::BadRequest("bundle already verified".into()).into())
            }
            FileState::Failed => return Err(Error::BadRequest("bundle failed".into()).into()),
            _ => {}
        }
        let verified = inner.ledger.verified();
        if start != verified && start != 0 {
            return Err(Error::OffsetMismatch { expected: verified }.into());
        }
        let mut owned = std::mem::replace(inner, Box::new(placeholder_bundle()));
        let (back, res) = tokio::task::spawn_blocking(move || {
            let r = owned.begin(start);
            (owned, r)
        })
        .await
        .map_err(join_err)?;
        *inner = back;
        res?;
        self.mark_active();
        Ok(Upload {
            receiver: self.clone(),
            target: Target::Bundle,
            guard,
            start,
            end_inclusive,
        })
    }

    fn mark_active(&self) {
        let mut st = lock(&self.shared.status);
        if st.0 == TransferState::Accepted {
            st.0 = TransferState::Active;
        }
        st.1 = SystemTime::now();
    }

    /// Cancels the transfer and deletes every `.part` (protocol.md 7.5). Later `PUT`s get
    /// [`PutError::Cancelled`].
    pub async fn cancel(&self) -> Result<()> {
        {
            let mut st = lock(&self.shared.status);
            if st.0 == TransferState::Complete {
                return Ok(());
            }
            st.0 = TransferState::Cancelled;
            st.1 = SystemTime::now();
        }
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || {
            remove_staging_contents(&shared.staging)?;
            shared.write_record(TransferState::Cancelled)
        })
        .await
        .map_err(join_err)?
    }
}

fn placeholder_bundle() -> BundleInner {
    let layout = Arc::new(
        BundleLayout::new(&Manifest {
            id: String::new(),
            chunk_size: crate::constants::DEFAULT_CHUNK_SIZE,
            total_bytes: 0,
            note: None,
            files: Vec::new(),
            dirs: Vec::new(),
            bundle: None,
        })
        .expect("empty layout"),
    );
    BundleInner {
        ledger: Ledger::fresh(Path::new(""), "", 0, 1, SyncPolicy::default()),
        layout,
        staging: PathBuf::new(),
        skipped: Vec::new(),
        reader: None,
        current: None,
        dirty: Vec::new(),
    }
}

/// Destinations recorded in the ledgers of a freshly built (or reloaded) transfer, by fid.
fn saved_from_ledgers(files: &[Slot], bundle: Option<&Slot>) -> Vec<Option<PathBuf>> {
    let mut saved: Vec<Option<PathBuf>> = vec![None; files.len()];
    let mut restore = |list: &[SavedFile]| {
        for f in list {
            if let Some(slot) = saved.get_mut(f.fid as usize) {
                *slot = Some(f.path.clone());
            }
        }
    };
    // Nothing else holds these locks yet: the receiver is still being built.
    for slot in files.iter().chain(bundle) {
        if let Ok(io) = slot.io.try_lock() {
            match &*io {
                SlotIo::File(PartFile {
                    inner: Some(part), ..
                }) => restore(&part.ledger.saved),
                SlotIo::Bundle(b) => restore(&b.ledger.saved),
                _ => {}
            }
        }
    }
    saved
}

/// Per-fid skip flags from a list of skipped fids (unknown fids are ignored).
fn skip_mask(skip: &[u32], files: usize) -> Vec<bool> {
    let mut mask = vec![false; files];
    for &fid in skip {
        if let Some(m) = mask.get_mut(fid as usize) {
            *m = true;
        }
    }
    mask
}

/// Deletes everything in a staging folder except `transfer.json`.
fn remove_staging_contents(staging: &Path) -> Result<()> {
    let rd = match std::fs::read_dir(staging) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in rd {
        let entry = entry?;
        if entry.file_name() != "transfer.json" {
            fsx::remove_if_exists(&entry.path())?;
        }
    }
    Ok(())
}

impl Shared {
    fn is_skipped(&self, fid: u32) -> bool {
        self.skipped.get(fid as usize).copied().unwrap_or(false)
    }

    fn record_saved(&self, list: &[SavedFile]) {
        let mut saved = lock(&self.saved);
        for f in list {
            if let Some(slot) = saved.get_mut(f.fid as usize) {
                *slot = Some(f.path.clone());
            }
        }
    }

    fn write_record(&self, state: TransferState) -> Result<()> {
        let rec = TransferRecord {
            v: 1,
            manifest: self.manifest.clone(),
            chunk_size: self.chunk_size,
            tokens: self.tokens.clone(),
            bundle_token: self.bundle_token.clone(),
            skip: self.skip.clone(),
            state,
        };
        fsx::write_atomic(
            &self.staging.join("transfer.json"),
            &serde_json::to_vec(&rec)?,
        )
    }

    /// Moves the transfer to `complete` or `failed` once nothing is outstanding
    /// (protocol.md 7.6). Completion creates empty folders and zero-length files.
    fn settle_blocking(&self) -> Result<()> {
        let _gate = lock(&self.settle_gate);
        if matches!(
            lock(&self.status).0,
            TransferState::Complete | TransferState::Failed | TransferState::Cancelled
        ) {
            return Ok(());
        }
        let mut any_failed = false;
        let mut outstanding = false;
        // Bundled files follow the bundle's own state.
        let streams = self
            .files
            .iter()
            .zip(&self.manifest.files)
            .filter(|(_, f)| !f.in_bundle)
            .map(|(s, _)| &s.progress)
            .chain(self.bundle.iter().map(|b| &b.progress));
        for p in streams {
            match lock(p).state {
                FileState::Verified | FileState::Skipped => {}
                FileState::Failed => any_failed = true,
                FileState::Waiting | FileState::Receiving => outstanding = true,
            }
        }
        if outstanding {
            return Ok(());
        }
        let new_state = if any_failed {
            TransferState::Failed
        } else {
            let root = fsx::open_root(&self.root)?;
            for d in &self.manifest.dirs {
                fsx::safe_dir(&root, d, false)?;
            }
            for f in &self.manifest.files {
                if f.size == 0 && !f.in_bundle && !self.is_skipped(f.fid) {
                    let (name, dirs) = split_path(&f.path)?;
                    let dir = fsx::safe_dir(&root, dirs, false)?;
                    let (final_name, _) = fsx::create_unique(&dir, name)?;
                    self.record_saved(&[SavedFile {
                        fid: f.fid,
                        path: relative_path(dirs, &final_name),
                    }]);
                }
            }
            TransferState::Complete
        };
        {
            let mut st = lock(&self.status);
            if matches!(st.0, TransferState::Cancelled) {
                return Ok(());
            }
            st.0 = new_state;
            st.1 = SystemTime::now();
        }
        remove_staging_contents(&self.staging)?;
        self.write_record(new_state)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    File(u32),
    Bundle,
}

/// One in-progress file or bundle `PUT` (protocol.md 7.4, 7.7). Holds that stream's lock.
///
/// Call [`Upload::receive`] with the body, then [`Upload::finish`] with the `pd-root`
/// trailer if the range reached end of file. After an error, [`Upload::response`] gives
/// the state for the error body (`verifiedOffset`).
#[derive(Debug)]
pub struct Upload {
    receiver: TransferReceiver,
    target: Target,
    guard: OwnedMutexGuard<SlotIo>,
    start: u64,
    end_inclusive: u64,
}

impl Upload {
    fn slot(&self) -> &Slot {
        let s = &self.receiver.shared;
        match self.target {
            Target::File(fid) => &s.files[fid as usize],
            Target::Bundle => s.bundle.as_ref().expect("bundle target has a slot"),
        }
    }

    /// Current state of this stream for the reply body.
    pub fn response(&self) -> PutResponse {
        let p = lock(&self.slot().progress);
        PutResponse {
            fid: match self.target {
                Target::File(fid) => Some(fid),
                Target::Bundle => None,
            },
            state: p.state,
            verified_offset: p.verified_offset,
            root: (p.state == FileState::Verified)
                .then(|| p.root.as_ref().map(hash_to_hex))
                .flatten(),
        }
    }

    async fn write(&mut self, chunk: VerifiedChunk) -> Result<()> {
        match &mut *self.guard {
            SlotIo::File(part) => part.write_chunk(chunk).await,
            SlotIo::Bundle(inner) => {
                let mut owned = std::mem::replace(inner, Box::new(placeholder_bundle()));
                let (back, res) = tokio::task::spawn_blocking(move || {
                    let r = owned.write_chunk(&chunk);
                    (owned, r)
                })
                .await
                .map_err(join_err)?;
                *inner = back;
                res
            }
            SlotIo::None => Err(Error::NotFound),
        }
    }

    async fn blocking<T, F>(&mut self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut PartInner) -> Result<T> + Send + 'static,
    {
        match &mut *self.guard {
            SlotIo::File(part) => part.run(f).await,
            _ => Err(Error::NotFound),
        }
    }

    async fn bundle_blocking<T, F>(&mut self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut BundleInner) -> Result<T> + Send + 'static,
    {
        let SlotIo::Bundle(inner) = &mut *self.guard else {
            return Err(Error::NotFound);
        };
        let mut owned = std::mem::replace(inner, Box::new(placeholder_bundle()));
        let (back, res) = tokio::task::spawn_blocking(move || {
            let r = f(&mut owned);
            (owned, r)
        })
        .await
        .map_err(join_err)?;
        *inner = back;
        res
    }

    async fn checkpoint(&mut self) -> Result<()> {
        match self.target {
            Target::File(_) => self.blocking(PartInner::checkpoint).await,
            Target::Bundle => self.bundle_blocking(BundleInner::checkpoint).await,
        }
    }

    async fn fail_stream(&mut self) -> Result<()> {
        match self.target {
            Target::File(_) => self.blocking(PartInner::fail).await,
            Target::Bundle => self.bundle_blocking(BundleInner::fail).await,
        }
    }

    fn check_space(&self, len: u64) -> Result<()> {
        let s = &self.receiver.shared;
        let space = s.free_space.space(&s.root)?;
        if space.fits(len) {
            Ok(())
        } else {
            Err(Error::NoSpace)
        }
    }

    /// Reads chunk records from `body`, verifying each (SR-22) and writing it at the
    /// verified offset. Stops at the first bad chunk: after [`MAX_CHUNK_RETRIES`] re-sends
    /// of the same chunk the file is `failed` and its `.part` deleted. A cut body keeps
    /// whole verified chunks only. Always ends with a durable checkpoint.
    pub async fn receive<R: AsyncRead + Unpin>(
        &mut self,
        body: R,
    ) -> std::result::Result<(), PutError> {
        let cs = self.receiver.shared.chunk_size;
        let mut dec = ChunkDecoder::new(body, self.start, self.end_inclusive, cs);
        let outcome = loop {
            if let Err(e) = self.receiver.check_open() {
                break Err(e);
            }
            match dec.next_chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(e) = self.check_space(chunk.data.len() as u64) {
                        break Err(e.into());
                    }
                    if let Err(e) = self.write(chunk).await {
                        break Err(e.into());
                    }
                    lock(&self.receiver.shared.status).1 = SystemTime::now();
                }
                Ok(None) => break Ok(()),
                Err(Error::ChunkHashMismatch { offset }) => {
                    let exhausted = {
                        let mut p = lock(&self.slot().progress);
                        let count = match p.retry {
                            Some((o, n)) if o == offset => n + 1,
                            _ => 1,
                        };
                        p.retry = Some((offset, count));
                        count > MAX_CHUNK_RETRIES
                    };
                    if exhausted {
                        self.fail_stream().await?;
                        self.settle().await?;
                    }
                    break Err(Error::ChunkHashMismatch { offset }.into());
                }
                Err(Error::BundleMismatch(why)) => {
                    self.fail_stream().await?;
                    self.settle().await?;
                    break Err(Error::BundleMismatch(why).into());
                }
                Err(e) => break Err(e.into()),
            }
        };
        if matches!(outcome, Err(PutError::Core(Error::BundleMismatch(_)))) {
            return outcome;
        }
        let failed = lock(&self.slot().progress).state == FileState::Failed;
        if !failed && !matches!(outcome, Err(PutError::Cancelled)) {
            self.checkpoint().await?;
        }
        outcome
    }

    /// Completes the request. If the stream is fully verified, `root` (the `pd-root`
    /// trailer) is required and checked; a mismatch fails the file at once, deletes its
    /// `.part` and returns [`Error::RootMismatch`] (SR-22). On success the file (or every
    /// bundle member) is moved into place (SR-18).
    pub async fn finish(
        mut self,
        root: Option<ChunkHash>,
    ) -> std::result::Result<PutResponse, PutError> {
        self.receiver.check_open()?;
        let size = match self.target {
            Target::File(fid) => self.receiver.shared.manifest.files[fid as usize].size,
            Target::Bundle => self
                .receiver
                .shared
                .layout
                .as_ref()
                .map_or(0, |l| l.total()),
        };
        let verified = lock(&self.slot().progress).verified_offset;
        if verified < size {
            return Ok(self.response());
        }
        let claimed = root.ok_or_else(|| Error::BadRequest("missing pd-root trailer".into()))?;
        let dest_root = self.receiver.shared.root.clone();
        let result = match self.target {
            Target::File(fid) => {
                let path = self.receiver.shared.manifest.files[fid as usize]
                    .path
                    .clone();
                self.blocking(move |p| {
                    p.finalize(&claimed, &dest_root, &path)?;
                    Ok(p.ledger.saved.clone())
                })
                .await
            }
            Target::Bundle => {
                let shared = self.receiver.shared.clone();
                self.bundle_blocking(move |b| b.finalize(&claimed, &dest_root, &shared.manifest))
                    .await
            }
        };
        if let Ok(saved) = &result {
            self.receiver.shared.record_saved(saved);
        }
        self.settle().await?;
        result?;
        Ok(self.response())
    }

    async fn settle(&self) -> Result<()> {
        let shared = self.receiver.shared.clone();
        tokio::task::spawn_blocking(move || shared.settle_blocking())
            .await
            .map_err(join_err)?
    }
}

/// Deletes staging folders under `root` idle longer than `max_idle` (normally
/// [`IDLE_TIMEOUT`], SR-20) and returns their transfer IDs so the host can mark those
/// transfers `failed`. Idle means no file in the folder was modified in that time.
pub async fn cleanup_idle(root: &Path, max_idle: Duration) -> Result<Vec<String>> {
    let parent = root.join(STAGING_DIR);
    tokio::task::spawn_blocking(move || {
        let mut removed = Vec::new();
        let rd = match std::fs::read_dir(&parent) {
            Ok(rd) => rd,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(removed),
            Err(e) => return Err(e.into()),
        };
        let now = SystemTime::now();
        for entry in rd {
            let entry = entry?;
            let Some(id) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_valid_transfer_id(&id) || !entry.file_type()?.is_dir() {
                continue;
            }
            let mut newest = entry.metadata()?.modified()?;
            for f in std::fs::read_dir(entry.path())? {
                let m = f?.metadata()?.modified()?;
                newest = newest.max(m);
            }
            if now.duration_since(newest).unwrap_or_default() > max_idle {
                std::fs::remove_dir_all(entry.path())?;
                removed.push(id);
            }
        }
        Ok(removed)
    })
    .await
    .map_err(join_err)?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::transfer::bundle_size;
    use crate::transfer::chunk::tests::TestRng;
    use crate::transfer::chunk::ChunkEncoder;
    use crate::transfer::manifest::{BundleInfo, FileEntry};

    /// Test free space: plenty unless told otherwise.
    #[derive(Debug)]
    pub(crate) struct FakeSpace(pub SpaceInfo);

    impl FakeSpace {
        pub(crate) fn plenty() -> Arc<dyn FreeSpace> {
            Arc::new(Self(SpaceInfo {
                available: 1 << 50,
                total: 1 << 50,
            }))
        }
    }

    impl FreeSpace for FakeSpace {
        fn space(&self, _path: &Path) -> io::Result<SpaceInfo> {
            Ok(self.0)
        }
    }

    const CS: u32 = 64 * 1024;

    fn chunks_of(data: &[u8], cs: u32) -> Vec<VerifiedChunk> {
        data.chunks(cs as usize)
            .enumerate()
            .map(|(i, c)| VerifiedChunk {
                offset: i as u64 * u64::from(cs),
                data: bytes::Bytes::copy_from_slice(c),
                hash: *blake3::hash(c).as_bytes(),
            })
            .collect()
    }

    #[tokio::test]
    async fn part_file_survives_drop_and_finalizes() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        std::fs::create_dir(&staging).unwrap();
        let dest = tmp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();
        let data = TestRng(5).bytes(20 * CS as usize + 123);
        let size = data.len() as u64;
        let chunks = chunks_of(&data, CS);
        let policy = SyncPolicy {
            bytes: 4 * u64::from(CS),
            interval: Duration::from_secs(3600),
        };

        let mut part = PartFile::open(&staging, 7, size, CS, policy).await.unwrap();
        for c in chunks.iter().take(10).cloned() {
            part.write_chunk(c).await.unwrap();
        }
        assert_eq!(part.verified_offset(), 10 * u64::from(CS));
        // Out-of-order writes are refused.
        assert!(matches!(
            part.write_chunk(chunks[12].clone()).await,
            Err(Error::OffsetMismatch { expected }) if expected == 10 * u64::from(CS)
        ));
        drop(part); // simulated crash: chunks 8 and 9 were never made durable

        let part_path = staging.join("7.part");
        let mut part = PartFile::open(&staging, 7, size, CS, policy).await.unwrap();
        assert_eq!(part.verified_offset(), 8 * u64::from(CS));
        assert_eq!(
            std::fs::metadata(&part_path).unwrap().len(),
            8 * u64::from(CS)
        );
        let hashes: Vec<ChunkHash> = chunks.iter().map(|c| c.hash).collect();
        assert_eq!(
            part.prefix(CS),
            prefix_hash(8 * u64::from(CS), CS, &hashes[..8])
        );

        for c in chunks.iter().skip(8).cloned() {
            part.write_chunk(c).await.unwrap();
        }
        part.checkpoint().await.unwrap();
        // A file is already at the final name: never overwrite it.
        std::fs::create_dir_all(dest.join("d")).unwrap();
        std::fs::write(dest.join("d").join("x.bin"), b"keep me").unwrap();
        let root = root_hash(size, CS, &hashes);
        let path = vec!["d".to_owned(), "x.bin".to_owned()];
        let final_path = part.finalize(root, &dest, &path).await.unwrap();
        assert_eq!(final_path, dest.join("d").join("x (1).bin"));
        assert_eq!(std::fs::read(&final_path).unwrap(), data);
        assert_eq!(
            std::fs::read(dest.join("d").join("x.bin")).unwrap(),
            b"keep me"
        );
        assert!(!part_path.exists());
        assert_eq!(part.state(), FileState::Verified);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "received files are never executable");
        }
    }

    #[tokio::test]
    async fn part_file_root_mismatch_deletes_part() {
        let tmp = tempfile::tempdir().unwrap();
        let data = TestRng(6).bytes(3 * CS as usize);
        let mut part = PartFile::open(tmp.path(), 0, data.len() as u64, CS, SyncPolicy::default())
            .await
            .unwrap();
        for c in chunks_of(&data, CS) {
            part.write_chunk(c).await.unwrap();
        }
        let r = part
            .finalize([0; 32], tmp.path(), &["out".to_owned()])
            .await;
        assert!(matches!(r, Err(Error::RootMismatch)));
        assert!(!tmp.path().join("0.part").exists());
        assert!(!tmp.path().join("out").exists());
        assert_eq!(part.state(), FileState::Failed);
    }

    fn manifest(sizes: &[(usize, bool)]) -> (Manifest, Vec<Vec<u8>>) {
        let mut rng = TestRng(77);
        let contents: Vec<Vec<u8>> = sizes.iter().map(|&(s, _)| rng.bytes(s)).collect();
        let mut m = Manifest {
            id: crate::transfer::manifest::new_transfer_id(),
            chunk_size: CS,
            total_bytes: contents.iter().map(|c| c.len() as u64).sum(),
            note: None,
            files: sizes
                .iter()
                .enumerate()
                .map(|(i, &(s, b))| FileEntry {
                    fid: u32::try_from(i).unwrap(),
                    path: vec!["T".into(), format!("f{i}.dat")],
                    size: s as u64,
                    mtime: 1_700_000_000_000,
                    mime: None,
                    in_bundle: b,
                })
                .collect(),
            dirs: vec![vec!["T".into(), "empty".into()]],
            bundle: None,
        };
        if m.files.iter().any(|f| f.in_bundle) {
            m.bundle = Some(BundleInfo {
                size: bundle_size(&m),
            });
        }
        (m, contents)
    }

    async fn body(data: &[u8], start: u64, end: u64) -> Vec<u8> {
        use tokio::io::AsyncReadExt;
        let src = &data[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
        let prior = chunks_of(&data[..usize::try_from(start).unwrap()], CS)
            .iter()
            .map(|c| c.hash)
            .collect();
        let mut enc = ChunkEncoder::new(src, start, end, CS, prior).unwrap();
        let mut out = Vec::new();
        enc.read_to_end(&mut out).await.unwrap();
        out
    }

    fn root_of(data: &[u8]) -> ChunkHash {
        let hashes: Vec<ChunkHash> = chunks_of(data, CS).iter().map(|c| c.hash).collect();
        root_hash(data.len() as u64, CS, &hashes)
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)] // one scenario, kept linear so the steps read in order
    async fn receiver_full_flow_with_bundle_and_reload() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Downloads");
        let (m, contents) = manifest(&[
            (5 * CS as usize + 9, false),
            (1000, true),
            (0, false),
            (70_000, true),
            (0, true),
        ]);
        let rx = TransferReceiver::accept(
            &root,
            m.clone(),
            FakeSpace::plenty(),
            AcceptOptions::default(),
        )
        .await
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        // A file already at a destination name: the incoming one gets " (1)".
        std::fs::create_dir(root.join("T")).unwrap();
        std::fs::write(root.join("T").join("f3.dat"), b"keep me").unwrap();
        let st = rx.state();
        assert_eq!(st.state, TransferState::Accepted);
        let tok0 = st.files[0].token.clone().unwrap();
        assert!(st.files[1].token.is_none());
        let btok = st.bundle.as_ref().unwrap().token.clone().unwrap();
        assert_eq!(tok0.len(), 22, "16 bytes base64url");

        // Bad token, bad offset.
        assert!(matches!(
            rx.begin_file(0, "nope", 0, 100).await,
            Err(PutError::BadToken)
        ));
        let size0 = contents[0].len() as u64;
        assert!(matches!(
            rx.begin_file(0, &tok0, u64::from(CS), size0 - 1).await,
            Err(PutError::Core(Error::OffsetMismatch { expected: 0 }))
        ));

        // Send the first 3 chunks of file 0, then "crash".
        let mut up = rx
            .begin_file(0, &tok0, 0, 3 * u64::from(CS) - 1)
            .await
            .unwrap();
        up.receive(&body(&contents[0], 0, 3 * u64::from(CS)).await[..])
            .await
            .unwrap();
        let resp = up.finish(None).await.unwrap();
        assert_eq!(resp.state, FileState::Receiving);
        assert_eq!(resp.verified_offset, 3 * u64::from(CS));
        // Bundle: send first chunk only (bundle total is under 2 chunks).
        let bundle_bytes = {
            use tokio::io::AsyncReadExt;
            struct Mem(Vec<Vec<u8>>);
            impl crate::transfer::MemberOpener for Mem {
                fn open(&self, fid: u32, off: u64) -> crate::transfer::bundle::OpenFuture {
                    let d = self.0[fid as usize][usize::try_from(off).unwrap()..].to_vec();
                    Box::pin(async move {
                        Ok(Box::new(std::io::Cursor::new(d))
                            as crate::transfer::bundle::MemberReader)
                    })
                }
            }
            let layout = Arc::new(BundleLayout::new(&m).unwrap());
            let mut w =
                crate::transfer::BundleWriter::new(layout, Arc::new(Mem(contents.clone())), 0)
                    .unwrap();
            let mut v = Vec::new();
            w.read_to_end(&mut v).await.unwrap();
            v
        };
        let btotal = bundle_bytes.len() as u64;
        let mut up = rx.begin_bundle(&btok, 0, u64::from(CS) - 1).await.unwrap();
        up.receive(&body(&bundle_bytes, 0, u64::from(CS)).await[..])
            .await
            .unwrap();
        up.finish(None).await.unwrap();
        let st = rx.state();
        assert_eq!(st.bundle.as_ref().unwrap().verified_offset, u64::from(CS));
        assert!(st.bundle.as_ref().unwrap().prefix.is_some());
        assert_eq!(st.files[1].state, FileState::Receiving);
        assert_eq!(st.files[1].verified_offset, 1000);
        assert_eq!(
            st.bytes_verified,
            3 * u64::from(CS) + 1000 + st.files[3].verified_offset
        );
        drop(rx);

        let rx = TransferReceiver::load(&root, &m.id, FakeSpace::plenty(), SyncPolicy::default())
            .await
            .unwrap();
        let st = rx.state();
        assert_eq!(st.files[0].verified_offset, 3 * u64::from(CS));
        assert_eq!(
            st.files[0].prefix.as_deref().unwrap(),
            hash_to_hex(&prefix_hash(
                3 * u64::from(CS),
                CS,
                &chunks_of(&contents[0], CS)[..3]
                    .iter()
                    .map(|c| c.hash)
                    .collect::<Vec<_>>()
            ))
        );
        let start = 3 * u64::from(CS);
        let mut up = rx.begin_file(0, &tok0, start, size0 - 1).await.unwrap();
        up.receive(&body(&contents[0], start, size0).await[..])
            .await
            .unwrap();
        let resp = up.finish(Some(root_of(&contents[0]))).await.unwrap();
        assert_eq!(resp.state, FileState::Verified);
        assert_eq!(resp.root.unwrap(), hash_to_hex(&root_of(&contents[0])));
        assert_eq!(rx.transfer_state(), TransferState::Active);
        assert_eq!(rx.saved_paths(), [root.join("T").join("f0.dat")]);
        // A restart after file 0 was placed still knows where it went.
        drop(rx);
        let rx = TransferReceiver::load(&root, &m.id, FakeSpace::plenty(), SyncPolicy::default())
            .await
            .unwrap();
        assert_eq!(rx.saved_paths(), [root.join("T").join("f0.dat")]);
        assert_eq!(rx.receive_root(), root.as_path());

        let bstart = u64::from(CS);
        let mut up = rx.begin_bundle(&btok, bstart, btotal - 1).await.unwrap();
        up.receive(&body(&bundle_bytes, bstart, btotal).await[..])
            .await
            .unwrap();
        let resp = up.finish(Some(root_of(&bundle_bytes))).await.unwrap();
        assert_eq!(resp.state, FileState::Verified);
        assert_eq!(rx.transfer_state(), TransferState::Complete);

        let t = root.join("T");
        assert_eq!(std::fs::read(t.join("f0.dat")).unwrap(), contents[0]);
        assert_eq!(std::fs::read(t.join("f1.dat")).unwrap(), contents[1]);
        assert_eq!(std::fs::read(t.join("f2.dat")).unwrap(), b"");
        assert_eq!(std::fs::read(t.join("f3 (1).dat")).unwrap(), contents[3]);
        assert_eq!(std::fs::read(t.join("f3.dat")).unwrap(), b"keep me");
        assert_eq!(std::fs::read(t.join("f4.dat")).unwrap(), b"");
        assert!(t.join("empty").is_dir());
        assert_eq!(
            rx.saved_paths(),
            ["f0.dat", "f1.dat", "f2.dat", "f3 (1).dat", "f4.dat"].map(|n| t.join(n)),
            "every destination in manifest order, collision rename included"
        );
        let st = rx.state();
        assert_eq!(st.bytes_verified, m.total_bytes);
        let json = serde_json::to_value(&st).unwrap();
        assert_eq!(json["files"][0]["state"], "verified");
        assert!(json["files"][0].get("prefix").is_none());
        assert_eq!(json["state"], "complete");
    }

    #[tokio::test]
    async fn chunk_retries_then_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, contents) = manifest(&[(2 * CS as usize, false), (CS as usize, false)]);
        let rx =
            TransferReceiver::accept(tmp.path(), m, FakeSpace::plenty(), AcceptOptions::default())
                .await
                .unwrap();
        let tok = rx.state().files[0].token.clone().unwrap();
        let good = body(&contents[0], 0, contents[0].len() as u64).await;
        let mut bad = good.clone();
        bad[CS as usize + 32 + 10] ^= 1; // second chunk
        for attempt in 1..=4 {
            let start = if attempt == 1 { 0 } else { u64::from(CS) };
            let src = if attempt == 1 {
                &bad[..]
            } else {
                &bad[CS as usize + 32..]
            };
            let mut up = rx
                .begin_file(0, &tok, start, 2 * u64::from(CS) - 1)
                .await
                .unwrap();
            let r = up.receive(src).await;
            assert!(
                matches!(r, Err(PutError::Core(Error::ChunkHashMismatch { offset })) if offset == u64::from(CS))
            );
            let resp = up.response();
            if attempt <= 3 {
                assert_eq!(resp.state, FileState::Receiving, "attempt {attempt}");
                assert_eq!(resp.verified_offset, u64::from(CS));
            } else {
                assert_eq!(resp.state, FileState::Failed);
            }
        }
        assert!(!rx.staging_dir().join("0.part").exists());
        assert!(rx
            .begin_file(0, &tok, 0, 2 * u64::from(CS) - 1)
            .await
            .is_err());
        // The other file still completes; the transfer then ends failed.
        let tok1 = rx.state().files[1].token.clone().unwrap();
        let mut up = rx.begin_file(1, &tok1, 0, u64::from(CS) - 1).await.unwrap();
        up.receive(&body(&contents[1], 0, u64::from(CS)).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&contents[1]))).await.unwrap();
        assert_eq!(rx.transfer_state(), TransferState::Failed);
    }

    #[tokio::test]
    async fn space_cancel_skip_and_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, contents) = manifest(&[(CS as usize, false), (CS as usize, false)]);
        let tight: Arc<dyn FreeSpace> = Arc::new(FakeSpace(SpaceInfo {
            available: MIN_RESERVE + 1000,
            total: 10 * MIN_RESERVE,
        }));
        assert!(matches!(
            TransferReceiver::accept(tmp.path(), m.clone(), tight, AcceptOptions::default()).await,
            Err(Error::NoSpace)
        ));
        let opts = AcceptOptions {
            skip: vec![1],
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(tmp.path(), m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let st = rx.state();
        assert_eq!(st.files[1].state, FileState::Skipped);
        assert!(st.files[1].token.is_none());
        let tok = st.files[0].token.clone().unwrap();
        let mut up = rx.begin_file(0, &tok, 0, u64::from(CS) - 1).await.unwrap();
        let b = body(&contents[0], 0, u64::from(CS)).await;
        up.receive(&b[..b.len() / 2]).await.unwrap_err();
        drop(up);
        rx.cancel().await.unwrap();
        assert!(matches!(
            rx.begin_file(0, &tok, 0, u64::from(CS) - 1).await,
            Err(PutError::Cancelled)
        ));
        assert!(!rx.staging_dir().join("0.part").exists());

        let removed = cleanup_idle(tmp.path(), Duration::from_secs(3600))
            .await
            .unwrap();
        assert_eq!(removed, Vec::<String>::new());
        let removed = cleanup_idle(tmp.path(), Duration::ZERO).await.unwrap();
        assert_eq!(removed, vec![m.id.clone()]);

        let mut reserved = m.clone();
        reserved.files[0].path = vec![".PlayDrop-Partial".into(), "x".into()];
        assert!(matches!(
            TransferReceiver::accept(
                tmp.path(),
                reserved,
                FakeSpace::plenty(),
                AcceptOptions::default()
            )
            .await,
            Err(Error::PathInvalid(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn planted_symlink_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = tmp.path().join("r");
        std::fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("T")).unwrap();
        let (m, contents) = manifest(&[(CS as usize, false)]);
        let rx = TransferReceiver::accept(&root, m, FakeSpace::plenty(), AcceptOptions::default())
            .await
            .unwrap();
        let tok = rx.state().files[0].token.clone().unwrap();
        let mut up = rx.begin_file(0, &tok, 0, u64::from(CS) - 1).await.unwrap();
        up.receive(&body(&contents[0], 0, u64::from(CS)).await[..])
            .await
            .unwrap();
        let r = up.finish(Some(root_of(&contents[0]))).await;
        assert!(matches!(r, Err(PutError::Core(Error::PathInvalid(_)))));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
