//! On-disk records of outgoing transfers (protocol.md 7.6 "sender persistence", Phase 9):
//! the transfer ID, manifest, recorded sources and every chunk hash sent so far, so a send
//! survives a restart of the sending host and a failed send resumes when the receiver is
//! back or when the same files are sent to it again.
//!
//! Records live under `<data_dir>/outgoing/`, 0600 inside the 0700 data directory, as
//! three files per transfer: `<id>.json` holds what never changes (peer, manifest,
//! sources) and is written once; `<id>.state` holds what changes (chunk size, hash counts,
//! verified streams, failure, counters) and is rewritten atomically as the send goes;
//! `<id>.hashes` is an append-only log of fixed 40-byte entries (stream tag `u32`, index
//! `u32`, hash, little-endian). A write adds only the hashes that are new, so its cost
//! depends neither on how far the send got nor on how many files it has. The state names
//! how many hashes per stream are valid; entries past that are ignored and a later entry
//! for the same index replaces an earlier one. A cancelled send that could not tell its
//! receiver leaves `<id>.tomb` until it can (24 h at most). Records hold file names and
//! local paths, never keys or tokens (SR-28); names and paths are never logged above
//! debug (SR-32).
//!
//! A record never holds a stream source's opener: a share target's handle dies with the
//! process, so a stream record is only resumed by a new send of the same items (the new
//! openers are attached by fid), never started by the host itself.

use std::collections::{BTreeMap, HashMap};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::identity::{secure_fs, DeviceId};
use crate::transfer::manifest::is_valid_transfer_id;
use crate::transfer::{
    fsx, ChunkHash, FileId, Manifest, SourceInfo, SourceRef, StreamRef, UnsavedHashes,
};
use crate::{Error, Result};

/// Format version of a record. A record with another version is dropped at load.
pub(crate) const RECORD_VERSION: u32 = 1;

/// Longest a record is kept or matched, from when the send was first offered. The
/// receiver deletes a `.part` idle for 24 h (protocol.md 7.6, SR-20), after which there
/// is nothing left to resume into.
pub(crate) const MAX_AGE: Duration = Duration::from_hours(24);

/// Folder of the records, under the data directory.
const DIR: &str = "outgoing";

/// Times the host starts a failed send again by itself (peer back, host start) before it
/// leaves the record for a send of the same files by the user. *Choice:* a receiver that
/// fails the send every time would otherwise restart it at every session for 24 h.
pub(crate) const MAX_AUTO_RESUMES: u32 = 3;

/// Why a send gave up while keeping its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FailureKind {
    /// The receiver could not be reached for the reconnect window: resumed by itself when
    /// the peer is back.
    Unreachable,
    /// The receiver kept dropping the upload without progress: only a new send of the
    /// same files resumes it, since the peer being online again proves nothing.
    Stalled,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if"
)]
fn is_zero(n: &u32) -> bool {
    *n == 0
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if"
)]
fn is_zero_usize(n: &usize) -> bool {
    *n == 0
}

/// Stream tag of the bundle in `<id>.hashes`; a file uses its fid.
const BUNDLE_TAG: u32 = u32::MAX;

/// Bytes per entry in `<id>.hashes`: tag, index, hash.
pub(crate) const ENTRY: usize = 4 + 4 + 32;

/// One outgoing transfer as the sender keeps it (protocol.md 7.6). On disk it is three
/// files: what never changes (`<id>.json`: version, peer, created time, manifest,
/// sources), written once; what changes ([`RecordState`] in `<id>.state`), small and
/// rewritten as the send goes; and the hash log (`<id>.hashes`). So a write's cost depends
/// neither on how far the send got nor on how many files it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SendRecord {
    /// [`RECORD_VERSION`].
    pub v: u32,
    /// The receiver.
    pub peer: DeviceId,
    /// When the send was first offered, Unix ms.
    pub created_ms: u64,
    /// The ID this device knows the send by (handle, events, `transfers()`), when it
    /// differs from the manifest's: a send of the same files that continued under a new
    /// transfer ID keeps its first ID locally. `None`: the manifest's ID.
    pub public_id: Option<String>,
    /// The manifest as offered; its `id` is the transfer ID on the wire.
    pub manifest: Manifest,
    /// One entry per fid.
    pub sources: Vec<RecordedSource>,
    /// The binding chunk size, once the receiver's answer fixed it (7.2).
    pub chunk_size: Option<u32>,
    /// Bytes of `<id>.hashes` the state vouches for; anything past them is ignored and
    /// cut off before the next append.
    pub log_len: u64,
    /// How many chunk hashes in `<id>.hashes` are valid, per streamed fid.
    pub hash_counts: BTreeMap<u32, usize>,
    /// How many chunk hashes of the bundle stream are valid.
    pub bundle_hash_count: usize,
    /// The valid hashes per streamed fid, as loaded from `<id>.hashes`.
    pub hashes: BTreeMap<u32, Vec<ChunkHash>>,
    /// The valid bundle hashes, as loaded.
    pub bundle_hashes: Vec<ChunkHash>,
    /// Set once the send gave up: it then waits for the peer or a new send.
    pub failed_at_ms: Option<u64>,
    /// Why it gave up.
    pub failure: Option<String>,
    /// What kind of give-up, which decides how it resumes.
    pub failure_kind: Option<FailureKind>,
    /// Times the host started it again by itself ([`MAX_AUTO_RESUMES`]).
    pub auto_resumes: u32,
    /// Times the receiver answered `404` and the manifest was offered again.
    pub reoffers: u32,
    /// Streamed fids the receiver reported verified (a `PUT` answer or its `GET` view).
    pub verified: Vec<u32>,
    /// The receiver reported the bundle verified.
    pub bundle_verified: bool,
}

/// The part of a record written once (`<id>.json`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StaticRef<'a> {
    v: u32,
    peer: DeviceId,
    created_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    public_id: Option<&'a str>,
    manifest: &'a Manifest,
    sources: &'a [RecordedSource],
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StaticOwned {
    v: u32,
    peer: DeviceId,
    created_ms: u64,
    #[serde(default)]
    public_id: Option<String>,
    manifest: Manifest,
    sources: Vec<RecordedSource>,
}

/// The part of a record that changes as the send goes (`<id>.state`). Its write is the
/// commit point for the hash counts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecordState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk_size: Option<u32>,
    #[serde(default)]
    pub log_len: u64,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub hash_counts: BTreeMap<u32, usize>,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub bundle_hash_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_kind: Option<FailureKind>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub auto_resumes: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reoffers: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub verified: Vec<u32>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bundle_verified: bool,
}

/// What a record keeps of one source: the SR-21 facts recorded at offer time, never the
/// opener of a stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RecordedSource {
    /// A local file, by absolute path.
    #[serde(rename_all = "camelCase")]
    Path {
        path: PathBuf,
        size: u64,
        mtime_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<FileId>,
    },
    /// A host-supplied stream, by its manifest name.
    #[serde(rename_all = "camelCase")]
    Stream {
        name: String,
        size: u64,
        mtime_ms: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<FileId>,
    },
}

impl RecordedSource {
    /// Describes `info`, the source of `manifest.files[fid]`. A relative path is made
    /// absolute against the current directory, so a restarted host (which may run from
    /// elsewhere) reopens the same file. `None` when that fails.
    pub(crate) fn of(manifest: &Manifest, fid: usize, info: &SourceInfo) -> Option<Self> {
        Some(match &info.source {
            SourceRef::Path(p) => Self::Path {
                path: std::path::absolute(p).ok()?,
                size: info.size,
                mtime_ms: info.mtime_ms,
                file_id: info.file_id,
            },
            SourceRef::Opener(_) => Self::Stream {
                name: manifest.files.get(fid)?.path.join("/"),
                size: info.size,
                mtime_ms: info.mtime_ms,
                file_id: info.file_id,
            },
        })
    }

    /// Whether `info` (the source of a new send) is the same item: same kind, size, mtime
    /// and, for a path, the same absolute path and file ID; for a stream, the same name and
    /// the same file ID when both sides know one (a phone's share target may not expose
    /// one through `fstat`).
    fn same_as(&self, name: &str, info: &SourceInfo) -> bool {
        match (self, &info.source) {
            (
                Self::Path {
                    path,
                    size,
                    mtime_ms,
                    file_id,
                },
                SourceRef::Path(p),
            ) => {
                std::path::absolute(p).is_ok_and(|abs| abs == *path)
                    && *size == info.size
                    && *mtime_ms == info.mtime_ms
                    && *file_id == info.file_id
            }
            (
                Self::Stream {
                    name: n,
                    size,
                    mtime_ms,
                    file_id,
                },
                SourceRef::Opener(_),
            ) => {
                n == name
                    && *size == info.size
                    && *mtime_ms == info.mtime_ms
                    && (file_id.is_none() || info.file_id.is_none() || *file_id == info.file_id)
            }
            _ => false,
        }
    }
}

impl SendRecord {
    /// A record for a send about to be offered. `None` when a source cannot be described
    /// (a path that cannot be made absolute); such a send runs without a record.
    pub(crate) fn new(
        peer: DeviceId,
        created_ms: u64,
        manifest: &Manifest,
        sources: &[SourceInfo],
    ) -> Option<Self> {
        let sources = sources
            .iter()
            .enumerate()
            .map(|(fid, s)| RecordedSource::of(manifest, fid, s))
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            v: RECORD_VERSION,
            peer,
            created_ms,
            public_id: None,
            manifest: manifest.clone(),
            chunk_size: None,
            log_len: 0,
            sources,
            hash_counts: BTreeMap::new(),
            bundle_hash_count: 0,
            hashes: BTreeMap::new(),
            bundle_hashes: Vec::new(),
            failed_at_ms: None,
            failure: None,
            failure_kind: None,
            auto_resumes: 0,
            reoffers: 0,
            verified: Vec::new(),
            bundle_verified: false,
        })
    }

    /// True when the receiver reported every stream of this send verified (every
    /// non-empty streamed file and the bundle) and there is at least one. A receiver that
    /// then answers `404` has finished the transfer and forgotten it, so the send is
    /// complete; zero-length files alone prove nothing and are offered again.
    pub(crate) fn all_streams_verified(&self) -> bool {
        let mut any = false;
        for f in self
            .manifest
            .files
            .iter()
            .filter(|f| !f.in_bundle && f.size > 0)
        {
            any = true;
            if !self.verified.contains(&f.fid) {
                return false;
            }
        }
        if self.manifest.bundle.is_some() {
            any = true;
            if !self.bundle_verified {
                return false;
            }
        }
        any
    }

    /// The in-memory summary the host indexes this record by.
    pub(crate) fn summary(&self) -> Summary {
        Summary {
            peer: self.peer,
            created_ms: self.created_ms,
            failed: self.failed_at_ms.is_some(),
            failure_kind: self.failure_kind,
            auto_resumes: self.auto_resumes,
            all_paths: self.all_paths(),
            wire_id: self.manifest.id.clone(),
            files: self.manifest.files.len(),
            total_bytes: self.manifest.total_bytes,
            first: self.manifest.files.first().map(|f| f.path.clone()),
        }
    }

    /// Forgets the hashes of stream sources whose mtime is unknown (0), and of the bundle
    /// when one of them travels in it. Such an item can be edited in place with the same
    /// size and file ID, and nothing in the record would notice; the hashes are rebuilt
    /// from the new opener at resume and compared with the receiver's prefix instead
    /// (SR-21).
    pub(crate) fn forget_unverifiable_hashes(&mut self) {
        let mut bundle = false;
        for (fid, s) in self.sources.iter().enumerate() {
            if matches!(s, RecordedSource::Stream { mtime_ms: 0, .. }) {
                let Ok(fid) = u32::try_from(fid) else {
                    continue;
                };
                self.hashes.remove(&fid);
                self.hash_counts.remove(&fid);
                bundle |= self
                    .manifest
                    .files
                    .get(fid as usize)
                    .is_some_and(|f| f.in_bundle);
            }
        }
        if bundle {
            self.bundle_hashes.clear();
            self.bundle_hash_count = 0;
        }
    }

    /// What changes as the send goes.
    pub(crate) fn state(&self) -> RecordState {
        RecordState {
            chunk_size: self.chunk_size,
            log_len: self.log_len,
            hash_counts: self.hash_counts.clone(),
            bundle_hash_count: self.bundle_hash_count,
            failed_at_ms: self.failed_at_ms,
            failure: self.failure.clone(),
            failure_kind: self.failure_kind,
            auto_resumes: self.auto_resumes,
            reoffers: self.reoffers,
            verified: self.verified.clone(),
            bundle_verified: self.bundle_verified,
        }
    }

    fn apply_state(&mut self, st: RecordState) {
        self.chunk_size = st.chunk_size;
        self.log_len = st.log_len;
        self.hash_counts = st.hash_counts;
        self.bundle_hash_count = st.bundle_hash_count;
        self.failed_at_ms = st.failed_at_ms;
        self.failure = st.failure;
        self.failure_kind = st.failure_kind;
        self.auto_resumes = st.auto_resumes;
        self.reoffers = st.reoffers;
        self.verified = st.verified;
        self.bundle_verified = st.bundle_verified;
    }

    /// The transfer ID.
    pub(crate) fn id(&self) -> &str {
        self.public_id.as_deref().unwrap_or(&self.manifest.id)
    }

    /// True once the record is older than [`MAX_AGE`] at `now_ms`.
    pub(crate) fn is_expired(&self, now_ms: u64) -> bool {
        let max = u64::try_from(MAX_AGE.as_millis()).unwrap_or(u64::MAX);
        now_ms.saturating_sub(self.created_ms) > max
    }

    /// True when every source is a local path, so the host can start the send again by
    /// itself.
    pub(crate) fn all_paths(&self) -> bool {
        self.sources
            .iter()
            .all(|s| matches!(s, RecordedSource::Path { .. }))
    }

    /// The recorded sources as the sender uses them, for a record whose sources are all
    /// paths. The offer-time facts are kept, not re-read, so the SR-21 check compares the
    /// file as it is now with the file as it was offered.
    pub(crate) fn path_sources(&self) -> Option<Vec<SourceInfo>> {
        self.sources
            .iter()
            .map(|s| match s {
                RecordedSource::Path {
                    path,
                    size,
                    mtime_ms,
                    file_id,
                } => Some(SourceInfo {
                    source: SourceRef::Path(path.clone()),
                    size: *size,
                    mtime_ms: *mtime_ms,
                    file_id: *file_id,
                }),
                RecordedSource::Stream { .. } => None,
            })
            .collect()
    }

    /// Whether a new send of `manifest` with `sources` to `peer` is this record's send
    /// again: same peer, same files one to one (path, size, mtime, bundled or not, in fid
    /// order), same empty folders, and the same items behind them
    /// ([`RecordedSource::same_as`]). The note and MIME hints are not compared: the
    /// recorded manifest is what gets offered again, since the receiver only takes the same
    /// ID back with the same manifest (protocol.md 7.3).
    pub(crate) fn matches(
        &self,
        peer: &DeviceId,
        manifest: &Manifest,
        sources: &[SourceInfo],
    ) -> bool {
        let (old, new) = (&self.manifest, manifest);
        self.peer == *peer
            && old.files.len() == new.files.len()
            && self.sources.len() == sources.len()
            && sources.len() == new.files.len()
            && old.dirs == new.dirs
            && old.total_bytes == new.total_bytes
            && old.files.iter().zip(&new.files).all(|(a, b)| {
                a.fid == b.fid
                    && a.path == b.path
                    && a.size == b.size
                    && a.mtime == b.mtime
                    && a.in_bundle == b.in_bundle
            })
            && self
                .sources
                .iter()
                .zip(sources)
                .zip(&new.files)
                .all(|((rec, info), f)| rec.same_as(&f.path.join("/"), info))
    }

    /// Notes how many hashes per stream the sender holds now (their bytes go to
    /// `<id>.hashes`).
    pub(crate) fn set_hash_counts(&mut self, files: &[(u32, usize)], bundle: usize) {
        self.hash_counts = files.iter().filter(|(_, n)| *n > 0).copied().collect();
        self.bundle_hash_count = bundle;
    }
}

/// What the host keeps in memory about a record, so finding candidates for a resume, a
/// cancel or an adoption reads no file (the full record is read only for a candidate).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Summary {
    pub peer: DeviceId,
    pub created_ms: u64,
    pub failed: bool,
    pub failure_kind: Option<FailureKind>,
    pub auto_resumes: u32,
    pub all_paths: bool,
    /// The transfer ID on the wire (the `DELETE` of a cancel goes to it).
    pub wire_id: String,
    pub files: usize,
    pub total_bytes: u64,
    /// The first file's manifest path, a cheap pre-filter for adoption.
    pub first: Option<Vec<String>>,
}

impl Summary {
    /// Whether a new send of `manifest` to `peer` may be this record's send again; the
    /// full rule is [`SendRecord::matches`].
    pub(crate) fn may_match(&self, peer: &DeviceId, manifest: &Manifest) -> bool {
        self.peer == *peer
            && self.files == manifest.files.len()
            && self.total_bytes == manifest.total_bytes
            && self.first.as_ref() == manifest.files.first().map(|f| &f.path)
    }

    /// Whether the host starts this record again by itself when its peer comes back: it
    /// failed because the receiver was unreachable, every source is a path, and it has
    /// not used up [`MAX_AUTO_RESUMES`].
    pub(crate) fn resumes_on_return(&self) -> bool {
        self.failed
            && self.failure_kind == Some(FailureKind::Unreachable)
            && self.all_paths
            && self.auto_resumes < MAX_AUTO_RESUMES
    }

    /// Whether the host starts this record again at its own start: it was running when
    /// the host stopped, and every source is a path.
    pub(crate) fn resumes_at_start(&self) -> bool {
        !self.failed && self.all_paths && self.auto_resumes < MAX_AUTO_RESUMES
    }

    /// True once the record is older than [`MAX_AGE`] at `now_ms`.
    pub(crate) fn is_expired(&self, now_ms: u64) -> bool {
        let max = u64::try_from(MAX_AGE.as_millis()).unwrap_or(u64::MAX);
        now_ms.saturating_sub(self.created_ms) > max
    }
}

/// Every record on disk by transfer ID, kept in step with the files by the send code.
#[derive(Debug, Default)]
pub(crate) struct Index {
    map: HashMap<String, Summary>,
}

impl Index {
    pub(crate) fn from_records(records: &[SendRecord]) -> Self {
        Self {
            map: records
                .iter()
                .map(|r| (r.id().to_owned(), r.summary()))
                .collect(),
        }
    }

    pub(crate) fn get(&self, id: &str) -> Option<&Summary> {
        self.map.get(id)
    }

    pub(crate) fn upsert(&mut self, id: &str, summary: Summary) {
        self.map.insert(id.to_owned(), summary);
    }

    pub(crate) fn remove(&mut self, id: &str) -> Option<Summary> {
        self.map.remove(id)
    }

    /// IDs whose summary satisfies `pred`, oldest record first.
    pub(crate) fn ids_where(&self, mut pred: impl FnMut(&Summary) -> bool) -> Vec<String> {
        let mut ids: Vec<(&String, &Summary)> = self.map.iter().filter(|(_, s)| pred(s)).collect();
        ids.sort_by_key(|(_, s)| s.created_ms);
        ids.into_iter().map(|(id, _)| id.clone()).collect()
    }
}

/// The records folder.
pub(crate) fn dir(data_dir: &Path) -> PathBuf {
    data_dir.join(DIR)
}

fn path_of(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{id}.json"))
}

fn hashes_path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{id}.hashes"))
}

fn state_path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{id}.state"))
}

fn tomb_path(data_dir: &Path, id: &str) -> PathBuf {
    dir(data_dir).join(format!("{id}.tomb"))
}

fn tag_of(stream: StreamRef) -> u32 {
    match stream {
        StreamRef::File(fid) => fid,
        StreamRef::Bundle => BUNDLE_TAG,
    }
}

fn encode_entries(unsaved: &[UnsavedHashes]) -> Vec<u8> {
    let n: usize = unsaved.iter().map(|u| u.hashes.len()).sum();
    let mut out = Vec::with_capacity(n * ENTRY);
    for u in unsaved {
        let tag = tag_of(u.stream);
        for (i, h) in u.hashes.iter().enumerate() {
            let index = u32::try_from(u.start + i).unwrap_or(u32::MAX);
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&index.to_le_bytes());
            out.extend_from_slice(h);
        }
    }
    out
}

/// Writes a record (blocking; call from a blocking thread): with `whole`, first the part
/// that never changes (`<id>.json`, atomically); then the hashes in `unsaved` to
/// `<id>.hashes` (appended and flushed, or with `rewrite` the whole log replaced
/// atomically by exactly these); then `state` (`<id>.state`, atomically), which makes the
/// new counts valid. A crash between the last two leaves entries the old state does not
/// count, or that replace an index with hashes it does not yet vouch for; a replaced
/// prefix is still compared with the receiver's before anything resumes (SR-21), so the
/// worst case is a restart at 0. Every file is 0600.
#[allow(
    clippy::too_many_arguments,
    reason = "the three files of one record write"
)]
pub(crate) fn write(
    data_dir: &Path,
    id: &str,
    whole: Option<&SendRecord>,
    mut state: RecordState,
    committed: u64,
    unsaved: &[UnsavedHashes],
    rewrite: bool,
) -> Result<u64> {
    if !is_valid_transfer_id(id) {
        return Err(Error::BadRequest("transfer ID".into()));
    }
    secure_fs::ensure_private_dir(&dir(data_dir))?;
    if let Some(rec) = whole {
        let json = serde_json::to_vec(&StaticRef {
            v: rec.v,
            peer: rec.peer,
            created_ms: rec.created_ms,
            public_id: rec.public_id.as_deref(),
            manifest: &rec.manifest,
            sources: &rec.sources,
        })?;
        fsx::write_atomic(&path_of(data_dir, id), &json)?;
    }
    let entries = encode_entries(unsaved);
    let log = hashes_path(data_dir, id);
    let added = entries.len() as u64;
    state.log_len = if rewrite {
        fsx::write_atomic(&log, &entries)?;
        added
    } else if entries.is_empty() {
        committed
    } else {
        use std::io::Write as _;
        let mut o = fsx::private_file_options();
        o.create(true);
        let mut f = o.open(&log).map_err(fsx::map_io)?;
        // Whatever a write that never committed left past the committed length goes
        // first, so the new entries sit where the state will say they do.
        f.set_len(committed).map_err(fsx::map_io)?;
        f.seek(SeekFrom::Start(committed)).map_err(fsx::map_io)?;
        f.write_all(&entries).map_err(fsx::map_io)?;
        f.sync_data().map_err(fsx::map_io)?;
        committed + added
    };
    let log_len = state.log_len;
    fsx::write_atomic(&state_path(data_dir, id), &serde_json::to_vec(&state)?)?;
    Ok(log_len)
}

/// [`write`] of a whole record with every hash it holds, replacing what is on disk.
#[cfg(test)]
pub(crate) fn write_whole(data_dir: &Path, rec: &SendRecord) -> Result<()> {
    let mut unsaved: Vec<UnsavedHashes> = rec
        .hashes
        .iter()
        .map(|(fid, h)| UnsavedHashes {
            stream: StreamRef::File(*fid),
            start: 0,
            hashes: h.clone(),
        })
        .collect();
    if !rec.bundle_hashes.is_empty() {
        unsaved.push(UnsavedHashes {
            stream: StreamRef::Bundle,
            start: 0,
            hashes: rec.bundle_hashes.clone(),
        });
    }
    write(
        data_dir,
        rec.id(),
        Some(rec),
        rec.state(),
        0,
        &unsaved,
        true,
    )
    .map(drop)
}

/// Fills a record's hashes from `<id>.hashes`: the latest entry per index wins, each
/// stream up to its count in the state or to the first missing index, whichever is less.
fn read_hashes(data_dir: &Path, rec: &mut SendRecord) {
    let mut bytes = std::fs::read(hashes_path(data_dir, rec.id())).unwrap_or_default();
    bytes.truncate(usize::try_from(rec.log_len).unwrap_or(usize::MAX));
    // A count can never exceed the stream's chunk count; anything larger is damage, and
    // trusting it would size the lists by it.
    let chunks = |size: u64| -> usize {
        rec.chunk_size.map_or(0, |cs| {
            usize::try_from(crate::transfer::chunk::chunk_count(size, cs)).unwrap_or(0)
        })
    };
    let bundle_cap = rec.manifest.bundle.map_or(0, |b| chunks(b.size));
    let mut streams: HashMap<u32, Vec<Option<ChunkHash>>> = HashMap::new();
    for e in bytes.as_chunks::<ENTRY>().0 {
        let tag = u32::from_le_bytes([e[0], e[1], e[2], e[3]]);
        let index = u32::from_le_bytes([e[4], e[5], e[6], e[7]]) as usize;
        let valid = if tag == BUNDLE_TAG {
            rec.bundle_hash_count.min(bundle_cap)
        } else {
            let cap = rec
                .manifest
                .files
                .get(tag as usize)
                .filter(|f| !f.in_bundle)
                .map_or(0, |f| chunks(f.size));
            rec.hash_counts.get(&tag).copied().unwrap_or(0).min(cap)
        };
        if index >= valid {
            continue;
        }
        let Ok(hash) = ChunkHash::try_from(&e[8..]) else {
            continue;
        };
        let list = streams.entry(tag).or_default();
        if list.len() <= index {
            list.resize(index + 1, None);
        }
        list[index] = Some(hash);
    }
    let complete = |list: Vec<Option<ChunkHash>>| -> Vec<ChunkHash> {
        list.into_iter().map_while(|h| h).collect()
    };
    rec.bundle_hashes = streams
        .remove(&BUNDLE_TAG)
        .map(complete)
        .unwrap_or_default();
    rec.bundle_hash_count = rec.bundle_hashes.len();
    rec.hashes = streams
        .into_iter()
        .map(|(tag, list)| (tag, complete(list)))
        .filter(|(_, h)| !h.is_empty())
        .collect();
    rec.hash_counts = rec.hashes.iter().map(|(t, h)| (*t, h.len())).collect();
}

fn remove_quiet(path: &Path) {
    if let Err(e) = std::fs::remove_file(path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::debug!(error = %e, "send record file not deleted");
        }
    }
}

/// Deletes the record of `id` and its hashes, if any (blocking).
pub(crate) fn remove(data_dir: &Path, id: &str) {
    if !is_valid_transfer_id(id) {
        return;
    }
    remove_quiet(&path_of(data_dir, id));
    remove_quiet(&state_path(data_dir, id));
    remove_quiet(&hashes_path(data_dir, id));
}

/// Reads the record of `id` with its hashes, if there is a valid one (blocking).
pub(crate) fn load(data_dir: &Path, id: &str) -> Option<SendRecord> {
    if !is_valid_transfer_id(id) {
        return None;
    }
    let mut rec = read(data_dir, id)?;
    read_hashes(data_dir, &mut rec);
    Some(rec)
}

/// Reads a record's fixed part and its state (a missing state is a send that never
/// wrote one: nothing yet).
fn read(data_dir: &Path, id: &str) -> Option<SendRecord> {
    let bytes = std::fs::read(path_of(data_dir, id)).ok()?;
    let fixed = serde_json::from_slice::<StaticOwned>(&bytes)
        .ok()
        .filter(|r| {
            r.v == RECORD_VERSION
                && r.public_id.as_deref().unwrap_or(&r.manifest.id) == id
                && r.sources.len() == r.manifest.files.len()
                && r.manifest.validate().is_ok()
        })?;
    let state = std::fs::read(state_path(data_dir, id))
        .ok()
        .and_then(|b| serde_json::from_slice::<RecordState>(&b).ok())
        .unwrap_or_default();
    let mut rec = SendRecord {
        v: fixed.v,
        peer: fixed.peer,
        created_ms: fixed.created_ms,
        public_id: fixed.public_id,
        manifest: fixed.manifest,
        sources: fixed.sources,
        chunk_size: None,
        log_len: 0,
        hash_counts: BTreeMap::new(),
        bundle_hash_count: 0,
        hashes: BTreeMap::new(),
        bundle_hashes: Vec::new(),
        failed_at_ms: None,
        failure: None,
        failure_kind: None,
        auto_resumes: 0,
        reoffers: 0,
        verified: Vec::new(),
        bundle_verified: false,
    };
    rec.apply_state(state);
    Some(rec)
}

/// Every valid, unexpired record with its hashes, at host start before any send runs
/// (blocking). Expired, unreadable, foreign-version and misnamed records are deleted on
/// the way, and so are leftover temp files of an interrupted write and state files and
/// hash logs without a record.
pub(crate) fn load_all(data_dir: &Path, now_ms: u64) -> Vec<SendRecord> {
    let Ok(rd) = std::fs::read_dir(dir(data_dir)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut logs = Vec::new();
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(id) = name.strip_suffix(".json") else {
            if Path::new(name)
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case("tmp"))
            {
                let _ = std::fs::remove_file(&path);
            } else if let Some(id) = name
                .strip_suffix(".hashes")
                .or_else(|| name.strip_suffix(".state"))
            {
                logs.push(id.to_owned());
            }
            continue;
        };
        match read(data_dir, id) {
            Some(mut r) if !r.is_expired(now_ms) => {
                read_hashes(data_dir, &mut r);
                out.push(r);
            }
            Some(_) => {
                tracing::debug!("send record older than 24 h; deleted");
                remove(data_dir, id);
            }
            None => {
                tracing::debug!("send record unreadable; deleted");
                remove(data_dir, id);
            }
        }
    }
    {
        for id in logs
            .iter()
            .filter(|id| !out.iter().any(|r| r.id() == id.as_str()))
        {
            remove_quiet(&hashes_path(data_dir, id));
            remove_quiet(&state_path(data_dir, id));
        }
    }
    out.sort_by_key(|r| r.created_ms);
    out
}

/// A cancel the receiver has not heard of yet: the `DELETE` goes out at the peer's next
/// session (protocol.md 7.5), for at most [`MAX_AGE`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Tomb {
    pub peer: DeviceId,
    pub at_ms: u64,
    /// The transfer ID on the wire, when it differs from the record's local ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_id: Option<String>,
}

impl Tomb {
    pub(crate) fn is_expired(&self, now_ms: u64) -> bool {
        let max = u64::try_from(MAX_AGE.as_millis()).unwrap_or(u64::MAX);
        now_ms.saturating_sub(self.at_ms) > max
    }
}

/// Writes the tomb of `id` (blocking).
pub(crate) fn write_tomb(data_dir: &Path, id: &str, tomb: &Tomb) -> Result<()> {
    if !is_valid_transfer_id(id) {
        return Err(Error::BadRequest("transfer ID".into()));
    }
    secure_fs::ensure_private_dir(&dir(data_dir))?;
    fsx::write_atomic(&tomb_path(data_dir, id), &serde_json::to_vec(tomb)?)
}

/// Deletes the tomb of `id` (blocking).
pub(crate) fn remove_tomb(data_dir: &Path, id: &str) {
    if is_valid_transfer_id(id) {
        remove_quiet(&tomb_path(data_dir, id));
    }
}

/// Every unexpired tomb by transfer ID (blocking); expired and unreadable ones are
/// deleted.
pub(crate) fn load_tombs(data_dir: &Path, now_ms: u64) -> HashMap<String, Tomb> {
    let Ok(rd) = std::fs::read_dir(dir(data_dir)) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".tomb")) else {
            continue;
        };
        let tomb = std::fs::read(entry.path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Tomb>(&b).ok())
            .filter(|t| is_valid_transfer_id(id) && !t.is_expired(now_ms));
        match tomb {
            Some(t) => {
                out.insert(id.to_owned(), t);
            }
            None => remove_quiet(&entry.path()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::ManifestBuilder;

    const CS: u32 = 64 * 1024;
    const NOW: u64 = 1_800_000_000_000;

    fn peer(b: u8) -> DeviceId {
        DeviceId::from_spki_hash([b; 32])
    }

    async fn build(paths: &[&Path]) -> (Manifest, Vec<SourceInfo>) {
        let mut b = ManifestBuilder::new(CS);
        for p in paths {
            b.add_path(p).await.unwrap();
        }
        b.build().unwrap()
    }

    #[tokio::test]
    async fn matching_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("Photos");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("a.jpg"), vec![1u8; 300_000]).unwrap();
        std::fs::write(src.join("b.txt"), b"note").unwrap();
        let (m, s) = build(&[&src]).await;
        let rec = SendRecord::new(peer(1), NOW, &m, &s).unwrap();

        // The same folder again, under a new ID: a match.
        let (m2, s2) = build(&[&src]).await;
        assert_ne!(m2.id, m.id);
        assert!(rec.matches(&peer(1), &m2, &s2));
        // Another peer: no match.
        assert!(!rec.matches(&peer(2), &m2, &s2));

        // A changed size.
        std::fs::write(src.join("b.txt"), b"note!").unwrap();
        let (m3, s3) = build(&[&src]).await;
        assert!(!rec.matches(&peer(1), &m3, &s3));
        std::fs::write(src.join("b.txt"), b"note").unwrap();

        // A changed mtime (same size).
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(src.join("a.jpg"))
            .unwrap();
        f.set_modified(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1000))
            .unwrap();
        drop(f);
        let (m4, s4) = build(&[&src]).await;
        assert!(!rec.matches(&peer(1), &m4, &s4));

        // The same names and facts at another absolute path.
        let rec4 = SendRecord::new(peer(1), NOW, &m4, &s4).unwrap();
        let other = tmp.path().join("elsewhere");
        std::fs::create_dir(&other).unwrap();
        let moved = other.join("Photos");
        std::fs::rename(&src, &moved).unwrap();
        let (m5, s5) = build(&[&moved]).await;
        assert_eq!(m5.files[0].path, m4.files[0].path);
        assert!(!rec4.matches(&peer(1), &m5, &s5), "another absolute path");
    }

    #[test]
    fn age() {
        let m = empty_manifest("0123456789abcdef0123456789abcdef");
        let rec = SendRecord::new(peer(1), NOW, &m, &[]).unwrap();
        assert!(!rec.is_expired(NOW + 1000));
        let day = u64::try_from(MAX_AGE.as_millis()).unwrap();
        assert!(rec.is_expired(NOW + day + 1));
    }

    fn empty_manifest(id: &str) -> Manifest {
        Manifest {
            id: id.into(),
            chunk_size: CS,
            total_bytes: 0,
            note: Some("n".into()),
            files: Vec::new(),
            dirs: vec![vec!["e".into()]],
            bundle: None,
        }
    }

    fn private_data(tmp: &tempfile::TempDir) -> PathBuf {
        let data = tmp.path().join("data");
        std::fs::create_dir(&data).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        data
    }

    fn unsaved(stream: StreamRef, start: usize, hashes: &[ChunkHash]) -> UnsavedHashes {
        UnsavedHashes {
            stream,
            start,
            hashes: hashes.to_vec(),
        }
    }

    /// A record with four streamed files (fids 0 to 3, 10 chunks each) and a bundle.
    async fn streamed_record(tmp: &tempfile::TempDir) -> SendRecord {
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        for i in 0..4 {
            std::fs::write(src.join(format!("a{i}")), vec![1u8; 10 * CS as usize]).unwrap();
        }
        std::fs::write(src.join("z.txt"), b"small").unwrap();
        let (m, s) = build(&[&src]).await;
        let mut rec = SendRecord::new(peer(1), NOW, &m, &s).unwrap();
        rec.chunk_size = Some(CS);
        rec
    }

    /// Writes `rec` with `unsaved` the way a send does, returning the new committed
    /// length of the log.
    fn put(
        data: &Path,
        rec: &SendRecord,
        committed: u64,
        unsaved: &[UnsavedHashes],
        rewrite: bool,
    ) -> u64 {
        write(
            data,
            rec.id(),
            Some(rec),
            rec.state(),
            committed,
            unsaved,
            rewrite,
        )
        .unwrap()
    }

    /// The hash log: appends, a replaced index, entries past the state's count, a gap,
    /// a count past the stream's chunk count and a rewrite all load as the state says
    /// (protocol.md 7.6).
    #[tokio::test]
    async fn hash_log_appends_and_rewrites() {
        let tmp = tempfile::tempdir().unwrap();
        let data = private_data(&tmp);
        let mut rec = streamed_record(&tmp).await;
        let h: Vec<ChunkHash> = (0..6u8).map(|i| [i; 32]).collect();
        rec.set_hash_counts(&[(3, 2)], 1);
        let mut len = put(
            &data,
            &rec,
            0,
            &[
                unsaved(StreamRef::File(3), 0, &h[..2]),
                unsaved(StreamRef::Bundle, 0, &h[5..]),
            ],
            false,
        );
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(back.hashes.get(&3).unwrap(), &h[..2]);
        assert_eq!(back.bundle_hashes, &h[5..]);

        // Only the new tail goes in; index 1 is replaced; the state now counts 3.
        rec.set_hash_counts(&[(3, 3)], 1);
        len = put(
            &data,
            &rec,
            len,
            &[unsaved(StreamRef::File(3), 1, &[h[4], h[2]])],
            false,
        );
        assert_eq!(len, 5 * ENTRY as u64, "appended, not rewritten");
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(back.hashes.get(&3).unwrap(), &[h[0], h[4], h[2]]);

        // A count past what the log holds stops at the first missing index; a count past
        // the stream's 10 chunks is cut to 10.
        rec.set_hash_counts(&[(3, 9)], 1);
        len = put(
            &data,
            &rec,
            len,
            &[unsaved(StreamRef::File(3), 6, &h[..1])],
            false,
        );
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(back.hashes.get(&3).unwrap().len(), 3, "index 3 is missing");
        rec.set_hash_counts(&[(3, 1_000_000)], 1);
        put(&data, &rec, len, &[], false);
        assert_eq!(
            load(&data, rec.id()).unwrap().hashes.get(&3).unwrap().len(),
            3
        );

        // A rewrite leaves exactly what it carries.
        rec.set_hash_counts(&[(3, 1)], 0);
        let len = put(
            &data,
            &rec,
            len,
            &[unsaved(StreamRef::File(3), 0, &h[2..3])],
            true,
        );
        assert_eq!(len, ENTRY as u64);
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(back.hashes.get(&3).unwrap(), &h[2..3]);
        assert_eq!(back.bundle_hashes, Vec::<ChunkHash>::new());

        remove(&data, rec.id());
        assert!(!hashes_path(&data, rec.id()).exists());
    }

    /// A write that died after appending half an entry (and never committed its state)
    /// leaves bytes the next append cuts off first, so the reload is exact.
    #[tokio::test]
    async fn torn_append_is_cut_before_the_next() {
        let tmp = tempfile::tempdir().unwrap();
        let data = private_data(&tmp);
        let mut rec = streamed_record(&tmp).await;
        let h: Vec<ChunkHash> = (0..4u8).map(|i| [i + 1; 32]).collect();
        rec.set_hash_counts(&[(0, 2)], 0);
        let len = put(
            &data,
            &rec,
            0,
            &[unsaved(StreamRef::File(0), 0, &h[..2])],
            false,
        );
        // The torn write: half an entry, then nothing.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(hashes_path(&data, rec.id()))
                .unwrap();
            f.write_all(&[0xAA; ENTRY / 2]).unwrap();
        }
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(
            back.hashes.get(&0).unwrap(),
            &h[..2],
            "the tail is not vouched for"
        );
        assert_eq!(back.log_len, len);

        let mut rec = back;
        rec.set_hash_counts(&[(0, 4)], 0);
        let len = put(
            &data,
            &rec,
            len,
            &[unsaved(StreamRef::File(0), 2, &h[2..])],
            false,
        );
        assert_eq!(len, 4 * ENTRY as u64);
        assert_eq!(
            std::fs::metadata(hashes_path(&data, rec.id()))
                .unwrap()
                .len(),
            len,
            "the torn half entry was cut"
        );
        let back = load(&data, rec.id()).unwrap();
        assert_eq!(back.hashes.get(&0).unwrap(), &h);
    }

    /// Tombs load until they are 24 h old.
    #[test]
    fn tombs_expire() {
        let tmp = tempfile::tempdir().unwrap();
        let data = private_data(&tmp);
        let day = u64::try_from(MAX_AGE.as_millis()).unwrap();
        let fresh = Tomb {
            peer: peer(1),
            at_ms: NOW,
            wire_id: None,
        };
        let old = Tomb {
            peer: peer(1),
            at_ms: NOW - day - 1,
            wire_id: Some("c".repeat(32)),
        };
        write_tomb(&data, &"a".repeat(32), &fresh).unwrap();
        write_tomb(&data, &"b".repeat(32), &old).unwrap();
        let tombs = load_tombs(&data, NOW);
        assert_eq!(tombs.len(), 1);
        assert_eq!(tombs.get(&"a".repeat(32)), Some(&fresh));
        assert!(!tomb_path(&data, &"b".repeat(32)).exists());
        remove_tomb(&data, &"a".repeat(32));
        assert!(load_tombs(&data, NOW).is_empty());
        assert!(load_all(&data, NOW).is_empty(), "tombs are not records");
    }

    /// A `404` completes a send only when every stream was verified; automatic resumes
    /// are for unreachable failures of path sends, at most [`MAX_AUTO_RESUMES`] times.
    #[tokio::test]
    async fn verified_streams_and_resume_rules() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("Mix");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("big.bin"), vec![1u8; 300_000]).unwrap();
        std::fs::write(src.join("small.txt"), b"note").unwrap();
        let (m, s) = build(&[&src]).await;
        let mut rec = SendRecord::new(peer(1), NOW, &m, &s).unwrap();
        assert!(!rec.all_streams_verified());
        rec.verified.push(0);
        assert!(
            !rec.all_streams_verified(),
            "the bundle is not verified yet"
        );
        rec.bundle_verified = true;
        assert!(rec.all_streams_verified());

        let mut sum = rec.summary();
        assert!(sum.resumes_at_start() && !sum.resumes_on_return());
        sum.failed = true;
        sum.failure_kind = Some(FailureKind::Stalled);
        assert!(!sum.resumes_on_return(), "a stall waits for a new send");
        sum.failure_kind = Some(FailureKind::Unreachable);
        assert!(sum.resumes_on_return());
        sum.auto_resumes = MAX_AUTO_RESUMES;
        assert!(!sum.resumes_on_return(), "automatic resumes are capped");
        assert!(rec.summary().may_match(&peer(1), &m));
        assert!(!rec.summary().may_match(&peer(2), &m));
    }

    #[test]
    fn write_reload_and_sweep() {
        let tmp = tempfile::tempdir().unwrap();
        let data = private_data(&tmp);
        let mk = |id: &str, created: u64| {
            SendRecord::new(peer(1), created, &empty_manifest(id), &[]).unwrap()
        };
        let fresh = mk("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", NOW);
        let day = u64::try_from(MAX_AGE.as_millis()).unwrap();
        let old = mk("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", NOW - day - 1);
        write_whole(&data, &fresh).unwrap();
        write_whole(&data, &old).unwrap();
        // A second write replaces the first in place.
        let mut failed = fresh.clone();
        failed.failed_at_ms = Some(NOW);
        failed.failure = Some("receiver unreachable".into());
        write_whole(&data, &failed).unwrap();
        assert_eq!(load(&data, fresh.id()), Some(failed.clone()));
        std::fs::write(
            dir(&data).join("cccccccccccccccccccccccccccccccc.json"),
            b"{",
        )
        .unwrap();
        // A hash log whose record is gone (a crash between the two deletes).
        std::fs::write(hashes_path(&data, &"e".repeat(32)), [0u8; ENTRY]).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                path_of(&data, fresh.id()),
                state_path(&data, fresh.id()),
                hashes_path(&data, fresh.id()),
            ] {
                let mode = std::fs::metadata(path).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o600, "records are owner-only");
            }
        }

        let all = load_all(&data, NOW);
        assert_eq!(all, vec![failed]);
        let mut left: Vec<_> = std::fs::read_dir(dir(&data))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        left.sort();
        let a = "a".repeat(32);
        assert_eq!(
            left,
            vec![
                format!("{a}.hashes"),
                format!("{a}.json"),
                format!("{a}.state")
            ],
            "the expired and the unreadable record and the orphan log are deleted"
        );
        remove(&data, fresh.id());
        assert!(load(&data, fresh.id()).is_none());
    }
}
