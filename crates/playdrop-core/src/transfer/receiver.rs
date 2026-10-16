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
//!     bundle.delivered     delivery log of the bundle's members (see `DeliveryLog`)
//! ```
//!
//! A streamed file leaves staging only after its root verifies, through an exclusive create
//! of the final name (with " (n)" on collision) and a rename onto that placeholder (SR-18).
//! A bundle member leaves the same way as soon as its own bytes are chunk-verified and
//! durable, before the bundle root arrives (see `BundleInner`).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex as StdMutex, MutexGuard};
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
use super::manifest::{is_valid_chunk_size, is_valid_transfer_id, FileEntry, Manifest};
use super::sink::{DeliverItem, DeliveredFile, ReceiveSink};
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

/// `Staged` mode (`docs/core-api.md` section 2): every verified file goes to `sink`
/// instead of being renamed beneath the receive root, which is then only a private staging
/// directory.
#[derive(Debug, Clone)]
pub struct SinkBinding {
    /// The host's sink.
    pub sink: Arc<dyn ReceiveSink>,
    /// The sender's Device ID (hex), for [`DeliverItem::peer`].
    pub peer: String,
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
    /// `Staged` mode: hand verified files to a sink. `None` places them beneath the root.
    pub sink: Option<SinkBinding>,
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
/// (after any " (n)" collision rename), or in `Staged` mode the sink's location string.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SavedFile {
    fid: u32,
    #[serde(default)]
    path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    location: Option<String>,
}

impl SavedFile {
    fn placed(fid: u32, path: PathBuf) -> Self {
        Self {
            fid,
            path,
            location: None,
        }
    }

    fn delivered(fid: u32, location: String) -> Self {
        Self {
            fid,
            path: PathBuf::new(),
            location: Some(location),
        }
    }
}

/// Where verified files go.
#[derive(Debug, Clone)]
enum Dest {
    /// Renamed beneath this receive root (desktop).
    Directory(PathBuf),
    /// Handed to a sink (mobile, `docs/core-api.md` section 2).
    Sink {
        binding: SinkBinding,
        transfer_id: String,
    },
}

impl Dest {
    /// Where verified files of transfer `transfer_id` go: `sink` when bound, else beneath
    /// `root`.
    fn new(root: &Path, sink: Option<&SinkBinding>, transfer_id: &str) -> Self {
        match sink {
            Some(binding) => Dest::Sink {
                binding: binding.clone(),
                transfer_id: transfer_id.to_owned(),
            },
            None => Dest::Directory(root.to_path_buf()),
        }
    }

    /// Places or delivers one verified staged file and says where it went. In `Staged`
    /// mode the staging file is deleted afterwards whatever the sink did, and a sink
    /// failure becomes [`Error::Sink`].
    fn put(&self, staged: &Path, entry: &FileEntry) -> Result<SavedFile> {
        // File names are allowed at DEBUG and below only (SR-32).
        tracing::debug!(fid = entry.fid, path = %entry.path.join("/"), "verified file placed");
        match self {
            Dest::Directory(root) => {
                let rel = place(staged, root, &entry.path)?;
                Ok(SavedFile::placed(entry.fid, rel))
            }
            Dest::Sink {
                binding,
                transfer_id,
            } => {
                let res = hand_to_sink(binding, transfer_id, staged, entry);
                fsx::remove_if_exists(staged)?;
                res.map(|loc| SavedFile::delivered(entry.fid, loc))
            }
        }
    }
}

/// Hands one verified staged file to the sink and returns the sink's location string. The
/// staging file is left in place; a sink failure becomes [`Error::Sink`].
fn hand_to_sink(
    binding: &SinkBinding,
    transfer_id: &str,
    staged: &Path,
    entry: &FileEntry,
) -> Result<String> {
    // Opened so that a later delete works on Windows even if the sink keeps the handle
    // (FILE_SHARE_DELETE, see `fsx::open_shared_read`).
    let file = fsx::open_shared_read(staged)?;
    let item = DeliverItem {
        transfer_id: transfer_id.to_owned(),
        peer: binding.peer.clone(),
        path: entry.path.clone(),
        size: entry.size,
        mtime_ms: entry.mtime,
        mime: entry.mime.clone(),
    };
    binding
        .sink
        .deliver(item, file)
        .map_err(|e| Error::Sink(e.message))
}

/// File name of the bundle's delivery log in the staging folder.
const DELIVERY_LOG: &str = "bundle.delivered";

/// Members handed over between two flushes of the delivery log. In `Staged` mode the
/// staging files of a batch are deleted only after its flush.
const DELIVERY_BATCH: usize = 256;

/// First line of the delivery log.
///
/// Version 2 (this build) carries no root: the log starts with the bundle upload, before
/// any member is placed, and the verified root is kept in the bundle's ledger record.
/// Version 1 (Phase 7 builds) carried the root, because it was written only once the root
/// had verified and just before the members were handed over.
#[derive(Debug, Serialize, Deserialize)]
struct DeliveryHeader {
    v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    root: Option<String>,
}

/// What a delivery log holds after a restart.
#[derive(Debug)]
struct DeliveryRecord {
    /// The verified bundle root, from a version 1 log only.
    root: Option<ChunkHash>,
    /// Members recorded as delivered (or, in `Directory` mode, about to be renamed onto
    /// the recorded placeholder), by fid; a later line for the same fid wins.
    done: HashMap<u32, SavedFile>,
    /// Length of the complete lines; a torn last line is cut off before appending.
    valid_len: u64,
}

/// Append-only record of a bundle's member delivery, so a kill while members are handed
/// over neither loses a member nor delivers one twice.
///
/// `bundle.delivered` holds newline-terminated JSON lines. Line 1 is `{"v":2}`, written
/// atomically when the bundle upload starts. Each following line is one [`SavedFile`]:
/// `{"fid":3,"path":"T/f3 (1).dat"}` in `Directory` mode, written after the placeholder
/// is reserved and before the rename onto it; `{"fid":3,"path":"","location":"..."}` in
/// `Staged` mode, written after the sink returned. Members are handed over as soon as
/// their bytes are chunk-verified and durable, long before the bundle root arrives (see
/// [`BundleInner`]). The log is flushed every [`DELIVERY_BATCH`] members and whenever the
/// delivery worker stops cleanly; the ledger's terminal record then lists every
/// destination, and the log is cleared with the rest of the staging folder.
///
/// A version 1 log left by a Phase 7 build is read the same way: its lines are members
/// already delivered, and its header root (written only after that root verified) stands
/// in for the root this build keeps in the ledger record.
#[derive(Debug)]
struct DeliveryLog {
    file: File,
}

impl DeliveryLog {
    fn path(staging: &Path) -> PathBuf {
        staging.join(DELIVERY_LOG)
    }

    /// Starts the log for a bundle whose upload is starting.
    fn create(staging: &Path) -> Result<Self> {
        let mut header = serde_json::to_vec(&DeliveryHeader { v: 2, root: None })?;
        header.push(b'\n');
        let path = Self::path(staging);
        fsx::write_atomic(&path, &header)?;
        Self::open(&path, header.len() as u64)
    }

    /// Reopens the log for appending after its last complete line.
    fn open(path: &Path, valid_len: u64) -> Result<Self> {
        let mut file = fsx::open_existing_regular(path)?;
        file.set_len(valid_len)?;
        file.seek(SeekFrom::End(0))?;
        Ok(Self { file })
    }

    /// Reads the log left by an earlier run. `None` when there is none or its header is
    /// unreadable (then no member had left staging).
    fn read(staging: &Path) -> Result<Option<DeliveryRecord>> {
        let bytes = match std::fs::read(Self::path(staging)) {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mut lines = bytes.split_inclusive(|&b| b == b'\n');
        let Some(first) = lines.next().filter(|l| l.ends_with(b"\n")) else {
            return Ok(None);
        };
        let Some(header) = serde_json::from_slice::<DeliveryHeader>(first).ok() else {
            return Ok(None);
        };
        let root = match (header.v, header.root.as_deref()) {
            (1, Some(hex)) => match super::hash_from_hex(hex) {
                Ok(root) => Some(root),
                Err(_) => return Ok(None),
            },
            (2, _) => None,
            _ => return Ok(None),
        };
        let mut valid_len = first.len() as u64;

        let mut done = HashMap::new();
        for line in lines {
            // A line torn by a power loss ends the log; it is cut off before appending.
            let Some(entry) = line
                .ends_with(b"\n")
                .then(|| serde_json::from_slice::<SavedFile>(line).ok())
                .flatten()
            else {
                break;
            };
            done.insert(entry.fid, entry);
            valid_len += line.len() as u64;
        }
        Ok(Some(DeliveryRecord {
            root,
            done,
            valid_len,
        }))
    }

    /// Appends one line. Not flushed: a killed process still leaves it in the page cache,
    /// and [`sync`](Self::sync) makes batches durable against a power loss.
    fn append(&mut self, saved: &SavedFile) -> Result<()> {
        let mut line = serde_json::to_vec(saved)?;
        line.push(b'\n');
        self.file.write_all(&line).map_err(fsx::map_io)
    }

    fn sync(&self) -> Result<()> {
        self.file.sync_data().map_err(fsx::map_io)
    }
}

/// Test-only kill points inside member delivery. The count is of members handed over by
/// one delivery worker (for a streamed file it is always 0).
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Crash {
    /// Stop once this many members were handed over, before the next one starts. For a
    /// streamed file: after the root verified, before its final name is reserved (or, in
    /// `Staged` mode, before the sink is called).
    Before(usize),
    /// Stop in the middle of member number n: in `Directory` mode after its log line (or,
    /// for a streamed file, its ledger record with the reserved name) and before its
    /// rename; in `Staged` mode after the sink took it and before it is recorded.
    Inside(usize),
    /// Streamed file in `Directory` mode only: after the rename onto the reserved name and
    /// before the ledger records the file as verified.
    After(usize),
}

/// The error a test kill point returns.
#[cfg(test)]
fn simulated_kill() -> Error {
    Error::Io(io::Error::other("simulated kill"))
}

/// What a bundle and its delivery worker share with the rest of the transfer.
#[derive(Debug, Clone)]
struct DeliveryShared {
    /// Members placed or handed to the sink so far, for
    /// [`TransferReceiver::files_done`].
    done_count: Arc<AtomicUsize>,
    /// Set by a cancel: once it is true no member is placed. The worker holds it while it
    /// places one member, so a cancel waits for the member in flight before it clears the
    /// staging folder, and never leaves a reserved name without its file.
    cancelled: Arc<StdMutex<bool>>,
    /// Raised by a cancel before it takes `cancelled`. `std` mutexes are not fair: a worker
    /// that drops and takes the lock again for every member could keep a waiting cancel
    /// out for thousands of members. The worker checks this flag before it takes the lock,
    /// so the cancel gets in right after the member in flight.
    cancel_requested: Arc<AtomicBool>,
}

impl DeliveryShared {
    fn new() -> Self {
        Self {
            done_count: Arc::new(AtomicUsize::new(0)),
            cancelled: Arc::new(StdMutex::new(false)),
            cancel_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Stops all member delivery for good: waits for the member in flight, after which no
    /// member is placed.
    fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::Release);
        *lock(&self.cancelled) = true;
    }

    fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::Acquire)
    }
}

/// Destination folders of `Directory` mode, each walked once beneath the receive root
/// (SR-18) and its handle kept for every member in it. Folders that received a member are
/// synced together when the worker stops, so thousands of small files do not cost
/// thousands of journal commits.
///
/// A kept handle follows its folder: if the user moves a destination folder while the
/// transfer runs, later members land in the moved folder. That is a local user's action on
/// their own folder, never a path the sender controls (every name was walked beneath the
/// receive root without following symlinks when the handle was opened). A deleted folder
/// makes the next placement fail; the next worker opens fresh handles.
struct Placer {
    staging: cap_std::fs::Dir,
    root: cap_std::fs::Dir,
    dirs: HashMap<Vec<String>, cap_std::fs::Dir>,
    touched: HashSet<Vec<String>>,
}

impl Placer {
    fn open(staging: &Path, dest_root: &Path) -> Result<Self> {
        Ok(Self {
            staging: fsx::open_own_dir(staging)?,
            root: fsx::open_root(dest_root)?,
            dirs: HashMap::new(),
            touched: HashSet::new(),
        })
    }

    /// Opens (once) the destination folder `parents` and marks it for the next sync.
    fn ensure_dir(&mut self, parents: &[String]) -> Result<()> {
        if !self.dirs.contains_key(parents) {
            let dir = fsx::safe_dir(&self.root, parents, false)?;
            self.dirs.insert(parents.to_vec(), dir);
        }
        if !self.touched.contains(parents) {
            self.touched.insert(parents.to_vec());
        }
        Ok(())
    }

    /// Makes the renames into every touched folder durable.
    fn sync(&mut self) -> Result<()> {
        for parents in self.touched.drain() {
            if let Some(dir) = self.dirs.get(&parents) {
                fsx::sync_dir_handle(dir)?;
            }
        }
        Ok(())
    }
}

/// Everything a delivery worker owns, moved onto its thread.
struct DeliveryWorker {
    dest: Dest,
    files: Arc<Vec<FileEntry>>,
    staging: PathBuf,
    log: DeliveryLog,
    /// Members delivered so far, from the log and from this worker.
    done: HashMap<u32, SavedFile>,
    shared: DeliveryShared,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    #[cfg(test)]
    crash: Option<Crash>,
    #[cfg(test)]
    killed: Arc<AtomicBool>,
    /// Test hook: an I/O error (not a kill) before fresh member n.
    #[cfg(test)]
    transient: Option<usize>,
}

/// What a delivery worker leaves behind when it stops.
#[derive(Debug)]
struct WorkerExit {
    log: DeliveryLog,
    done: HashMap<u32, SavedFile>,
    result: Result<()>,
}

/// A running delivery worker. Members go in by fid, in archive order.
#[derive(Debug)]
struct WorkerHandle {
    jobs: mpsc::Sender<u32>,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<WorkerExit>,
}

impl DeliveryWorker {
    /// Serves members until the job channel closes, then flushes. Stops at the first
    /// error, without a flush: what it did is in the log (page cache) and is checked
    /// again from disk before any later delivery.
    fn run(mut self, jobs: &mpsc::Receiver<u32>) -> WorkerExit {
        let result = self.serve(jobs);
        if result.is_err() {
            self.failed.store(true, Ordering::Release);
        }
        WorkerExit {
            log: self.log,
            done: self.done,
            result,
        }
    }

    fn serve(&mut self, jobs: &mpsc::Receiver<u32>) -> Result<()> {
        let dest = self.dest.clone();
        let mut placer = match &dest {
            Dest::Directory(root) => Some(Placer::open(&self.staging, root)?),
            Dest::Sink { .. } => None,
        };
        let mut handed: Vec<PathBuf> = Vec::new();
        let mut fresh = 0usize;
        let cancelled = Arc::clone(&self.shared.cancelled);
        while let Ok(fid) = jobs.recv() {
            if self.stop.load(Ordering::Acquire) || self.shared.is_cancelled() {
                break;
            }
            let gate = lock(&cancelled);
            if *gate {
                break;
            }
            match (&dest, placer.as_mut()) {
                (Dest::Directory(_), Some(placer)) => self.place(fid, placer, &mut fresh)?,
                (
                    Dest::Sink {
                        binding,
                        transfer_id,
                    },
                    _,
                ) => self.hand(fid, binding, transfer_id, &mut handed, &mut fresh)?,
                (Dest::Directory(_), None) => unreachable!("Directory mode opens a placer"),
            }
            drop(gate);
        }
        if let Some(placer) = placer.as_mut() {
            placer.sync()?;
        }
        self.log.sync()?;
        for p in handed {
            fsx::remove_if_exists(&p)?;
        }
        Ok(())
    }

    fn record(&mut self, saved: SavedFile) {
        self.done.insert(saved.fid, saved);
        self.shared.done_count.fetch_add(1, Ordering::Release);
    }

    #[cfg(test)]
    fn crash_at(&self, at: Crash) -> Result<()> {
        if let Crash::Before(n) = at {
            if self.transient == Some(n) {
                return Err(Error::Io(io::Error::other("simulated transient error")));
            }
        }
        if self.crash == Some(at) {
            self.killed.store(true, Ordering::Release);
            return Err(simulated_kill());
        }
        Ok(())
    }

    /// `Directory` mode, one member (SR-18): the final name is reserved exclusively
    /// (" (n)" on collision), a log line names it, then the staged file is renamed onto
    /// it. A member delivered before a restart of the stream keeps its first place; the
    /// bytes that came again are dropped.
    fn place(&mut self, fid: u32, placer: &mut Placer, fresh: &mut usize) -> Result<()> {
        let staged_name = member_name(fid);
        let staged = self.staging.join(&staged_name);
        let staged_exists = exists(&staged)?;
        if self.done.contains_key(&fid) {
            if staged_exists {
                fsx::remove_if_exists(&staged)?;
            }
            return Ok(());
        }
        let files = Arc::clone(&self.files);
        let entry = files
            .get(fid as usize)
            .ok_or_else(|| Error::BadRequest(format!("unknown bundle member {fid}")))?;
        let (name, parents) = split_path(&entry.path)?;
        placer.ensure_dir(parents)?;
        let dir = &placer.dirs[parents];
        if !staged_exists {
            // Durable bytes without a staging file and without a log line: only after a
            // power loss that kept the rename but lost its log line (not possible on a
            // filesystem that orders a file's appended data before a later rename, like
            // ext4). Believed only when a regular file of the member's size sits at its
            // manifest name; its " (n)" name cannot be known, so that name is reported.
            // Anything else is an inconsistency, found again by `reconcile` on load.
            if !has_file_of_size(dir, name, entry.size)? {
                return Err(Error::Io(io::Error::new(
                    io::ErrorKind::NotFound,
                    "a verified bundle member is missing from staging",
                )));
            }
            self.record(SavedFile::placed(fid, relative_path(parents, name)));
            return Ok(());
        }
        #[cfg(test)]
        self.crash_at(Crash::Before(*fresh))?;
        // File names are allowed at DEBUG and below only (SR-32).
        tracing::debug!(fid, path = %entry.path.join("/"), "verified file placed");
        // A kill between this exclusive create and the log line leaves one empty file
        // behind; closing that window needs `renameat2(RENAME_NOREPLACE)` (rename without a
        // placeholder), which `cap-std` does not offer on every platform.
        let (final_name, placeholder) = fsx::create_unique(dir, name)?;
        drop(placeholder);
        let saved = SavedFile::placed(fid, relative_path(parents, &final_name));
        if let Err(e) = self.log.append(&saved) {
            // Without its log line the placeholder would stay empty forever and every
            // retry would add another " (n)": remove it (best effort).
            let _ = dir.remove_file(&final_name);
            return Err(e);
        }
        #[cfg(test)]
        self.crash_at(Crash::Inside(*fresh))?;
        placer
            .staging
            .rename(&staged_name, dir, &final_name)
            .map_err(fsx::map_io)?;
        self.record(saved);
        *fresh += 1;
        if fresh.is_multiple_of(DELIVERY_BATCH) {
            // The destination folders are synced once, when the worker stops: a rename a
            // power loss undoes is redone from this log, and a folder sync per batch would
            // force journal commits while the stream is still writing.
            self.log.sync()?;
        }
        Ok(())
    }

    /// `Staged` mode, one member: the sink takes the staged file, then a log line records
    /// its location. Staging files are deleted a batch at a time, only after the log
    /// lines naming them are durable, so a member whose staging file is gone is always in
    /// the log. A kill after the sink took a member but before its line was written hands
    /// that one member to the sink once more (the sink trait cannot make the two atomic);
    /// no member is ever delivered more than twice, and only the one in flight can be.
    fn hand(
        &mut self,
        fid: u32,
        binding: &SinkBinding,
        transfer_id: &str,
        handed: &mut Vec<PathBuf>,
        fresh: &mut usize,
    ) -> Result<()> {
        let staged = self.staging.join(member_name(fid));
        if self.done.contains_key(&fid) {
            // Delivered before a restart of the stream; its bytes came again.
            fsx::remove_if_exists(&staged)?;
            return Ok(());
        }
        if !exists(&staged)? {
            return Err(Error::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "a verified bundle member is missing from staging",
            )));
        }
        let files = Arc::clone(&self.files);
        let entry = files
            .get(fid as usize)
            .ok_or_else(|| Error::BadRequest(format!("unknown bundle member {fid}")))?;
        #[cfg(test)]
        self.crash_at(Crash::Before(*fresh))?;
        tracing::debug!(fid, path = %entry.path.join("/"), "verified file placed");
        let location = hand_to_sink(binding, transfer_id, &staged, entry)?;
        #[cfg(test)]
        self.crash_at(Crash::Inside(*fresh))?;
        let saved = SavedFile::delivered(fid, location);
        self.log.append(&saved)?;
        self.record(saved);
        handed.push(staged);
        *fresh += 1;
        if handed.len() >= DELIVERY_BATCH {
            self.log.sync()?;
            for p in handed.drain(..) {
                fsx::remove_if_exists(&p)?;
            }
        }
        Ok(())
    }
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
    /// Verified offset of the last durable record: every byte below it is synced. Bundle
    /// members ending at or below it may leave staging.
    durable: u64,
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
            durable: 0,
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
            l.durable = p.verified_offset;
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
        l.durable = verified;
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
        self.durable = verified;
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
        self.durable = 0;
        self.saved.clear();
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
        self.durable = verified;
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
    #[cfg(test)]
    crash: Option<Crash>,
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

    /// Test hook: fails like a killed process at `at`.
    #[cfg(test)]
    fn crash_at(&self, at: Crash) -> Result<()> {
        if self.crash == Some(at) {
            return Err(simulated_kill());
        }
        Ok(())
    }

    /// The root of a file whose bytes all arrived and whose root verified, while it still
    /// waits to be placed (a kill or an I/O error stopped the placement).
    fn pending_root(&self) -> Option<ChunkHash> {
        let root = lock(&self.ledger.progress).root?;
        self.ledger.root_matches(&root).then_some(root)
    }

    fn finalize(&mut self, claimed: &ChunkHash, dest: &Dest, entry: &FileEntry) -> Result<()> {
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
        if lock(&self.ledger.progress).root != Some(*claimed) {
            // The root is made durable before anything leaves staging, so a kill or an I/O
            // error from here on places the file from staging on the next try instead of
            // asking the sender for it again. In `Directory` mode it also shrinks the step
            // after the name is reserved to one small record write.
            lock(&self.ledger.progress).root = Some(*claimed);
            self.ledger.persist()?;
        }
        self.place(dest, entry)
    }

    /// Moves a verified file to its final place, then records it as verified. The root
    /// is already checked and kept in memory.
    ///
    /// `Directory` mode closes the window between the rename and the terminal record: the
    /// final name is reserved first (exclusive create, " (n)" on collision, SR-18), a
    /// ledger record still saying `receiving` names it together with the root, then the
    /// `.part` is renamed onto that placeholder, then the record says `verified`. After a
    /// kill, [`PartFile::open_blocking`] and this method read the record: a missing `.part`
    /// means the rename happened; a `.part` still there is renamed onto the placeholder
    /// when it is still an empty regular file, and placed afresh otherwise. No file is
    /// downloaded or placed twice.
    ///
    /// `Staged` mode hands the file to the sink, then records it as verified with the
    /// sink's location, then deletes the staging file. A kill between the sink call and
    /// that record hands this one file to the sink again on restart (at most twice), as
    /// for bundle members.
    fn place(&mut self, dest: &Dest, entry: &FileEntry) -> Result<()> {
        let root = lock(&self.ledger.progress)
            .root
            .ok_or_else(|| Error::BadRequest("file root not verified".into()))?;
        let staged_exists = exists(&self.path)?;
        match dest {
            Dest::Directory(dest_root) => {
                let reserved = self
                    .ledger
                    .saved
                    .first()
                    .filter(|s| s.fid == self.fid && s.location.is_none())
                    .cloned();
                if !staged_exists {
                    if reserved.is_some() {
                        // Renamed before a kill; only the terminal record was missing.
                        return self.ledger.finish(FileState::Verified, Some(root));
                    }
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "a verified file is missing from staging",
                    )));
                }
                let (staging, staged_name) = staged_parts(&self.path)?;
                let staging = fsx::open_own_dir(staging)?;
                let staged_name = staged_name.to_owned();
                let root_dir = fsx::open_root(dest_root)?;
                let (name, parents) = split_path(&entry.path)?;
                let dir = fsx::safe_dir(&root_dir, parents, false)?;
                let reused = match &reserved {
                    Some(prev) => reserved_placeholder(&dir, parents, &prev.path)?,
                    None => None,
                };
                let final_name = if let Some(name) = reused {
                    name
                } else {
                    #[cfg(test)]
                    self.crash_at(Crash::Before(0))?;
                    // File names are allowed at DEBUG and below only (SR-32).
                    tracing::debug!(fid = self.fid, path = %entry.path.join("/"), "verified file placed");
                    // A kill between this exclusive create and the record below leaves one
                    // empty file behind; closing that window needs
                    // `renameat2(RENAME_NOREPLACE)` (rename without a placeholder).
                    let (final_name, placeholder) = fsx::create_unique(&dir, name)?;
                    drop(placeholder);
                    self.ledger.saved = vec![SavedFile::placed(
                        self.fid,
                        relative_path(parents, &final_name),
                    )];
                    if let Err(e) = self.ledger.persist() {
                        // Without the record the placeholder would stay empty forever and
                        // every retry would add another " (n)": remove it (best effort).
                        self.ledger.saved.clear();
                        let _ = dir.remove_file(&final_name);
                        return Err(e);
                    }
                    final_name
                };
                #[cfg(test)]
                self.crash_at(Crash::Inside(0))?;
                staging
                    .rename(&staged_name, &dir, &final_name)
                    .map_err(fsx::map_io)?;
                fsx::sync_dir_handle(&dir)?;
                #[cfg(test)]
                self.crash_at(Crash::After(0))?;
                self.ledger.finish(FileState::Verified, Some(root))
            }
            Dest::Sink {
                binding,
                transfer_id,
            } => {
                if !staged_exists {
                    return Err(Error::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "a verified file is missing from staging",
                    )));
                }
                #[cfg(test)]
                self.crash_at(Crash::Before(0))?;
                tracing::debug!(fid = self.fid, path = %entry.path.join("/"), "verified file placed");
                let location = match hand_to_sink(binding, transfer_id, &self.path, entry) {
                    Ok(location) => location,
                    Err(e @ Error::Sink(_)) => {
                        self.fail()?;
                        return Err(e);
                    }
                    Err(e) => return Err(e),
                };
                #[cfg(test)]
                self.crash_at(Crash::Inside(0))?;
                self.ledger.saved = vec![SavedFile::delivered(self.fid, location)];
                self.ledger.finish(FileState::Verified, Some(root))?;
                fsx::remove_if_exists(&self.path)?;
                Ok(())
            }
        }
    }

    /// Places a file left waiting by a kill or an I/O error, without asking the sender for
    /// anything. Returns where it went.
    fn retry_placement(&mut self, dest: &Dest, entry: &FileEntry) -> Result<Vec<SavedFile>> {
        if self.pending_root().is_none() {
            return Err(Error::BadRequest("file root not verified".into()));
        }
        self.file = None;
        self.place(dest, entry)?;
        Ok(self.ledger.saved.clone())
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
                #[cfg(test)]
                crash: None,
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
        // A record with the root and a reserved final name, not yet verified: the file was
        // being placed when the process stopped (`PartInner::place`). Its `.part` may
        // already be renamed away.
        let placing =
            verified == size && lock(&ledger.progress).root.is_some() && !ledger.saved.is_empty();
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
                if verified > 0 && !placing {
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

    /// After a restart, finishes placing a file that was being placed when the process
    /// stopped, from staging and without asking the sender for anything. Returns the
    /// sink's message when the sink refused it (the file then failed); any other error
    /// leaves the file waiting for the next upload request.
    fn resume_placement(&mut self, dest: &Dest, entry: &FileEntry) -> Option<String> {
        if !self.placement_pending() {
            return None;
        }
        let inner = self.inner.as_mut()?;
        match inner.retry_placement(dest, entry) {
            Ok(_) => None,
            Err(Error::Sink(why)) => Some(why),
            Err(e) => {
                tracing::warn!(
                    fid = entry.fid,
                    "placing a verified file still fails after a restart; it is tried again at the next upload request"
                );
                tracing::debug!(error = %e, "placement error");
                None
            }
        }
    }

    /// True when every byte arrived and the root verified but the file is not placed yet.
    fn placement_pending(&self) -> bool {
        self.inner
            .as_ref()
            .is_some_and(|p| p.pending_root().is_some())
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
        self.run(move |p| {
            let entry = FileEntry {
                fid: p.fid,
                path,
                size: p.ledger.size,
                mtime: 0,
                mime: None,
                in_bundle: false,
            };
            p.finalize(&root, &Dest::Directory(dest_root.clone()), &entry)?;
            let rel = p
                .ledger
                .saved
                .first()
                .map(|s| s.path.clone())
                .unwrap_or_default();
            Ok(dest_root.join(rel))
        })
        .await
    }
}

// ---------------------------------------------------------------------------------------
// Bundle sink
// ---------------------------------------------------------------------------------------

/// Receive state of the bundle stream (protocol.md 7.7, SR-19) and the delivery of its
/// members.
///
/// Members are written to `<staging>/<fid>.part` as their chunks verify. A member is
/// handed over (renamed beneath the receive root, or given to the sink) as soon as every
/// byte of it is chunk-verified and durable: its data ends at or below the verified offset
/// the ledger last made durable. Eligibility therefore advances at each checkpoint, never
/// for bytes that only live in the page cache. Handing over runs on a dedicated delivery
/// worker thread fed after each checkpoint, so the network and hashing path never waits
/// on the sink or on renames. The worker owns the [`DeliveryLog`]. This places members
/// before the bundle root arrives, a deviation from protocol.md 7.7 approved for Phase 8:
/// each member's own bytes are verified chunk by chunk, so the root only confirms the
/// archive as a whole.
///
/// - Root match: the remaining members are handed over, the worker drains, and the
///   ledger's terminal record lists every destination.
/// - Root mismatch: the bundle fails as before (SR-22, "failed integrity check"), but
///   members already placed stay where they are: their bytes were chunk-verified, and
///   landed files are never discarded, on every platform.
/// - Restart at offset 0 (a sender whose prefix differs, for example a restarted sender
///   that lost its chunk hashes and offers the same id again): the worker stops, the log
///   and its record of delivered members stay, undelivered staging files are deleted and
///   the ledger starts again. A changed manifest would have been refused with
///   `409 conflict`, so sizes and mtimes are unchanged: a member already placed is not
///   delivered a second time. Its bytes from the new stream are written, then dropped, and
///   the final list reports the earlier location.
/// - Cancel: the worker stops after the member in flight and places nothing more; placed
///   members stay, staging is deleted as before.
/// - Kill and load: members in the log are done (a `Directory` member logged but not yet
///   renamed is renamed onto its reserved placeholder first); members durable but not in
///   the log are handed over when streaming resumes, or at once when the whole bundle had
///   arrived and its root verified.
/// - A delivery error that is not the sink's (a transient I/O error) stops the worker
///   but keeps the verified data, the log and the root. The bundle reports `receiving`
///   at its full offset and delivery is tried again at the next `begin_bundle`, `finish`
///   or load. A sender seeing that state restarts at 0; the receiver then finishes the
///   delivery instead and answers `409 offset_mismatch` at the full size, so the sender
///   asks for the state again and finds the bundle verified, with nothing downloaded twice.
#[derive(Debug)]
struct BundleInner {
    ledger: Ledger,
    layout: Arc<BundleLayout>,
    staging: PathBuf,
    skipped: Vec<bool>,
    reader: Option<BundleReader>,
    current: Option<(u32, File)>,
    dirty: Vec<u32>,
    /// Where members go.
    dest: Dest,
    /// The manifest's files, by fid.
    files: Arc<Vec<FileEntry>>,
    delivery: DeliveryShared,
    worker: Option<WorkerHandle>,
    /// The delivery log and the members it records while no worker runs. `None` means
    /// they are read again from disk before the next delivery (after a load, or after a
    /// worker stopped on an error).
    parked: Option<(DeliveryLog, HashMap<u32, SavedFile>)>,
    /// Index in the layout of the first member not yet given to the running worker.
    next: usize,
    /// The error that stopped delivery. No new worker starts until the next retry point
    /// (`begin_bundle`, `finish` or load), so a failing disk is not tried at every chunk.
    stalled: Option<Error>,
    #[cfg(test)]
    crash: Option<Crash>,
    /// Set when a test kill point fired: this "process" does no more delivery.
    #[cfg(test)]
    killed: Arc<AtomicBool>,
    /// Test hook for the next worker only: an I/O error before its fresh member n.
    #[cfg(test)]
    transient: Option<usize>,
}

impl Drop for BundleInner {
    fn drop(&mut self) {
        // A receiver going away (host shutdown, or a test standing in for a kill) stops
        // the worker after the member in flight; the log on disk lets a load resume.
        if self.worker.is_none() {
            return;
        }
        // The last receiver clone is often dropped on an async task (peer removal, idle
        // cleanup). Joining waits for one member and two syncs, so on a multi-thread
        // runtime the wait is announced with `block_in_place` and other tasks move to
        // another thread. It is done here rather than at each host call site so no caller
        // can forget it. A current-thread runtime (tests) cannot hand its tasks over; the
        // join there is short and bounded by the member in flight.
        let multi_thread = tokio::runtime::Handle::try_current()
            .is_ok_and(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread);
        if multi_thread {
            tokio::task::block_in_place(|| {
                let _ = self.stop_worker(false);
            });
        } else {
            let _ = self.stop_worker(false);
        }
    }
}

impl BundleInner {
    #[allow(clippy::too_many_arguments)]
    fn new(
        ledger: Ledger,
        layout: Arc<BundleLayout>,
        staging: PathBuf,
        skipped: Vec<bool>,
        dest: Dest,
        files: Arc<Vec<FileEntry>>,
        delivery: DeliveryShared,
    ) -> Self {
        Self {
            ledger,
            layout,
            staging,
            skipped,
            reader: None,
            current: None,
            dirty: Vec::new(),
            dest,
            files,
            delivery,
            worker: None,
            parked: None,
            next: 0,
            stalled: None,
            #[cfg(test)]
            crash: None,
            #[cfg(test)]
            killed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            transient: None,
        }
    }

    fn member_path(&self, fid: u32) -> PathBuf {
        self.staging.join(member_name(fid))
    }

    fn is_skipped(&self, fid: u32) -> bool {
        self.skipped.get(fid as usize).copied().unwrap_or(false)
    }

    /// The root of a bundle that fully arrived and whose root verified, while members
    /// still wait for delivery.
    fn pending_root(&self) -> Option<ChunkHash> {
        let root = lock(&self.ledger.progress).root?;
        self.ledger.root_matches(&root).then_some(root)
    }

    /// Brings member `.part` files in line with the durable bundle offset. Members in
    /// `done` already left staging.
    fn reconcile(&mut self, done: &HashSet<u32>) -> Result<()> {
        let verified = self.ledger.verified();
        let mut consistent = true;
        for m in self.layout.members() {
            if self.is_skipped(m.fid) || done.contains(&m.fid) {
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
                    // A durable member renamed into place whose log line a power loss
                    // lost; the worker reports it at its manifest name. Believed only
                    // when a regular file of the member's size is there.
                    Err(e)
                        if e.kind() == io::ErrorKind::NotFound
                            && m.data_end() <= verified
                            && self.placed_at_manifest_name(m.fid)? => {}
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

    /// `Directory` mode: true when a regular file of member `fid`'s size sits at its
    /// manifest name beneath the receive root.
    fn placed_at_manifest_name(&self, fid: u32) -> Result<bool> {
        let Dest::Directory(dest_root) = &self.dest else {
            return Ok(false);
        };
        let Some(entry) = self.files.get(fid as usize) else {
            return Ok(false);
        };
        let (name, parents) = split_path(&entry.path)?;
        let root = fsx::open_root(dest_root)?;
        let dir = match fsx::safe_dir(&root, parents, false) {
            Ok(dir) => dir,
            Err(Error::PathInvalid(_)) => return Ok(false),
            Err(e) => return Err(e),
        };
        has_file_of_size(&dir, name, entry.size)
    }

    /// Deletes every member's staging file. The delivery log stays: it records which
    /// members already left staging. Only called while no worker runs.
    fn remove_members(&mut self) -> Result<()> {
        self.current = None;
        self.dirty.clear();
        for m in self.layout.members() {
            fsx::remove_if_exists(&self.member_path(m.fid))?;
        }
        Ok(())
    }

    fn begin(&mut self, start: u64) -> Result<()> {
        // Every upload request is a point to try a stopped delivery again.
        self.stalled = None;
        if start == 0 && self.ledger.verified() > 0 {
            self.restart()?;
        }
        self.reader = Some(BundleReader::new(self.layout.clone(), start)?);
        // The log exists from the start of the upload, before any member is placed.
        if !self.delivery.is_cancelled() {
            if let Err(e) = self.ensure_parked() {
                self.delivery_failed(e)?;
            }
        }
        // Members that became durable before a restart or a pause go out now.
        self.feed()
    }

    /// Starts the stream again at 0 without placing any member twice (see the type doc).
    fn restart(&mut self) -> Result<()> {
        if let Err(e) = self.stop_worker(false) {
            if matches!(e, Error::Sink(_)) {
                self.fail()?;
                return Err(e);
            }
            // Any other error: the log on disk is read again just below.
        }
        // Finishes a member a stopped worker left half placed before its staging file is
        // deleted, and keeps the record of every member already delivered.
        self.ensure_parked()?;
        self.remove_members()?;
        self.ledger.reset()
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

    /// Makes the verified offset durable, then hands every member it completed to the
    /// delivery worker.
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
        self.ledger.persist()?;
        self.feed()
    }

    fn fail(&mut self) -> Result<()> {
        // Nothing more is handed over. Members already placed stay where they are: their
        // bytes were chunk-verified, and landed files are never discarded. The failed
        // record still lists them, so the transfer reports where they went.
        let _ = self.stop_worker(false);
        let done = match self.parked.take() {
            Some((_, done)) => done,
            None => DeliveryLog::read(&self.staging)
                .ok()
                .flatten()
                .map(|r| r.done)
                .unwrap_or_default(),
        };
        self.ledger.saved = self
            .layout
            .members()
            .iter()
            .filter(|m| !self.is_skipped(m.fid))
            .filter_map(|m| done.get(&m.fid).cloned())
            .collect();
        self.remove_members()?;
        fsx::remove_if_exists(&DeliveryLog::path(&self.staging))?;
        self.ledger.finish(FileState::Failed, None)
    }

    fn finalize(&mut self, claimed: &ChunkHash) -> Result<Vec<SavedFile>> {
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
        self.checkpoint()?;
        if lock(&self.ledger.progress).root != Some(*claimed) {
            // The root is durable before the last members are handed over, so a kill or an
            // I/O error from here on finishes delivery from disk without the sender.
            lock(&self.ledger.progress).root = Some(*claimed);
            self.ledger.persist()?;
        }
        self.deliver_all(*claimed)
    }

    /// Delivers what a kill or an I/O error left waiting in a bundle whose root verified.
    fn retry_pending(&mut self) -> Result<Vec<SavedFile>> {
        let root = self
            .pending_root()
            .ok_or_else(|| Error::BadRequest("bundle root not verified".into()))?;
        self.deliver_all(root)
    }

    /// Reopens an unfinished bundle after a restart (only while the transfer is still
    /// `resumable` does delivery go on). Returns the sink's message when the sink failed
    /// the resumed delivery.
    fn restore(&mut self, resumable: bool) -> Result<Option<String>> {
        let rec = DeliveryLog::read(&self.staging)?;
        if let Some(v1_root) = rec.as_ref().and_then(|r| r.root) {
            // A Phase 7 log was written only after the root verified (see `DeliveryLog`).
            let mut p = lock(&self.ledger.progress);
            if p.root.is_none() {
                p.root = Some(v1_root);
            }
        }
        let done: HashSet<u32> = if resumable {
            match rec {
                Some(rec) => {
                    let parked = self.recover(rec)?;
                    let done: HashSet<u32> = parked.1.keys().copied().collect();
                    // Progress counts members placed before the restart too.
                    self.delivery
                        .done_count
                        .store(done.len(), Ordering::Release);
                    self.parked = Some(parked);
                    done
                }
                None => HashSet::new(),
            }
        } else {
            rec.map(|r| r.done.into_keys().collect())
                .unwrap_or_default()
        };
        if lock(&self.ledger.progress).root.is_some() && self.pending_root().is_none() {
            // A root that does not match the durable hashes cannot come from this module;
            // without a trusted root nothing is finished early.
            lock(&self.ledger.progress).root = None;
        }
        self.reconcile(&done)?;
        if !resumable {
            return Ok(None);
        }
        if let Some(root) = self.pending_root() {
            tracing::info!(
                delivered = done.len(),
                "resuming delivery of a verified bundle"
            );
            match self.deliver_all(root) {
                Ok(_) => {}
                Err(Error::Sink(why)) => return Ok(Some(why)),
                Err(e) => {
                    tracing::warn!("bundle delivery still fails after a restart; it is tried again at the next upload request");
                    tracing::debug!(error = %e, "bundle delivery error");
                }
            }
        }
        Ok(None)
    }

    /// Finishes what a stopped worker left half done, from the log on disk.
    ///
    /// `Directory` mode: a logged member whose staging file is still there was about to
    /// be renamed onto its recorded placeholder; the rename is done now, or the member is
    /// placed afresh when a power loss lost the placeholder. `Staged` mode: the sink
    /// already has every logged member, so leftover staging files are deleted once the
    /// log is durable.
    fn recover(&self, rec: DeliveryRecord) -> Result<(DeliveryLog, HashMap<u32, SavedFile>)> {
        let mut log = DeliveryLog::open(&DeliveryLog::path(&self.staging), rec.valid_len)?;
        let mut done = rec.done;
        done.retain(|fid, _| self.layout.member(*fid).is_some() && !self.is_skipped(*fid));
        let mut fids: Vec<u32> = done.keys().copied().collect();
        fids.sort_unstable();
        match &self.dest {
            Dest::Directory(dest_root) => {
                let mut placer: Option<Placer> = None;
                for fid in fids {
                    if !exists(&self.member_path(fid))? {
                        continue;
                    }
                    if placer.is_none() {
                        placer = Some(Placer::open(&self.staging, dest_root)?);
                    }
                    let placer = placer.as_mut().expect("opened above");
                    let entry = self
                        .files
                        .get(fid as usize)
                        .ok_or_else(|| Error::BadRequest(format!("unknown bundle member {fid}")))?;
                    let (name, parents) = split_path(&entry.path)?;
                    placer.ensure_dir(parents)?;
                    let dir = &placer.dirs[parents];
                    let recorded = &done[&fid].path;
                    // Between this check and the rename below another local process could
                    // write into the empty placeholder; the rename would then replace that
                    // write. Only a process of the same user can do that, inside a folder
                    // this host just reserved a name in; `renameat2(RENAME_NOREPLACE)`
                    // would close it. A placeholder that already holds data means the
                    // rename happened (`holds_data`); in the rare case that someone else
                    // filled our empty placeholder, the staged copy is dropped instead.
                    let reserved = reserved_placeholder(dir, parents, recorded)?;
                    let target = if let Some(reserved) = reserved {
                        reserved
                    } else if holds_data(dir, parents, recorded)? {
                        // The rename onto the placeholder happened before the stream was
                        // restarted at 0; this staging file holds bytes that came again.
                        // Renaming would overwrite (SR-18) and placing afresh would
                        // duplicate, so the copy is dropped and the first place stands.
                        fsx::remove_if_exists(&self.member_path(fid))?;
                        continue;
                    } else {
                        // The placeholder is gone (a power loss lost it): place afresh.
                        let (final_name, placeholder) = fsx::create_unique(dir, name)?;
                        drop(placeholder);
                        let saved = SavedFile::placed(fid, relative_path(parents, &final_name));
                        if let Err(e) = log.append(&saved) {
                            let _ = dir.remove_file(&final_name);
                            return Err(e);
                        }
                        done.insert(fid, saved);
                        final_name
                    };
                    placer
                        .staging
                        .rename(member_name(fid), dir, &target)
                        .map_err(fsx::map_io)?;
                }
                if let Some(placer) = placer.as_mut() {
                    placer.sync()?;
                }
                log.sync()?;
            }
            Dest::Sink { .. } => {
                log.sync()?;
                for fid in fids {
                    fsx::remove_if_exists(&self.member_path(fid))?;
                }
            }
        }
        Ok((log, done))
    }

    /// Makes sure the delivery log and its record of delivered members are at hand while
    /// no worker runs: from memory, else from disk (finishing what a stopped worker left
    /// half done), else a fresh log.
    fn ensure_parked(&mut self) -> Result<()> {
        if self.delivery.is_cancelled() {
            return Err(cancelled_error());
        }
        if self.parked.is_some() || self.worker.is_some() {
            return Ok(());
        }
        let parked = match DeliveryLog::read(&self.staging)? {
            Some(rec) => self.recover(rec)?,
            None => (DeliveryLog::create(&self.staging)?, HashMap::new()),
        };
        self.parked = Some(parked);
        Ok(())
    }

    fn start_worker(&mut self) -> Result<()> {
        #[cfg(test)]
        if self.killed.load(Ordering::Acquire) {
            return Err(simulated_kill());
        }
        self.ensure_parked()?;
        let (log, done) = self.parked.take().expect("ensured above");
        self.delivery
            .done_count
            .store(done.len(), Ordering::Release);
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let worker = DeliveryWorker {
            dest: self.dest.clone(),
            files: Arc::clone(&self.files),
            staging: self.staging.clone(),
            log,
            done,
            shared: self.delivery.clone(),
            stop: Arc::clone(&stop),
            failed: Arc::clone(&failed),
            #[cfg(test)]
            crash: self.crash,
            #[cfg(test)]
            killed: Arc::clone(&self.killed),
            #[cfg(test)]
            transient: self.transient.take(),
        };
        let (jobs, inbox) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("playdrop-deliver".into())
            .spawn(move || worker.run(&inbox))
            .map_err(Error::Io)?;
        self.worker = Some(WorkerHandle {
            jobs,
            stop,
            failed,
            thread,
        });
        self.next = 0;
        Ok(())
    }

    /// Stops the worker: with `drain`, after every member already given to it; without,
    /// after the member in flight. A clean stop parks the log and the record of delivered
    /// members; an error leaves them to be read again from disk.
    fn stop_worker(&mut self, drain: bool) -> Result<()> {
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        if !drain {
            worker.stop.store(true, Ordering::Release);
        }
        drop(worker.jobs);
        self.next = 0;
        let exit = worker
            .thread
            .join()
            .map_err(|_| Error::Io(io::Error::other("bundle delivery worker panicked")))?;
        exit.result?;
        self.parked = Some((exit.log, exit.done));
        Ok(())
    }

    /// A delivery error: a sink refusal fails the bundle (the transfer fails with its
    /// message); anything else stops delivery until the next retry point.
    fn delivery_failed(&mut self, e: Error) -> Result<()> {
        if matches!(e, Error::Sink(_)) {
            self.fail()?;
            return Err(e);
        }
        tracing::warn!("bundle member delivery stopped; it is tried again at the next upload request or restart");
        // The error text may carry a file name (SR-32).
        tracing::debug!(error = %e, "bundle delivery error");
        self.stalled = Some(e);
        Ok(())
    }

    /// Hands every member whose bytes are now chunk-verified and durable to the worker,
    /// starting it when needed.
    fn feed(&mut self) -> Result<()> {
        // After a cancel nothing is placed, and no log is created again in the staging
        // folder the cancel just cleared (an upload still in flight reaches checkpoints).
        if self.stalled.is_some() || self.delivery.is_cancelled() {
            return Ok(());
        }
        if self
            .worker
            .as_ref()
            .is_some_and(|w| w.failed.load(Ordering::Acquire))
        {
            if let Err(e) = self.stop_worker(false) {
                return self.delivery_failed(e);
            }
        }
        let durable = self.ledger.durable;
        let layout = Arc::clone(&self.layout);
        let members = layout.members();
        if members
            .get(self.next)
            .is_none_or(|m| m.data_end() > durable)
        {
            return Ok(());
        }
        if self.worker.is_none() {
            if let Err(e) = self.start_worker() {
                return self.delivery_failed(e);
            }
        }
        let worker = self.worker.as_ref().expect("started above");
        let mut next = self.next;
        while let Some(m) = members.get(next).filter(|m| m.data_end() <= durable) {
            if !self.skipped.get(m.fid as usize).copied().unwrap_or(false) {
                // A worker that already stopped on an error is found at the next feed or
                // stop; the member is then given to the next worker.
                let _ = worker.jobs.send(m.fid);
            }
            next += 1;
        }
        self.next = next;
        Ok(())
    }

    /// Hands every eligible member to a worker and waits until it has placed them all.
    /// A sink refusal fails the bundle.
    fn drain(&mut self) -> Result<()> {
        self.stalled = None;
        self.feed()?;
        if let Some(e) = self.stalled.take() {
            return Err(e);
        }
        if let Err(e) = self.stop_worker(true) {
            if matches!(e, Error::Sink(_)) {
                self.fail()?;
            }
            return Err(e);
        }
        Ok(())
    }

    /// Hands every remaining member over, waits for the worker to finish them, then
    /// records the bundle as verified with every destination. Only once the root verified.
    fn deliver_all(&mut self, root: ChunkHash) -> Result<Vec<SavedFile>> {
        if self.delivery.is_cancelled() {
            return Err(cancelled_error());
        }
        // `finish`, a restart request and load are retry points: a worker that failed
        // earlier (or while draining) gets one more try with a fresh worker, fresh folder
        // handles and the log read again from disk, before the error is reported.
        if let Err(first) = self.drain() {
            if matches!(first, Error::Sink(_)) {
                return Err(first);
            }
            tracing::debug!(error = %first, "bundle delivery failed; one more try with a fresh worker");
            self.parked = None;
            self.drain()?;
        }
        let mut saved = Vec::with_capacity(self.layout.members().len());
        {
            let empty = HashMap::new();
            let done = self.parked.as_ref().map_or(&empty, |p| &p.1);
            for m in self.layout.members() {
                if self.is_skipped(m.fid) {
                    continue;
                }
                match done.get(&m.fid) {
                    Some(s) => saved.push(s.clone()),
                    None => {
                        // Only a cancel stops a draining worker early.
                        return Err(Error::Io(io::Error::other(
                            "bundle delivery stopped before every member was placed",
                        )));
                    }
                }
            }
        }
        // The log stays until the transfer settles and clears the staging folder: once
        // this record is written it is no longer read.
        self.ledger.saved.clone_from(&saved);
        self.ledger.finish(FileState::Verified, Some(root))?;
        Ok(saved)
    }
}

/// The error of delivery work asked for after a cancel.
fn cancelled_error() -> Error {
    Error::Io(io::Error::other("the transfer was cancelled"))
}

/// True when `path` exists (without following a symlink at it).
fn exists(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// True when `name` in `dir` is a regular file of exactly `size` bytes (not following a
/// symlink).
fn has_file_of_size(dir: &cap_std::fs::Dir, name: &str, size: u64) -> Result<bool> {
    match dir.symlink_metadata(name) {
        Ok(meta) => Ok(meta.is_file() && meta.len() == size),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// True when the file a delivery log line names in `dir` is a regular file with data: the
/// staged member was already renamed onto its placeholder.
fn holds_data(dir: &cap_std::fs::Dir, parents: &[String], recorded: &Path) -> Result<bool> {
    let Some(name) = recorded.file_name().and_then(|n| n.to_str()) else {
        return Ok(false);
    };
    if relative_path(parents, name) != recorded {
        return Ok(false);
    }
    match dir.symlink_metadata(name) {
        Ok(meta) => Ok(meta.is_file() && meta.len() > 0),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// The name of the placeholder a delivery log line reserved in `dir`, when it is still
/// there as an empty regular file (so renaming onto it overwrites nothing, SR-18).
fn reserved_placeholder(
    dir: &cap_std::fs::Dir,
    parents: &[String],
    recorded: &Path,
) -> Result<Option<String>> {
    let Some(name) = recorded.file_name().and_then(|n| n.to_str()) else {
        return Ok(None);
    };
    if relative_path(parents, name) != recorded {
        return Ok(None);
    }
    match dir.symlink_metadata(name) {
        Ok(meta) if meta.is_file() && meta.len() == 0 => Ok(Some(name.to_owned())),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
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
    /// Final destination of each fid (relative to the receive root, or the sink's
    /// location), once placed.
    saved: StdMutex<Vec<Option<SavedFile>>>,
    /// `Staged` mode binding; `None` renames beneath `root`.
    sink: Option<SinkBinding>,
    /// Why the transfer failed, when a sink said so (shown to the user).
    failure: StdMutex<Option<String>>,
    /// Serialises [`Shared::settle_blocking`]: concurrent `PUT`s that finish together
    /// must not both complete the transfer.
    settle_gate: StdMutex<()>,
    /// Shared with the bundle's delivery worker: members delivered so far, and the cancel
    /// gate.
    delivery: DeliveryShared,
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
            discard_if_finished(&staging)?;
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
            Self::build(
                root, staging, record, free_space, opts.sync, opts.sink, false,
            )
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
        Self::load_with(root, id, free_space, policy, None).await
    }

    /// [`load`](Self::load) for a receiver in `Staged` mode: `sink` gets the files that
    /// verify from now on.
    pub async fn load_with(
        root: &Path,
        id: &str,
        free_space: Arc<dyn FreeSpace>,
        policy: SyncPolicy,
        sink: Option<SinkBinding>,
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
            Self::build(root, staging, record, free_space, policy, sink, true)
        })
        .await
        .map_err(join_err)?
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass over the manifest, read top to bottom"
    )]
    fn build(
        root: PathBuf,
        staging: PathBuf,
        rec: TransferRecord,
        free_space: Arc<dyn FreeSpace>,
        policy: SyncPolicy,
        sink: Option<SinkBinding>,
        from_disk: bool,
    ) -> Result<Self> {
        let m = &rec.manifest;
        let cs = rec.chunk_size;
        let mask = skip_mask(&rec.skip, m.files.len());
        let mut resume_failure: Option<String> = None;
        let resumable = matches!(rec.state, TransferState::Accepted | TransferState::Active);
        let dest = Dest::new(&root, sink.as_ref(), &m.id);
        let delivery = DeliveryShared::new();
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
                let mut part = if from_disk {
                    PartFile::open_blocking(&staging, f.fid, f.size, cs, policy)?
                } else {
                    PartFile::fresh(&staging, f.fid, f.size, cs, policy)
                };
                if from_disk && resumable {
                    if let Some(why) = part.resume_placement(&dest, f) {
                        resume_failure.get_or_insert(why);
                    }
                }
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
                let mut inner = BundleInner::new(
                    ledger,
                    layout.clone(),
                    staging.clone(),
                    mask.clone(),
                    dest.clone(),
                    Arc::new(m.files.clone()),
                    delivery.clone(),
                );
                if from_disk
                    && !matches!(
                        lock(&progress).state,
                        FileState::Verified | FileState::Failed
                    )
                {
                    if let Some(why) = inner.restore(resumable)? {
                        resume_failure.get_or_insert(why);
                    }
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
                sink,
                failure: StdMutex::new(resume_failure),
                settle_gate: StdMutex::new(()),
                delivery,
            }),
        };
        if resumable {
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
            .filter(|s| s.location.is_none())
            .map(|s| self.shared.root.join(&s.path))
            .collect()
    }

    /// Where each file placed so far went, in manifest order (`docs/core-api.md` section
    /// 2): the final path in `Directory` mode, the sink's string in `Staged` mode.
    pub fn delivered(&self) -> Vec<DeliveredFile> {
        let s = &self.shared;
        lock(&s.saved)
            .iter()
            .flatten()
            .map(|f| DeliveredFile {
                path: s
                    .manifest
                    .files
                    .get(f.fid as usize)
                    .map(|e| e.path.join("/"))
                    .unwrap_or_default(),
                location: f
                    .location
                    .clone()
                    .unwrap_or_else(|| s.root.join(&f.path).display().to_string()),
            })
            .collect()
    }

    /// True in `Staged` mode (files go to a sink).
    pub fn is_staged(&self) -> bool {
        self.shared.sink.is_some()
    }

    /// Files that reached their final place so far, for progress: streamed files that are
    /// verified (placed) or skipped, bundle members already placed or handed to the sink
    /// (they go out while the bundle streams), and zero-length files once created.
    pub fn files_done(&self) -> usize {
        let s = &self.shared;
        let bundle_verified = s
            .bundle
            .as_ref()
            .is_some_and(|b| lock(&b.progress).state == FileState::Verified);
        let saved = lock(&s.saved);
        let mut n = 0;
        for (f, slot) in s.manifest.files.iter().zip(&s.files) {
            let done = if s.is_skipped(f.fid) {
                true
            } else if f.in_bundle {
                bundle_verified
            } else if f.size == 0 {
                saved.get(f.fid as usize).is_some_and(Option::is_some)
            } else {
                lock(&slot.progress).state == FileState::Verified
            };
            n += usize::from(done);
        }
        if !bundle_verified {
            n += s.delivery.done_count.load(Ordering::Acquire);
        }
        n
    }

    /// The sink's message when it made the transfer fail.
    pub fn failure(&self) -> Option<String> {
        lock(&self.shared.failure).clone()
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
        if start == 0 && part.placement_pending() {
            // Every byte is here and the root verified; only the placement failed. Finish
            // it instead of downloading the file again (see `PartInner::place`).
            let dest = s.dest();
            let entry = entry.clone();
            let res = part.run(move |p| p.retry_placement(&dest, &entry)).await;
            drop(guard);
            return Err(self.placement_retried(res, entry_size(s, fid)).await);
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
        let retry = start == 0 && inner.pending_root().is_some();
        let mut owned = std::mem::replace(inner, Box::new(placeholder_bundle()));
        let (back, res) = tokio::task::spawn_blocking(move || {
            // Every byte is here and the root verified; only delivery stopped. Finish it
            // instead of downloading the bundle again (see `BundleInner`).
            let r = if retry {
                owned.retry_pending().map(Some)
            } else {
                owned.begin(start).map(|()| None)
            };
            (owned, r)
        })
        .await
        .map_err(join_err)?;
        *inner = back;
        if retry {
            drop(guard);
            let res = res.map(Option::unwrap_or_default);
            return Err(self.placement_retried(res, layout.total()).await);
        }
        if let Err(e @ Error::Sink(_)) = res {
            // The sink refused a member handed over before this request; the bundle failed.
            drop(guard);
            return Err(self.sink_failed(e).await);
        }
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

    /// Reply to a restart request that finished a waiting placement instead. Success, or
    /// an error that is not the sink's, answers `offset_mismatch` at the full size: the
    /// sender asks for the state again and finds the file verified (or tries again later,
    /// bounded by its own stall limit). A sink refusal fails the transfer with its text.
    async fn placement_retried(&self, res: Result<Vec<SavedFile>>, size: u64) -> PutError {
        match res {
            Ok(saved) => self.shared.record_saved(&saved),
            Err(e @ Error::Sink(_)) => return self.sink_failed(e).await,
            Err(e) => {
                tracing::warn!(
                    "placing verified data failed again; it is tried at the next upload request"
                );
                tracing::debug!(error = %e, "placement error");
            }
        }
        if let Err(e) = self.settle().await {
            return e.into();
        }
        Error::OffsetMismatch { expected: size }.into()
    }

    /// Records a sink refusal as the transfer's failure and settles it.
    async fn sink_failed(&self, e: Error) -> PutError {
        if let Error::Sink(why) = &e {
            self.shared.set_failure(why.clone());
        }
        if let Err(settle) = self.settle().await {
            return settle.into();
        }
        e.into()
    }

    async fn settle(&self) -> Result<()> {
        let shared = self.shared.clone();
        tokio::task::spawn_blocking(move || shared.settle_blocking())
            .await
            .map_err(join_err)?
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
            // Waits for a bundle member in flight, then lets the delivery worker place
            // nothing more. Members already placed stay.
            shared.delivery.cancel();
            remove_staging_contents(&shared.staging)?;
            shared.write_record(TransferState::Cancelled)
        })
        .await
        .map_err(join_err)?
    }
}

fn entry_size(s: &Shared, fid: u32) -> u64 {
    s.manifest.files.get(fid as usize).map_or(0, |f| f.size)
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
    BundleInner::new(
        Ledger::fresh(Path::new(""), "", 0, 1, SyncPolicy::default()),
        layout,
        PathBuf::new(),
        Vec::new(),
        Dest::Directory(PathBuf::new()),
        Arc::new(Vec::new()),
        DeliveryShared::new(),
    )
}

/// Destinations recorded in the ledgers of a freshly built (or reloaded) transfer, by fid.
fn saved_from_ledgers(files: &[Slot], bundle: Option<&Slot>) -> Vec<Option<SavedFile>> {
    let mut saved: Vec<Option<SavedFile>> = vec![None; files.len()];
    let mut restore = |list: &[SavedFile]| {
        for f in list {
            if let Some(slot) = saved.get_mut(f.fid as usize) {
                *slot = Some(f.clone());
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
/// Removes the staging folder of an earlier transfer with the same ID if that transfer
/// ended for good (declined, cancelled, failed) or its record is unreadable. `cancel` and
/// failures keep the folder with a terminal record, and an outbox pull offers the same ID
/// again on every retry (protocol.md 7.8), so without this one failed pull would refuse
/// every later one with "transfer id already known". A folder whose transfer can still
/// resume (pending, accepted, active) or that completed is left alone; `accept` then
/// refuses it as before.
fn discard_if_finished(staging: &Path) -> Result<()> {
    let bytes = match std::fs::read(staging.join("transfer.json")) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // No record: either no folder at all, or a folder left half-made by a crash
            // between `create_dir` and the first record write.
            return match std::fs::symlink_metadata(staging) {
                Ok(m) if m.is_dir() => Ok(std::fs::remove_dir_all(staging)?),
                Ok(_) => Err(Error::BadRequest(
                    "transfer id already known; use load".into(),
                )),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        Err(e) => return Err(e.into()),
    };
    let finished = match serde_json::from_slice::<TransferRecord>(&bytes) {
        Ok(r) => matches!(
            r.state,
            TransferState::Declined | TransferState::Cancelled | TransferState::Failed
        ),
        Err(_) => true,
    };
    if finished {
        std::fs::remove_dir_all(staging)?;
    }
    Ok(())
}

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
                *slot = Some(f.clone());
            }
        }
    }

    /// Where verified files go.
    fn dest(&self) -> Dest {
        Dest::new(&self.root, self.sink.as_ref(), &self.manifest.id)
    }

    /// Remembers why the transfer failed (the first reason wins).
    fn set_failure(&self, why: String) {
        let mut f = lock(&self.failure);
        if f.is_none() {
            *f = Some(why);
        }
    }

    /// Completion work once every stream verified (protocol.md 7.2): empty folders and
    /// zero-length files, made beneath the root or handed to the sink.
    fn complete_blocking(&self) -> Result<()> {
        let dest = self.dest();
        match &dest {
            Dest::Directory(_) => {
                let root = fsx::open_root(&self.root)?;
                for d in &self.manifest.dirs {
                    fsx::safe_dir(&root, d, false)?;
                }
                for f in &self.manifest.files {
                    if f.size == 0 && !f.in_bundle && !self.is_skipped(f.fid) {
                        let (name, dirs) = split_path(&f.path)?;
                        let dir = fsx::safe_dir(&root, dirs, false)?;
                        let (final_name, _) = fsx::create_unique(&dir, name)?;
                        self.record_saved(&[SavedFile::placed(
                            f.fid,
                            relative_path(dirs, &final_name),
                        )]);
                    }
                }
            }
            Dest::Sink { binding, .. } => {
                for d in &self.manifest.dirs {
                    binding
                        .sink
                        .make_dir(d.clone())
                        .map_err(|e| Error::Sink(e.message))?;
                }
                for f in &self.manifest.files {
                    if f.size == 0 && !f.in_bundle && !self.is_skipped(f.fid) {
                        let staged = self.staging.join(format!("{}.empty", f.fid));
                        drop(fsx::open_or_create(&staged)?);
                        let saved = dest.put(&staged, f)?;
                        self.record_saved(&[saved]);
                    }
                }
            }
        }
        Ok(())
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
            match self.complete_blocking() {
                Ok(()) => TransferState::Complete,
                Err(Error::Sink(why)) => {
                    self.set_failure(why);
                    TransferState::Failed
                }
                Err(e) => return Err(e),
            }
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
        let outcome = if !failed && !matches!(outcome, Err(PutError::Cancelled)) {
            match self.checkpoint().await {
                Ok(()) => outcome,
                Err(e) => Err(e.into()),
            }
        } else {
            outcome
        };
        if let Err(PutError::Core(Error::Sink(why))) = &outcome {
            // The sink refused a bundle member handed over while streaming; the bundle
            // failed and the transfer fails with the sink's text.
            self.receiver.shared.set_failure(why.clone());
            self.settle().await?;
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
        let dest = self.receiver.shared.dest();
        let result = match self.target {
            Target::File(fid) => {
                let entry = self.receiver.shared.manifest.files[fid as usize].clone();
                self.blocking(move |p| {
                    p.finalize(&claimed, &dest, &entry)?;
                    Ok(p.ledger.saved.clone())
                })
                .await
            }
            Target::Bundle => {
                // On a failure the bundle's record still lists the members that stayed
                // where they were placed.
                let out = self
                    .bundle_blocking(move |b| Ok((b.finalize(&claimed), b.ledger.saved.clone())))
                    .await;
                match out {
                    Ok((r, kept)) => {
                        if r.is_err() {
                            self.receiver.shared.record_saved(&kept);
                        }
                        r
                    }
                    Err(e) => Err(e),
                }
            }
        };
        match &result {
            Ok(saved) => self.receiver.shared.record_saved(saved),
            Err(Error::Sink(why)) => self.receiver.shared.set_failure(why.clone()),
            Err(_) => {}
        }
        self.settle().await?;
        if let Err(e) = result {
            let p = lock(&self.slot().progress).clone();
            if !matches!(e, Error::Sink(_))
                && p.state == FileState::Receiving
                && p.verified_offset == size
                && p.root.is_some()
            {
                // Every byte is here and the root verified; only placing failed. Saying
                // where this side stands makes the sender ask for the state again and
                // restart at 0, which retries the placement from staging (see
                // `TransferReceiver::begin_bundle`) instead of downloading anything again.
                tracing::warn!(
                    "placing verified data failed; it is tried again at the next upload request"
                );
                tracing::debug!(error = %e, "placement error");
                return Err(Error::OffsetMismatch { expected: size }.into());
            }
            return Err(e.into());
        }
        Ok(self.response())
    }

    async fn settle(&self) -> Result<()> {
        self.receiver.settle().await
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
        // The verified file stays in staging and the sender is told to re-plan; the
        // refusal of the symlinked folder holds at every retry.
        assert!(
            matches!(r, Err(PutError::Core(Error::OffsetMismatch { expected })) if expected == u64::from(CS)),
            "{r:?}"
        );
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        let r = rx.begin_file(0, &tok, 0, u64::from(CS) - 1).await;
        assert!(matches!(
            r,
            Err(PutError::Core(Error::OffsetMismatch { .. }))
        ));
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }

    /// A sink that keeps what it was given, for `Staged` mode tests.
    #[derive(Debug, Default)]
    struct MemSink {
        got: StdMutex<Vec<(DeliverItem, Vec<u8>)>>,
        dirs: StdMutex<Vec<Vec<String>>>,
        refuse: bool,
    }

    impl ReceiveSink for MemSink {
        fn deliver(
            &self,
            item: DeliverItem,
            mut file: File,
        ) -> std::result::Result<String, crate::transfer::SinkError> {
            if self.refuse {
                return Err(crate::transfer::SinkError::new(
                    "storage permission revoked",
                ));
            }
            assert_eq!(
                file.stream_position().unwrap(),
                0,
                "handed over at offset 0"
            );
            let mut v = Vec::new();
            file.read_to_end(&mut v).unwrap();
            let loc = format!("content://test/{}", item.path.join("/"));
            lock(&self.got).push((item, v));
            Ok(loc)
        }

        fn make_dir(
            &self,
            path: Vec<String>,
        ) -> std::result::Result<(), crate::transfer::SinkError> {
            lock(&self.dirs).push(path);
            Ok(())
        }
    }

    async fn bundle_of(m: &Manifest, contents: &[Vec<u8>]) -> Vec<u8> {
        use tokio::io::AsyncReadExt;
        #[derive(Debug)]
        struct Mem(Vec<Vec<u8>>);
        impl crate::transfer::MemberOpener for Mem {
            fn open(&self, fid: u32, off: u64) -> crate::transfer::bundle::OpenFuture {
                let d = self.0[fid as usize][usize::try_from(off).unwrap()..].to_vec();
                Box::pin(async move {
                    Ok(Box::new(std::io::Cursor::new(d)) as crate::transfer::bundle::MemberReader)
                })
            }
        }
        let layout = Arc::new(BundleLayout::new(m).unwrap());
        let mut w = crate::transfer::BundleWriter::new(layout, Arc::new(Mem(contents.to_vec())), 0)
            .unwrap();
        let mut v = Vec::new();
        w.read_to_end(&mut v).await.unwrap();
        v
    }

    /// `Staged` mode (`docs/core-api.md` section 2): every verified file, empty file and
    /// bundle member reaches the sink once, at offset 0; staging files are deleted; the
    /// staging root holds no final files; `delivered` lists the sink's strings.
    #[tokio::test]
    async fn staged_mode_hands_every_file_to_the_sink() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = tmp.path().join("staging");
        let (m, contents) = manifest(&[
            (2 * CS as usize + 5, false),
            (900, true),
            (0, false),
            (40, true),
        ]);
        let sink = Arc::new(MemSink::default());
        let opts = AcceptOptions {
            sink: Some(SinkBinding {
                sink: sink.clone(),
                peer: "ab".repeat(32),
            }),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(&staging, m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        assert!(rx.is_staged());
        let st = rx.state();
        let tok0 = st.files[0].token.clone().unwrap();
        let btok = st.bundle.as_ref().unwrap().token.clone().unwrap();
        let size0 = contents[0].len() as u64;
        let mut up = rx.begin_file(0, &tok0, 0, size0 - 1).await.unwrap();
        up.receive(&body(&contents[0], 0, size0).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&contents[0]))).await.unwrap();
        let bundle = bundle_of(&m, &contents).await;
        let bsize = bundle.len() as u64;
        let mut up = rx.begin_bundle(&btok, 0, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, 0, bsize).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&bundle))).await.unwrap();

        assert_eq!(rx.transfer_state(), TransferState::Complete);
        let got = lock(&sink.got);
        assert_eq!(got.len(), 4, "streamed, two members and the empty file");
        for (item, bytes) in got.iter() {
            let fid: usize = item.path[1][1..2].parse().unwrap();
            assert_eq!(bytes, &contents[fid], "{:?}", item.path);
            assert_eq!(item.size, contents[fid].len() as u64);
            assert_eq!(item.transfer_id, m.id);
            assert_eq!(item.peer, "ab".repeat(32));
            assert_eq!(item.mtime_ms, 1_700_000_000_000);
        }
        assert_eq!(
            *lock(&sink.dirs),
            vec![vec!["T".to_owned(), "empty".to_owned()]]
        );
        assert!(rx.saved_paths().is_empty(), "no paths in Staged mode");
        let mut delivered = rx.delivered();
        delivered.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(delivered.len(), 4);
        assert_eq!(delivered[0].path, "T/f0.dat");
        assert_eq!(delivered[0].location, "content://test/T/f0.dat");
        // Nothing but the staging folder is left under the staging root.
        let left: Vec<_> = std::fs::read_dir(&staging)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, [std::ffi::OsString::from(STAGING_DIR)]);
        let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
    }

    /// A sink that keeps every delivered file open, like a host that copies lazily.
    #[derive(Debug, Default)]
    struct KeepingSink {
        kept: StdMutex<Vec<File>>,
    }

    impl ReceiveSink for KeepingSink {
        fn deliver(
            &self,
            item: DeliverItem,
            file: File,
        ) -> std::result::Result<String, crate::transfer::SinkError> {
            lock(&self.kept).push(file);
            Ok(item.path.join("/"))
        }

        fn make_dir(
            &self,
            _path: Vec<String>,
        ) -> std::result::Result<(), crate::transfer::SinkError> {
            Ok(())
        }
    }

    /// A sink that still holds the delivered handles does not stop the core from deleting
    /// the staged files and completing (on Windows this needs `FILE_SHARE_DELETE`), and
    /// the kept handles still read the verified bytes.
    #[tokio::test]
    async fn sink_keeping_files_open_does_not_block_cleanup() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, contents) = manifest(&[(CS as usize + 7, false), (300, true), (50, true)]);
        let sink = Arc::new(KeepingSink::default());
        let opts = AcceptOptions {
            sink: Some(SinkBinding {
                sink: sink.clone(),
                peer: "cd".repeat(32),
            }),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(tmp.path(), m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let st = rx.state();
        let tok0 = st.files[0].token.clone().unwrap();
        let btok = st.bundle.as_ref().unwrap().token.clone().unwrap();
        let size0 = contents[0].len() as u64;
        let mut up = rx.begin_file(0, &tok0, 0, size0 - 1).await.unwrap();
        up.receive(&body(&contents[0], 0, size0).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&contents[0]))).await.unwrap();
        let bundle = bundle_of(&m, &contents).await;
        let bsize = bundle.len() as u64;
        let mut up = rx.begin_bundle(&btok, 0, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, 0, bsize).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&bundle))).await.unwrap();

        assert_eq!(rx.transfer_state(), TransferState::Complete);
        let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        let mut kept = lock(&sink.kept);
        assert_eq!(kept.len(), 3);
        let mut lengths: Vec<usize> = kept
            .iter_mut()
            .map(|f| {
                let mut v = Vec::new();
                f.read_to_end(&mut v).unwrap();
                v.len()
            })
            .collect();
        lengths.sort_unstable();
        assert_eq!(lengths, [50, 300, CS as usize + 7]);
    }

    /// A sink error fails the file and the transfer with the sink's message.
    #[tokio::test]
    async fn sink_error_fails_the_transfer_with_its_message() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, contents) = manifest(&[(3000, false)]);
        let sink = Arc::new(MemSink {
            refuse: true,
            ..MemSink::default()
        });
        let opts = AcceptOptions {
            sink: Some(SinkBinding {
                sink,
                peer: "cd".repeat(32),
            }),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(tmp.path(), m, FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let tok = rx.state().files[0].token.clone().unwrap();
        let mut up = rx.begin_file(0, &tok, 0, 2999).await.unwrap();
        up.receive(&body(&contents[0], 0, 3000).await[..])
            .await
            .unwrap();
        let r = up.finish(Some(root_of(&contents[0]))).await;
        assert!(
            matches!(r, Err(PutError::Core(Error::Sink(ref m))) if m == "storage permission revoked")
        );
        assert_eq!(rx.transfer_state(), TransferState::Failed);
        assert_eq!(rx.failure().as_deref(), Some("storage permission revoked"));
        assert!(!rx.staging_dir().join("0.part").exists());
    }

    #[tokio::test]
    async fn same_id_is_accepted_again_after_a_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest_for_reuse();
        let rx = TransferReceiver::accept(
            tmp.path(),
            m.clone(),
            FakeSpace::plenty(),
            AcceptOptions::default(),
        )
        .await
        .unwrap();
        rx.cancel().await.unwrap();
        // An outbox pull retries with the same transfer ID (protocol.md 7.8).
        let again = TransferReceiver::accept(
            tmp.path(),
            m.clone(),
            FakeSpace::plenty(),
            AcceptOptions::default(),
        )
        .await
        .expect("a cancelled transfer must not block its ID");
        assert_eq!(again.transfer_state(), TransferState::Accepted);
    }

    #[tokio::test]
    async fn same_id_is_still_refused_while_resumable() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest_for_reuse();
        let _rx = TransferReceiver::accept(
            tmp.path(),
            m.clone(),
            FakeSpace::plenty(),
            AcceptOptions::default(),
        )
        .await
        .unwrap();
        let err =
            TransferReceiver::accept(tmp.path(), m, FakeSpace::plenty(), AcceptOptions::default())
                .await
                .expect_err("an accepted transfer is resumed with load, not accepted twice");
        assert!(matches!(err, Error::BadRequest(_)), "{err:?}");
    }

    #[tokio::test]
    async fn half_made_staging_without_a_record_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let m = manifest_for_reuse();
        std::fs::create_dir_all(tmp.path().join(STAGING_DIR).join(&m.id)).unwrap();
        TransferReceiver::accept(tmp.path(), m, FakeSpace::plenty(), AcceptOptions::default())
            .await
            .expect("a folder with no record is left over from a crash");
    }

    /// A one-file manifest with a fixed ID, for the ID reuse tests.
    fn manifest_for_reuse() -> Manifest {
        manifest(&[(1000, false)]).0
    }

    /// A bundle-only manifest of five members (one empty) for the delivery kill tests.
    fn bundle_members() -> (Manifest, Vec<Vec<u8>>) {
        manifest(&[
            (300, true),
            (0, true),
            (70_000, true),
            (5, true),
            (1000, true),
        ])
    }

    /// Arms a kill point in the bundle's member delivery.
    fn set_crash(rx: &TransferReceiver, crash: Crash) {
        let slot = rx.shared.bundle.as_ref().expect("manifest has a bundle");
        let mut io = slot.io.try_lock().expect("no upload running");
        let SlotIo::Bundle(inner) = &mut *io else {
            panic!("bundle slot holds the bundle");
        };
        inner.crash = Some(crash);
    }

    /// Receives the whole bundle of `m` under `root`, "kills" the process at `crash` while
    /// the verified members are handed over, then reopens the transfer from disk the way a
    /// restarted host does. Nothing is sent again after the reopen.
    async fn receive_kill_reload(
        root: &Path,
        m: &Manifest,
        contents: &[Vec<u8>],
        sink: Option<SinkBinding>,
        crash: Crash,
    ) -> TransferReceiver {
        let opts = AcceptOptions {
            sink: sink.clone(),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(root, m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let btok = rx.state().bundle.unwrap().token.unwrap();
        let bundle = bundle_of(m, contents).await;
        let bsize = bundle.len() as u64;
        set_crash(&rx, crash);
        let mut up = rx.begin_bundle(&btok, 0, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, 0, bsize).await[..])
            .await
            .unwrap();
        let r = up.finish(Some(root_of(&bundle))).await;
        // The kill stopped placement after every byte verified: the reply sends the
        // sender back to planning (409 at the full size) instead of failing.
        assert!(
            matches!(r, Err(PutError::Core(Error::OffsetMismatch { expected })) if expected == bsize),
            "{r:?}"
        );
        assert!(kill_fired(&rx), "{crash:?}");
        assert_eq!(rx.transfer_state(), TransferState::Active);
        assert!(
            rx.staging_dir().join(DELIVERY_LOG).exists(),
            "the delivery log outlives the kill"
        );
        drop(rx);
        let rx = TransferReceiver::load_with(
            root,
            &m.id,
            FakeSpace::plenty(),
            SyncPolicy::default(),
            sink,
        )
        .await
        .unwrap();
        // Delivery finished during the reopen: no byte was asked for again.
        assert_eq!(rx.transfer_state(), TransferState::Complete);
        assert_eq!(rx.state().bundle.unwrap().state, FileState::Verified);
        rx
    }

    /// `Directory` mode: a kill after k of n members were renamed into place, or in the
    /// middle of member k (log line written, rename not done), resumes on reopen and
    /// leaves every member exactly once at its own name, with no " (1)" duplicates.
    #[tokio::test]
    async fn bundle_delivery_survives_a_kill_in_directory_mode() {
        let (_, sample) = bundle_members();
        let n = sample.len();
        let mut crashes: Vec<Crash> = [0, 1, n - 1].map(Crash::Before).to_vec();
        crashes.extend([0, 1, n - 1].map(Crash::Inside));
        for crash in crashes {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("Downloads");
            let (m, contents) = bundle_members();
            let rx = receive_kill_reload(&root, &m, &contents, None, crash).await;
            let t = root.join("T");
            let mut names: Vec<String> = std::fs::read_dir(&t)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            let mut want: Vec<String> = (0..n).map(|i| format!("f{i}.dat")).collect();
            want.push("empty".to_owned());
            want.sort();
            assert_eq!(names, want, "{crash:?}: each member once, no duplicates");
            for (i, c) in contents.iter().enumerate() {
                assert_eq!(&std::fs::read(t.join(format!("f{i}.dat"))).unwrap(), c);
            }
            assert_eq!(
                rx.saved_paths(),
                (0..n)
                    .map(|i| t.join(format!("f{i}.dat")))
                    .collect::<Vec<_>>(),
                "{crash:?}: every member listed once, in order"
            );
            let delivered = rx.delivered();
            assert_eq!(delivered.len(), n, "{crash:?}");
            let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        }
    }

    /// `Staged` mode: a kill after k of n members reached the sink resumes on reopen and
    /// the sink sees every member exactly once. A kill after the sink took member k but
    /// before its log line hands that member over once more (documented on
    /// `BundleInner::hand_members`) and no other.
    #[tokio::test]
    async fn bundle_delivery_survives_a_kill_in_staged_mode() {
        let (_, sample) = bundle_members();
        let n = sample.len();
        let mut crashes: Vec<Crash> = [0, 1, n - 1].map(Crash::Before).to_vec();
        crashes.extend([0, 1, n - 1].map(Crash::Inside));
        for crash in crashes {
            let tmp = tempfile::tempdir().unwrap();
            let (m, contents) = bundle_members();
            let sink = Arc::new(MemSink::default());
            let binding = SinkBinding {
                sink: sink.clone(),
                peer: "ef".repeat(32),
            };
            let rx = receive_kill_reload(tmp.path(), &m, &contents, Some(binding), crash).await;
            let got = lock(&sink.got);
            let mut counts = vec![0usize; n];
            for (item, bytes) in got.iter() {
                let fid: usize = item.path[1][1..2].parse().unwrap();
                assert_eq!(bytes, &contents[fid], "{crash:?}: {:?}", item.path);
                counts[fid] += 1;
            }
            let mut want = vec![1usize; n];
            if let Crash::Inside(k) = crash {
                want[k] = 2;
            }
            assert_eq!(counts, want, "{crash:?}: deliveries per member");
            let mut paths: Vec<String> = rx.delivered().into_iter().map(|d| d.path).collect();
            assert_eq!(
                paths.len(),
                n,
                "{crash:?}: delivered lists every member once"
            );
            paths.sort();
            paths.dedup();
            assert_eq!(paths.len(), n, "{crash:?}");
            let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        }
    }

    /// A torn last line in the delivery log (power loss mid-append) is ignored and cut off
    /// before the next append. A version 1 log (Phase 7) is still read, with its root.
    #[test]
    fn delivery_log_drops_a_torn_tail() {
        let tmp = tempfile::tempdir().unwrap();
        let mut log = DeliveryLog::create(tmp.path()).unwrap();
        log.append(&SavedFile::placed(2, PathBuf::from("T/a.dat")))
            .unwrap();
        log.append(&SavedFile::delivered(3, "content://x".into()))
            .unwrap();
        drop(log);
        let path = DeliveryLog::path(tmp.path());
        let full = std::fs::read(&path).unwrap();
        std::fs::write(&path, [&full[..], b"{\"fid\":4,\"pa"].concat()).unwrap();
        let rec = DeliveryLog::read(tmp.path()).unwrap().unwrap();
        assert_eq!(rec.root, None, "version 2 keeps the root in the ledger");
        assert_eq!(rec.valid_len, full.len() as u64);
        assert_eq!(rec.done.len(), 2);
        assert_eq!(rec.done[&2].path, PathBuf::from("T/a.dat"));
        assert_eq!(rec.done[&3].location.as_deref(), Some("content://x"));
        let mut log = DeliveryLog::open(&path, rec.valid_len).unwrap();
        log.append(&SavedFile::placed(4, PathBuf::from("T/b.dat")))
            .unwrap();
        drop(log);
        let rec = DeliveryLog::read(tmp.path()).unwrap().unwrap();
        assert_eq!(rec.done.len(), 3);
        // No header, no log.
        std::fs::write(&path, b"{\"fid\":4").unwrap();
        assert!(DeliveryLog::read(tmp.path()).unwrap().is_none());
        // A Phase 7 log: header with the verified root, then the same lines.
        let root = [7u8; 32];
        let v1 = format!(
            "{{\"v\":1,\"root\":\"{}\"}}\n{{\"fid\":2,\"path\":\"T/a.dat\"}}\n",
            hash_to_hex(&root)
        );
        std::fs::write(&path, v1).unwrap();
        let rec = DeliveryLog::read(tmp.path()).unwrap().unwrap();
        assert_eq!(rec.root, Some(root));
        assert_eq!(rec.done.len(), 1);
    }

    /// Eight bundle members of 40,000 bytes, about five chunks in all: the first three
    /// chunks hold four whole members.
    fn spread_members() -> (Manifest, Vec<Vec<u8>>) {
        manifest(&[(40_000, true); 8])
    }

    /// A checkpoint after every chunk, so members become durable chunk by chunk.
    fn every_chunk() -> SyncPolicy {
        SyncPolicy {
            bytes: u64::from(CS),
            interval: Duration::from_secs(3600),
        }
    }

    /// True once a test kill point fired in the bundle's delivery worker.
    fn kill_fired(rx: &TransferReceiver) -> bool {
        let slot = rx.shared.bundle.as_ref().expect("manifest has a bundle");
        let io = slot.io.try_lock().expect("no upload running");
        let SlotIo::Bundle(inner) = &*io else {
            panic!("bundle slot holds the bundle");
        };
        inner.killed.load(Ordering::Acquire)
    }

    /// Streams the first three chunks of the bundle with a checkpoint per chunk, so
    /// members 0 to 3 are handed over while the bundle streams and before any root;
    /// "kills" the process at `crash` inside that early delivery; reopens the transfer from
    /// disk like a restarted host and sends the rest from the durable offset.
    async fn early_kill_reload(
        root: &Path,
        sink: Option<SinkBinding>,
        crash: Crash,
    ) -> (TransferReceiver, Vec<Vec<u8>>) {
        let (m, contents) = spread_members();
        let opts = AcceptOptions {
            sink: sink.clone(),
            sync: every_chunk(),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(root, m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let btok = rx.state().bundle.unwrap().token.unwrap();
        let bundle = bundle_of(&m, &contents).await;
        let bsize = bundle.len() as u64;
        let part = 3 * u64::from(CS);
        set_crash(&rx, crash);
        let mut up = rx.begin_bundle(&btok, 0, part - 1).await.unwrap();
        up.receive(&body(&bundle, 0, part).await[..]).await.unwrap();
        assert_eq!(up.finish(None).await.unwrap().state, FileState::Receiving);
        let t0 = Instant::now();
        while !kill_fired(&rx) {
            assert!(
                t0.elapsed() < Duration::from_secs(10),
                "{crash:?} never fired"
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(rx.staging_dir().join(DELIVERY_LOG).exists());
        drop(rx);

        let rx = TransferReceiver::load_with(root, &m.id, FakeSpace::plenty(), every_chunk(), sink)
            .await
            .unwrap();
        let st = rx.state();
        assert!(
            matches!(st.state, TransferState::Accepted | TransferState::Active),
            "{crash:?}: {:?}",
            st.state
        );
        let from = st.bundle.unwrap().verified_offset;
        assert_eq!(from, part, "{crash:?}: the streamed bytes stayed durable");
        let mut up = rx.begin_bundle(&btok, from, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, from, bsize).await[..])
            .await
            .unwrap();
        let resp = up.finish(Some(root_of(&bundle))).await.unwrap();
        assert_eq!(resp.state, FileState::Verified, "{crash:?}");
        assert_eq!(rx.transfer_state(), TransferState::Complete, "{crash:?}");
        (rx, contents)
    }

    fn early_crashes() -> Vec<Crash> {
        vec![
            Crash::Before(0),
            Crash::Before(1),
            Crash::Inside(0),
            Crash::Inside(1),
            Crash::Inside(3),
        ]
    }

    /// `Directory` mode: a kill before or inside a member handed over while the bundle
    /// still streams resumes on reopen; every member lands once at its own name.
    #[tokio::test]
    async fn early_delivery_survives_a_kill_in_directory_mode() {
        for crash in early_crashes() {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("Downloads");
            let (rx, contents) = early_kill_reload(&root, None, crash).await;
            let t = root.join("T");
            let mut names: Vec<String> = std::fs::read_dir(&t)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            let mut want: Vec<String> = (0..contents.len()).map(|i| format!("f{i}.dat")).collect();
            want.push("empty".to_owned());
            want.sort();
            assert_eq!(names, want, "{crash:?}: each member once, no duplicates");
            for (i, c) in contents.iter().enumerate() {
                assert_eq!(&std::fs::read(t.join(format!("f{i}.dat"))).unwrap(), c);
            }
            assert_eq!(rx.saved_paths().len(), contents.len(), "{crash:?}");
            assert_eq!(rx.files_done(), contents.len());
            let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        }
    }

    /// `Staged` mode: the same kills; the sink sees every member once, except that a kill
    /// after the sink took member k and before it was recorded hands k over once more.
    #[tokio::test]
    async fn early_delivery_survives_a_kill_in_staged_mode() {
        for crash in early_crashes() {
            let tmp = tempfile::tempdir().unwrap();
            let sink = Arc::new(MemSink::default());
            let binding = SinkBinding {
                sink: sink.clone(),
                peer: "ef".repeat(32),
            };
            let (rx, contents) = early_kill_reload(tmp.path(), Some(binding), crash).await;
            let n = contents.len();
            let mut counts = vec![0usize; n];
            for (item, bytes) in lock(&sink.got).iter() {
                let fid: usize = item.path[1][1..2].parse().unwrap();
                assert_eq!(bytes, &contents[fid], "{crash:?}: {:?}", item.path);
                counts[fid] += 1;
            }
            let mut want = vec![1usize; n];
            if let Crash::Inside(k) = crash {
                want[k] = 2;
            }
            assert_eq!(counts, want, "{crash:?}: deliveries per member");
            assert_eq!(rx.delivered().len(), n, "{crash:?}");
            let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        }
    }

    /// `Directory` mode: a member placed early, whose staging file came back because the
    /// stream restarted at 0 just before a kill, is not placed a second time on reopen:
    /// the copy is dropped and the first place stands.
    #[tokio::test]
    async fn recovery_drops_a_restreamed_copy_of_a_placed_member() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("Downloads");
        let (m, contents) = spread_members();
        let opts = AcceptOptions {
            sync: every_chunk(),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(&root, m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        let btok = rx.state().bundle.unwrap().token.unwrap();
        let bundle = bundle_of(&m, &contents).await;
        let bsize = bundle.len() as u64;
        let part = 3 * u64::from(CS);
        let mut up = rx.begin_bundle(&btok, 0, part - 1).await.unwrap();
        up.receive(&body(&bundle, 0, part).await[..]).await.unwrap();
        up.finish(None).await.unwrap();
        let t0 = Instant::now();
        while rx.files_done() < 4 {
            assert!(t0.elapsed() < Duration::from_secs(10));
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let staging = rx.staging_dir().to_path_buf();
        drop(rx);
        // As if a restart at 0 had rewritten member 0 before the kill.
        std::fs::write(staging.join(member_name(0)), &contents[0]).unwrap();

        let rx = TransferReceiver::load(&root, &m.id, FakeSpace::plenty(), every_chunk())
            .await
            .unwrap();
        assert!(
            !staging.join(member_name(0)).exists(),
            "the copy is dropped"
        );
        let mut up = rx.begin_bundle(&btok, part, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, part, bsize).await[..])
            .await
            .unwrap();
        up.finish(Some(root_of(&bundle))).await.unwrap();
        assert_eq!(rx.transfer_state(), TransferState::Complete);
        let t = root.join("T");
        let mut names: Vec<String> = std::fs::read_dir(&t)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        let mut want: Vec<String> = (0..contents.len()).map(|i| format!("f{i}.dat")).collect();
        want.push("empty".to_owned());
        want.sort();
        assert_eq!(names, want, "no duplicate");
        assert_eq!(std::fs::read(t.join("f0.dat")).unwrap(), contents[0]);
    }

    /// A sink that takes 20 ms per file and counts deliveries per path.
    #[derive(Debug, Default)]
    struct SlowCountingSink {
        got: StdMutex<HashMap<String, usize>>,
    }

    impl ReceiveSink for SlowCountingSink {
        fn deliver(
            &self,
            item: DeliverItem,
            _file: File,
        ) -> std::result::Result<String, crate::transfer::SinkError> {
            std::thread::sleep(Duration::from_millis(20));
            *lock(&self.got).entry(item.path.join("/")).or_default() += 1;
            Ok(item.path.join("/"))
        }

        fn make_dir(
            &self,
            _path: Vec<String>,
        ) -> std::result::Result<(), crate::transfer::SinkError> {
            Ok(())
        }
    }

    /// A worker that fails with an I/O error while `finish` is already waiting for it to
    /// drain is retried once by `finish` with a fresh worker: the transfer completes in
    /// that same request and every member reaches the sink once.
    #[tokio::test]
    async fn finish_retries_a_worker_that_fails_while_draining() {
        let tmp = tempfile::tempdir().unwrap();
        let (m, contents) = spread_members();
        let sink = Arc::new(SlowCountingSink::default());
        let opts = AcceptOptions {
            sink: Some(SinkBinding {
                sink: sink.clone(),
                peer: "ef".repeat(32),
            }),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(tmp.path(), m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        {
            let slot = rx.shared.bundle.as_ref().unwrap();
            let mut io = slot.io.try_lock().unwrap();
            let SlotIo::Bundle(inner) = &mut *io else {
                panic!("bundle slot holds the bundle");
            };
            // Members reach the worker only at the end of the request (default policy);
            // at 20 ms each, member 5 fails well after `finish` started draining.
            inner.transient = Some(5);
        }
        let btok = rx.state().bundle.unwrap().token.unwrap();
        let bundle = bundle_of(&m, &contents).await;
        let bsize = bundle.len() as u64;
        let mut up = rx.begin_bundle(&btok, 0, bsize - 1).await.unwrap();
        up.receive(&body(&bundle, 0, bsize).await[..])
            .await
            .unwrap();
        let resp = up.finish(Some(root_of(&bundle))).await.unwrap();
        assert_eq!(resp.state, FileState::Verified);
        assert_eq!(rx.transfer_state(), TransferState::Complete);
        let got = lock(&sink.got);
        assert_eq!(got.len(), contents.len());
        assert!(got.values().all(|&n| n == 1), "{got:?}");
    }

    /// `Directory` mode: a durable member with no staging file and no log line is believed
    /// placed only when a regular file of its size sits at its manifest name; otherwise the
    /// bundle is inconsistent and starts again at 0.
    #[tokio::test]
    async fn a_missing_staging_file_is_believed_only_with_the_file_in_place() {
        for planted in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("Downloads");
            let (m, contents) = spread_members();
            let opts = AcceptOptions {
                sync: every_chunk(),
                ..AcceptOptions::default()
            };
            let rx = TransferReceiver::accept(&root, m.clone(), FakeSpace::plenty(), opts)
                .await
                .unwrap();
            let btok = rx.state().bundle.unwrap().token.unwrap();
            let bundle = bundle_of(&m, &contents).await;
            let bsize = bundle.len() as u64;
            let part = 3 * u64::from(CS);
            // Killed before the first member is placed: nothing is in the log.
            set_crash(&rx, Crash::Before(0));
            let mut up = rx.begin_bundle(&btok, 0, part - 1).await.unwrap();
            up.receive(&body(&bundle, 0, part).await[..]).await.unwrap();
            up.finish(None).await.unwrap();
            let t0 = Instant::now();
            while !kill_fired(&rx) {
                assert!(t0.elapsed() < Duration::from_secs(10));
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            let staging = rx.staging_dir().to_path_buf();
            drop(rx);
            std::fs::remove_file(staging.join(member_name(1))).unwrap();
            if planted {
                std::fs::create_dir_all(root.join("T")).unwrap();
                std::fs::write(root.join("T").join("f1.dat"), &contents[1]).unwrap();
            }
            let rx = TransferReceiver::load(&root, &m.id, FakeSpace::plenty(), every_chunk())
                .await
                .unwrap();
            let from = rx.state().bundle.unwrap().verified_offset;
            if planted {
                assert_eq!(from, part, "believed: the stream goes on");
            } else {
                assert_eq!(from, 0, "inconsistent: the bundle starts again");
            }
            let mut up = rx.begin_bundle(&btok, from, bsize - 1).await.unwrap();
            up.receive(&body(&bundle, from, bsize).await[..])
                .await
                .unwrap();
            up.finish(Some(root_of(&bundle))).await.unwrap();
            assert_eq!(rx.transfer_state(), TransferState::Complete);
            let t = root.join("T");
            let mut names: Vec<String> = std::fs::read_dir(&t)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            let mut want: Vec<String> = (0..contents.len()).map(|i| format!("f{i}.dat")).collect();
            want.push("empty".to_owned());
            want.sort();
            assert_eq!(names, want, "planted {planted}");
            assert_eq!(std::fs::read(t.join("f1.dat")).unwrap(), contents[1]);
        }
    }

    /// Receives one streamed file, "kills" the process at `crash` while it is placed, then
    /// reopens the transfer from disk. Nothing is sent again after the reopen.
    async fn file_kill_reload(
        root: &Path,
        sink: Option<SinkBinding>,
        crash: Crash,
    ) -> (TransferReceiver, Vec<u8>) {
        let (m, contents) = manifest(&[(2 * CS as usize + 7, false)]);
        let opts = AcceptOptions {
            sink: sink.clone(),
            ..AcceptOptions::default()
        };
        let rx = TransferReceiver::accept(root, m.clone(), FakeSpace::plenty(), opts)
            .await
            .unwrap();
        {
            let mut io = rx.shared.files[0].io.try_lock().unwrap();
            let SlotIo::File(part) = &mut *io else {
                panic!("streamed file");
            };
            part.inner.as_mut().unwrap().crash = Some(crash);
        }
        let tok = rx.state().files[0].token.clone().unwrap();
        let size = contents[0].len() as u64;
        let mut up = rx.begin_file(0, &tok, 0, size - 1).await.unwrap();
        up.receive(&body(&contents[0], 0, size).await[..])
            .await
            .unwrap();
        let r = up.finish(Some(root_of(&contents[0]))).await;
        assert!(
            matches!(r, Err(PutError::Core(Error::OffsetMismatch { expected })) if expected == size),
            "{crash:?}: {r:?}"
        );
        assert_eq!(rx.transfer_state(), TransferState::Active);
        drop(rx);
        let rx = TransferReceiver::load_with(
            root,
            &m.id,
            FakeSpace::plenty(),
            SyncPolicy::default(),
            sink,
        )
        .await
        .unwrap();
        assert_eq!(rx.transfer_state(), TransferState::Complete, "{crash:?}");
        assert_eq!(rx.state().files[0].state, FileState::Verified);
        let staged: Vec<_> = std::fs::read_dir(rx.staging_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(staged, [std::ffi::OsString::from("transfer.json")]);
        (rx, contents.into_iter().next().unwrap())
    }

    /// `Directory` mode: a kill before the name is reserved, between the reservation and
    /// the rename, or after the rename finishes the placement on reopen, once, at the
    /// reserved name: no re-download and no "name (2)".
    #[tokio::test]
    async fn file_placement_survives_a_kill_in_directory_mode() {
        for crash in [Crash::Before(0), Crash::Inside(0), Crash::After(0)] {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("Downloads");
            // A file already at the destination name: the incoming one gets " (1)".
            std::fs::create_dir_all(root.join("T")).unwrap();
            std::fs::write(root.join("T").join("f0.dat"), b"keep me").unwrap();
            let (rx, data) = file_kill_reload(&root, None, crash).await;
            let t = root.join("T");
            let mut names: Vec<String> = std::fs::read_dir(&t)
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            assert_eq!(names, ["empty", "f0 (1).dat", "f0.dat"], "{crash:?}");
            assert_eq!(std::fs::read(t.join("f0 (1).dat")).unwrap(), data);
            assert_eq!(std::fs::read(t.join("f0.dat")).unwrap(), b"keep me");
            assert_eq!(rx.saved_paths(), [t.join("f0 (1).dat")], "{crash:?}");
            assert_eq!(rx.files_done(), 1);
        }
    }

    /// `Staged` mode: a kill before the sink call hands the file over once on reopen; a
    /// kill after the sink took it and before the record hands it over a second time.
    #[tokio::test]
    async fn file_placement_survives_a_kill_in_staged_mode() {
        for (crash, times) in [(Crash::Before(0), 1usize), (Crash::Inside(0), 2)] {
            let tmp = tempfile::tempdir().unwrap();
            let sink = Arc::new(MemSink::default());
            let binding = SinkBinding {
                sink: sink.clone(),
                peer: "ef".repeat(32),
            };
            let (rx, data) = file_kill_reload(tmp.path(), Some(binding), crash).await;
            let got = lock(&sink.got);
            assert_eq!(got.len(), times, "{crash:?}");
            assert!(got.iter().all(|(_, b)| *b == data));
            assert_eq!(rx.delivered().len(), 1);
            assert_eq!(rx.delivered()[0].location, "content://test/T/f0.dat");
        }
    }
}
