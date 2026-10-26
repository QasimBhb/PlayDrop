//! Send side of the transfer plane: per-file chunk hashes, encoders that start at any
//! verified offset, and the resume decision of protocol.md 7.6 (SR-21).

use std::io::SeekFrom;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncSeekExt};

use super::bundle::{BundleLayout, BundleWriter, MemberOpener, OpenFuture};
use super::chunk::{chunk_count, prefix_hash, root_hash, ChunkEncoder, ChunkHash};
use super::manifest::{Manifest, SourceInfo};
use super::receiver::{FileState, TransferStateView};
use super::source::SourceRef;
use crate::{Error, Result};

/// What the sender does with one file (or the bundle) after a drop (protocol.md 7.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeAction {
    /// Nothing to send: verified, skipped or already failed on the receiver.
    Done,
    /// `PUT` from this offset, re-hashing only from there.
    Resume {
        /// The receiver's verified offset (0 for a fresh start).
        from: u64,
    },
    /// `PUT` from 0; the receiver discards its `.part` (prefix, mtime or inode differs, or
    /// local hashes are lost).
    Restart,
    /// The source size changed: cancel the transfer with `source_changed` and re-offer
    /// (protocol.md 7.6, section 11 SR-21 deviation).
    Cancel,
}

/// Resume plan for a whole transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumePlan {
    /// One action per streamed (not bundled) file, by fid.
    pub files: Vec<(u32, ResumeAction)>,
    /// Action for the bundle, when there is one.
    pub bundle: Option<ResumeAction>,
    /// Streams planned as a restart only because this sender lost their hashes below the
    /// receiver's offset; see [`TransferSender::plan_resume`].
    pub rebuild: Vec<LostHashes>,
}

impl ResumePlan {
    /// True when any entry says the transfer must be cancelled and re-offered.
    pub fn must_cancel(&self) -> bool {
        self.files.iter().any(|(_, a)| *a == ResumeAction::Cancel)
            || self.bundle == Some(ResumeAction::Cancel)
    }
}

/// How a source compares with what was recorded at offer time (SR-21).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceStatus {
    /// Size, mtime and file ID all match.
    Unchanged,
    /// Same size but mtime or file ID differs: restart at 0.
    Modified,
    /// Size differs or the file is gone: cancel.
    Resized,
}

/// Sender state for one transfer (protocol.md 7.4 to 7.7): the manifest, the recorded
/// sources, and every chunk hash sent, so resume and roots never re-read a file.
#[derive(Debug)]
pub struct TransferSender {
    manifest: Manifest,
    sources: Arc<Vec<SourceInfo>>,
    hashes: Vec<Vec<ChunkHash>>,
    bundle_hashes: Vec<ChunkHash>,
    layout: Option<Arc<BundleLayout>>,
    chunk_size: u32,
    /// Per fid, the lowest hash index that changed since [`take_unsaved`] last ran
    /// (`usize::MAX`: none), so a record keeps its hashes by appending only what is new.
    ///
    /// [`take_unsaved`]: Self::take_unsaved
    unsaved: Vec<usize>,
    /// The same for the bundle.
    bundle_unsaved: usize,
}

/// Hashes of one stream that changed since they were last saved: `hashes` replace
/// everything from index `start` on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsavedHashes {
    /// Which stream.
    pub stream: StreamRef,
    /// The first changed index.
    pub start: usize,
    /// The hashes from `start` to the end of the stream's list.
    pub hashes: Vec<ChunkHash>,
}

impl TransferSender {
    /// Creates the sender state. `sources[fid]` must describe `manifest.files[fid]`.
    /// `chunk_size` is the binding size from the receiver's answer (protocol.md 7.2).
    pub fn new(manifest: Manifest, sources: Vec<SourceInfo>, chunk_size: u32) -> Result<Self> {
        if sources.len() != manifest.files.len() {
            return Err(Error::BadRequest("one source per manifest file".into()));
        }
        if !super::manifest::is_valid_chunk_size(chunk_size) {
            return Err(Error::BadRequest(format!("chunk size {chunk_size}")));
        }
        let layout = match manifest.bundle {
            Some(_) => Some(Arc::new(BundleLayout::new(&manifest)?)),
            None => None,
        };
        Ok(Self {
            hashes: vec![Vec::new(); manifest.files.len()],
            unsaved: vec![usize::MAX; manifest.files.len()],
            manifest,
            sources: Arc::new(sources),
            bundle_hashes: Vec::new(),
            bundle_unsaved: usize::MAX,
            layout,
            chunk_size,
        })
    }

    /// The manifest being sent.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The binding chunk size from the receiver's answer.
    pub fn chunk_size(&self) -> u32 {
        self.chunk_size
    }

    /// Re-checks one source against its recorded size, mtime and file ID (SR-21).
    pub async fn check_source(&self, fid: u32) -> Result<SourceStatus> {
        Ok(self.open_checked(fid).await?.0)
    }

    /// Compares a source with what was recorded at offer time and, when it is unchanged,
    /// returns it opened at offset 0, so a `PUT` calls a stream opener once (`docs/core-api.md`
    /// section 3). A path is checked without following symlinks before it is opened; a
    /// stream is checked through `fstat` of the handle it returns.
    async fn open_checked(&self, fid: u32) -> Result<(SourceStatus, Option<tokio::fs::File>)> {
        let src = self.source(fid)?;
        match &src.source {
            SourceRef::Path(path) => {
                let meta = match tokio::fs::symlink_metadata(path).await {
                    Ok(m) if m.is_file() => m,
                    Ok(_) => return Ok((SourceStatus::Resized, None)),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Ok((SourceStatus::Resized, None))
                    }
                    Err(e) => return Err(e.into()),
                };
                let status = compare(src, &meta);
                if status != SourceStatus::Unchanged {
                    return Ok((status, None));
                }
                Ok((status, Some(tokio::fs::File::open(path).await?)))
            }
            SourceRef::Opener(_) => {
                let file = match src.source.open().await {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        return Ok((SourceStatus::Resized, None))
                    }
                    Err(e) => return Err(e.into()),
                };
                let meta = file.metadata().await?;
                if !meta.is_file() {
                    return Ok((SourceStatus::Resized, None));
                }
                let status = compare(src, &meta);
                Ok((status, (status == SourceStatus::Unchanged).then_some(file)))
            }
        }
    }

    fn source(&self, fid: u32) -> Result<&SourceInfo> {
        self.sources.get(fid as usize).ok_or(Error::NotFound)
    }

    /// Opens an encoder for file `fid` from chunk-aligned offset `from` to end of file.
    /// The source must be unchanged since the offer. Starting above 0 needs the hashes of
    /// the chunks below `from`; starting at 0 forgets earlier hashes.
    pub async fn file_encoder(
        &mut self,
        fid: u32,
        from: u64,
    ) -> Result<ChunkEncoder<tokio::fs::File>> {
        let entry = self
            .manifest
            .files
            .get(fid as usize)
            .ok_or(Error::NotFound)?;
        if entry.in_bundle {
            return Err(Error::BadRequest(format!(
                "fid {fid} travels in the bundle"
            )));
        }
        let size = entry.size;
        let (status, file) = self.open_checked(fid).await?;
        let Some(mut file) = file.filter(|_| status == SourceStatus::Unchanged) else {
            return Err(Error::BadRequest(format!(
                "source of fid {fid} changed since the offer"
            )));
        };
        let prior = self.prior(fid, from)?;
        if from > 0 {
            file.seek(SeekFrom::Start(from)).await?;
        }
        ChunkEncoder::new(file, from, size, self.chunk_size, prior)
    }

    /// The hashes recorded for file `fid` (empty when none were sent yet).
    pub fn hashes(&self, fid: u32) -> &[ChunkHash] {
        self.hashes.get(fid as usize).map_or(&[], Vec::as_slice)
    }

    /// The hashes recorded for the bundle stream.
    pub fn bundle_hashes(&self) -> &[ChunkHash] {
        &self.bundle_hashes
    }

    /// The sources recorded at offer time, by fid.
    pub fn sources(&self) -> &[SourceInfo] {
        &self.sources
    }

    fn prior(&mut self, fid: u32, from: u64) -> Result<Vec<ChunkHash>> {
        let k = usize::try_from(from / u64::from(self.chunk_size))
            .map_err(|_| Error::TooLarge("offset".into()))?;
        let list = &mut self.hashes[fid as usize];
        if list.len() < k {
            return Err(Error::BadRequest(
                "chunk hashes below the offset are lost; restart".into(),
            ));
        }
        list.truncate(k);
        Ok(list.clone())
    }

    /// Stores the hashes an encoder for `fid` produced (from [`ChunkEncoder::into_hashes`]).
    /// Call it even after a cut stream: hashes of chunks read are valid source facts.
    pub fn record_hashes(&mut self, fid: u32, hashes: Vec<ChunkHash>) {
        self.set_stream(StreamRef::File(fid), hashes);
    }

    /// Replaces a stream's hashes, noting from which index they differ.
    fn set_stream(&mut self, stream: StreamRef, new: Vec<ChunkHash>) {
        let (list, mark) = match stream {
            StreamRef::File(fid) => match (
                self.hashes.get_mut(fid as usize),
                self.unsaved.get_mut(fid as usize),
            ) {
                (Some(l), Some(m)) => (l, m),
                _ => return,
            },
            StreamRef::Bundle => (&mut self.bundle_hashes, &mut self.bundle_unsaved),
        };
        let same = list.iter().zip(&new).take_while(|(a, b)| a == b).count();
        if same < new.len() {
            *mark = (*mark).min(same);
        }
        *list = new;
    }

    /// The hashes that changed since the last call, per stream, and marks them saved.
    pub fn take_unsaved(&mut self) -> Vec<UnsavedHashes> {
        let mut out = Vec::new();
        for (fid, (list, mark)) in self.hashes.iter().zip(self.unsaved.iter_mut()).enumerate() {
            if *mark < list.len() {
                out.push(UnsavedHashes {
                    stream: StreamRef::File(u32::try_from(fid).unwrap_or(u32::MAX)),
                    start: *mark,
                    hashes: list[*mark..].to_vec(),
                });
            }
            *mark = usize::MAX;
        }
        if self.bundle_unsaved < self.bundle_hashes.len() {
            out.push(UnsavedHashes {
                stream: StreamRef::Bundle,
                start: self.bundle_unsaved,
                hashes: self.bundle_hashes[self.bundle_unsaved..].to_vec(),
            });
        }
        self.bundle_unsaved = usize::MAX;
        out
    }

    /// Marks every held hash unsaved, so the next [`take_unsaved`](Self::take_unsaved)
    /// returns them all (a record rewritten from scratch).
    pub fn mark_all_unsaved(&mut self) {
        self.unsaved.fill(0);
        self.bundle_unsaved = 0;
    }

    /// Marks every held hash saved (they were loaded from the record).
    pub fn mark_all_saved(&mut self) {
        self.unsaved.fill(usize::MAX);
        self.bundle_unsaved = usize::MAX;
    }

    /// Root of file `fid` for the `pd-root` trailer, once every chunk hash is known.
    pub fn root(&self, fid: u32) -> Option<ChunkHash> {
        let size = self.manifest.files.get(fid as usize)?.size;
        let hashes = &self.hashes[fid as usize];
        (hashes.len() as u64 == chunk_count(size, self.chunk_size))
            .then(|| root_hash(size, self.chunk_size, hashes))
    }

    /// Opens an encoder over the deterministic bundle from `from` (protocol.md 7.7).
    pub fn bundle_encoder(&mut self, from: u64) -> Result<ChunkEncoder<BundleWriter>> {
        let layout = self.layout.clone().ok_or(Error::NotFound)?;
        let k = usize::try_from(from / u64::from(self.chunk_size))
            .map_err(|_| Error::TooLarge("offset".into()))?;
        if self.bundle_hashes.len() < k {
            return Err(Error::BadRequest(
                "bundle hashes below the offset are lost; restart".into(),
            ));
        }
        self.bundle_hashes.truncate(k);
        let opener = Arc::new(FsOpener {
            sources: self.sources.clone(),
        });
        let total = layout.total();
        let writer = BundleWriter::new(layout, opener, from)?;
        ChunkEncoder::new(
            writer,
            from,
            total,
            self.chunk_size,
            self.bundle_hashes.clone(),
        )
    }

    /// Stores the hashes a bundle encoder produced.
    pub fn record_bundle_hashes(&mut self, hashes: Vec<ChunkHash>) {
        self.set_stream(StreamRef::Bundle, hashes);
    }

    /// Bundle root for the `pd-root` trailer, once every chunk hash is known.
    pub fn bundle_root(&self) -> Option<ChunkHash> {
        let total = self.layout.as_ref()?.total();
        (self.bundle_hashes.len() as u64 == chunk_count(total, self.chunk_size))
            .then(|| root_hash(total, self.chunk_size, &self.bundle_hashes))
    }

    /// Decides, per unfinished file and for the bundle, whether to resume, restart or
    /// cancel (protocol.md 7.6, SR-21): the remote `prefix` must equal the prefix of the
    /// locally recorded hashes, and the source must still match its recorded size, mtime
    /// and file ID.
    ///
    /// A stream whose hashes below the receiver's offset this sender does not hold (a send
    /// killed mid-`PUT` records none for that stream) is planned as a restart and listed
    /// in [`ResumePlan::rebuild`]: the caller may rebuild those hashes from the unchanged
    /// source ([`HashRebuilder`]), install them and plan again, so a lost record costs
    /// one local read instead of a re-send from 0. The rebuilt prefix still has to match
    /// the receiver's before any byte is resumed (SR-21).
    pub async fn plan_resume(&self, remote: &TransferStateView) -> Result<ResumePlan> {
        if remote.id != self.manifest.id {
            return Err(Error::NotFound);
        }
        if remote.chunk_size != self.chunk_size {
            return Err(Error::BadRequest("chunk size changed".into()));
        }
        let mut files = Vec::new();
        let mut rebuild = Vec::new();
        for f in self
            .manifest
            .files
            .iter()
            .filter(|f| !f.in_bundle && f.size > 0)
        {
            let r = remote
                .files
                .iter()
                .find(|r| r.fid == f.fid)
                .ok_or_else(|| Error::BadRequest(format!("remote state lacks fid {}", f.fid)))?;
            let action = match r.state {
                FileState::Verified | FileState::Skipped | FileState::Failed => ResumeAction::Done,
                FileState::Waiting | FileState::Receiving => {
                    match self.check_source(f.fid).await? {
                        SourceStatus::Resized => ResumeAction::Cancel,
                        SourceStatus::Modified if r.verified_offset == 0 => {
                            ResumeAction::Resume { from: 0 }
                        }
                        SourceStatus::Modified => ResumeAction::Restart,
                        SourceStatus::Unchanged => self.decide_unchanged(
                            StreamRef::File(f.fid),
                            r.verified_offset,
                            f.size,
                            r.prefix.as_deref(),
                            &mut rebuild,
                        ),
                    }
                }
            };
            files.push((f.fid, action));
        }
        let bundle = match (&self.layout, &remote.bundle) {
            (Some(layout), Some(rb)) => Some(match rb.state {
                FileState::Verified | FileState::Skipped | FileState::Failed => ResumeAction::Done,
                FileState::Waiting | FileState::Receiving => {
                    let mut worst = SourceStatus::Unchanged;
                    for m in layout.members() {
                        match self.check_source(m.fid).await? {
                            SourceStatus::Resized => worst = SourceStatus::Resized,
                            SourceStatus::Modified if worst == SourceStatus::Unchanged => {
                                worst = SourceStatus::Modified;
                            }
                            _ => {}
                        }
                    }
                    match worst {
                        SourceStatus::Resized => ResumeAction::Cancel,
                        SourceStatus::Modified if rb.verified_offset == 0 => {
                            ResumeAction::Resume { from: 0 }
                        }
                        SourceStatus::Modified => ResumeAction::Restart,
                        SourceStatus::Unchanged => self.decide_unchanged(
                            StreamRef::Bundle,
                            rb.verified_offset,
                            layout.total(),
                            rb.prefix.as_deref(),
                            &mut rebuild,
                        ),
                    }
                }
            }),
            (Some(_), None) => {
                return Err(Error::BadRequest("remote state lacks the bundle".into()))
            }
            _ => None,
        };
        Ok(ResumePlan {
            files,
            bundle,
            rebuild,
        })
    }

    /// [`plan_resume`](Self::plan_resume), then rebuilds every lost prefix from the
    /// source, installs it and plans again. A rebuild that fails leaves that stream as a
    /// restart. For callers that may hold the sender while the source is read; the host
    /// uses the steps separately so a long read does not lock the send.
    pub async fn plan_resume_rebuilding(
        &mut self,
        remote: &TransferStateView,
    ) -> Result<ResumePlan> {
        let plan = self.plan_resume(remote).await?;
        if plan.rebuild.is_empty() {
            return Ok(plan);
        }
        let rebuilder = self.rebuilder();
        for lost in &plan.rebuild {
            match rebuilder.hashes(lost.stream, lost.offset).await {
                Ok(h) => self.install_hashes(lost.stream, h),
                Err(e) => {
                    tracing::debug!(stream = ?lost.stream, error = %e, "lost chunk hashes could not be rebuilt");
                }
            }
        }
        let mut plan = self.plan_resume(remote).await?;
        plan.rebuild.clear();
        Ok(plan)
    }

    /// The prefix decision for a stream whose source is unchanged. When the receiver
    /// reports a chunk-aligned prefix inside the stream and fewer hashes than that are held
    /// here, the stream is also listed in `rebuild`, so the hashes can be rebuilt instead
    /// of restarting.
    fn decide_unchanged(
        &self,
        stream: StreamRef,
        offset: u64,
        size: u64,
        prefix: Option<&str>,
        rebuild: &mut Vec<LostHashes>,
    ) -> ResumeAction {
        let held = match stream {
            StreamRef::File(fid) => self.hashes(fid),
            StreamRef::Bundle => &self.bundle_hashes,
        };
        let cs = u64::from(self.chunk_size);
        let lost = offset > 0
            && offset < size
            && offset.is_multiple_of(cs)
            && prefix.is_some()
            && usize::try_from(offset / cs).is_ok_and(|k| held.len() < k);
        if lost {
            rebuild.push(LostHashes { stream, offset });
        }
        decide(offset, size, prefix, held, self.chunk_size)
    }

    /// What rebuilding lost hashes needs, detached from the sender so the read runs
    /// without holding it.
    pub fn rebuilder(&self) -> HashRebuilder {
        HashRebuilder {
            sources: Arc::clone(&self.sources),
            layout: self.layout.clone(),
            chunk_size: self.chunk_size,
        }
    }

    /// Installs hashes rebuilt by a [`HashRebuilder`] (or copied from a running encoder),
    /// from chunk 0.
    pub fn install_hashes(&mut self, stream: StreamRef, hashes: Vec<ChunkHash>) {
        match stream {
            StreamRef::File(fid) => self.record_hashes(fid, hashes),
            StreamRef::Bundle => self.record_bundle_hashes(hashes),
        }
    }

    /// Brings the held hashes of a stream up to those of a running encoder (`all`, from
    /// chunk 0), copying only the new tail. Called during a long `PUT`, so the record
    /// holds hashes before the `PUT` ends (protocol.md 7.6).
    pub fn sync_hashes(&mut self, stream: StreamRef, all: &[ChunkHash]) {
        let (list, mark) = match stream {
            StreamRef::File(fid) => match (
                self.hashes.get_mut(fid as usize),
                self.unsaved.get_mut(fid as usize),
            ) {
                (Some(l), Some(m)) => (l, m),
                _ => return,
            },
            StreamRef::Bundle => (&mut self.bundle_hashes, &mut self.bundle_unsaved),
        };
        if all.len() >= list.len() && all[..list.len()] == list[..] {
            *mark = (*mark).min(list.len());
            list.extend_from_slice(&all[list.len()..]);
        } else {
            *mark = 0;
            list.clear();
            list.extend_from_slice(all);
        }
    }
}

/// One stream of a transfer: a streamed file or the bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamRef {
    /// A streamed (not bundled) file, by fid.
    File(u32),
    /// The tar bundle (protocol.md 7.7).
    Bundle,
}

/// A stream whose hashes below the receiver's verified offset the sender lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LostHashes {
    /// Which stream.
    pub stream: StreamRef,
    /// The receiver's verified offset: hashes from 0 to here are needed.
    pub offset: u64,
}

/// Rebuilds lost chunk hashes from the sources (protocol.md 7.6): a file is read from 0
/// to the offset on this device only; the bundle is deterministic, so it is regenerated
/// from 0 the same way. Holds only shared handles, so it runs without the sender.
#[derive(Debug, Clone)]
pub struct HashRebuilder {
    sources: Arc<Vec<SourceInfo>>,
    layout: Option<Arc<BundleLayout>>,
    chunk_size: u32,
}

impl HashRebuilder {
    /// The chunk hashes of `stream` from 0 to `offset` (chunk-aligned).
    pub async fn hashes(&self, stream: StreamRef, offset: u64) -> Result<Vec<ChunkHash>> {
        let hashes = match stream {
            StreamRef::File(fid) => {
                let src = self.sources.get(fid as usize).ok_or(Error::NotFound)?;
                let f = src.source.open().await?;
                hash_prefix(f, offset, self.chunk_size).await?
            }
            StreamRef::Bundle => {
                let layout = self.layout.clone().ok_or(Error::NotFound)?;
                let opener = Arc::new(FsOpener {
                    sources: Arc::clone(&self.sources),
                });
                let w = BundleWriter::new(layout, opener, 0)?;
                hash_prefix(w, offset, self.chunk_size).await?
            }
        };
        tracing::debug!(
            ?stream,
            bytes = offset,
            "chunk hashes below the receiver's offset rebuilt from the source"
        );
        Ok(hashes)
    }
}

/// Chunk hashes of the first `end` bytes of `reader` (`end` chunk-aligned), computed the
/// same way a `PUT` computes them, so the result can stand in for hashes that were lost.
async fn hash_prefix<R: AsyncRead + Unpin>(
    reader: R,
    end: u64,
    chunk_size: u32,
) -> Result<Vec<ChunkHash>> {
    let mut enc = ChunkEncoder::new(reader, 0, end, chunk_size, Vec::new())?;
    while std::future::poll_fn(|cx| enc.poll_next_record(cx))
        .await?
        .is_some()
    {}
    Ok(enc.into_hashes())
}

/// Prefix comparison of protocol.md 7.6 for one stream.
fn decide(
    offset: u64,
    size: u64,
    remote_prefix: Option<&str>,
    local: &[ChunkHash],
    chunk_size: u32,
) -> ResumeAction {
    if offset == 0 {
        return ResumeAction::Resume { from: 0 };
    }
    // An offset at the size means every byte arrived but nothing was placed yet: either
    // the root never reached the receiver, or it verified and placing the files stopped
    // on an I/O error. A range cannot start at the size, so the request is a restart at 0
    // (the only start the wire allows). A receiver that holds a verified root does not
    // discard anything for it: it finishes placing the files and answers
    // `offset_mismatch` at the full size, and the next plan finds the stream verified.
    // Without a root the receiver starts again from 0.
    if offset >= size || !offset.is_multiple_of(u64::from(chunk_size)) {
        return ResumeAction::Restart;
    }
    let Ok(k) = usize::try_from(offset / u64::from(chunk_size)) else {
        return ResumeAction::Restart;
    };
    let Some(claimed) = remote_prefix.and_then(|p| super::hash_from_hex(p).ok()) else {
        return ResumeAction::Restart;
    };
    if local.len() < k {
        return ResumeAction::Restart;
    }
    if prefix_hash(offset, chunk_size, &local[..k]) == claimed {
        ResumeAction::Resume { from: offset }
    } else {
        ResumeAction::Restart
    }
}

/// Size, mtime and file ID of a source now against what was recorded at offer time
/// (SR-21). A recorded mtime of 0 (a stream with an unknown mtime) is not compared, and
/// neither is a file ID the platform did not expose.
fn compare(recorded: &SourceInfo, meta: &std::fs::Metadata) -> SourceStatus {
    if meta.len() != recorded.size {
        return SourceStatus::Resized;
    }
    let mtime_differs =
        recorded.mtime_ms != 0 && super::manifest::mtime_ms(meta) != recorded.mtime_ms;
    let id_now = super::manifest::file_id(meta);
    let id_differs = recorded.file_id.is_some() && id_now.is_some() && id_now != recorded.file_id;
    if mtime_differs || id_differs {
        SourceStatus::Modified
    } else {
        SourceStatus::Unchanged
    }
}

/// Opens bundle members from the recorded sources (paths or host openers).
#[derive(Debug)]
struct FsOpener {
    sources: Arc<Vec<SourceInfo>>,
}

impl MemberOpener for FsOpener {
    fn open(&self, fid: u32, offset: u64) -> OpenFuture {
        // Small members are read whole in one blocking call (bundle.rs, Phase 9 item 2).
        match self.sources.get(fid as usize) {
            Some(s) => super::bundle::open_source_member(s.source.clone(), s.size, offset),
            None => Box::pin(async { Err(std::io::Error::other("unknown fid")) }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::manifest::ManifestBuilder;
    use crate::transfer::receiver::{FileStateView, StreamStateView, TransferState};
    use tokio::io::AsyncReadExt;

    const CS: u32 = 64 * 1024;

    async fn setup(dir: &std::path::Path) -> (TransferSender, Vec<u8>) {
        let data = crate::transfer::chunk::tests::TestRng(3).bytes(4 * CS as usize + 50);
        let src = dir.join("src");
        std::fs::create_dir(&src).unwrap();
        std::fs::write(src.join("big"), &data).unwrap();
        std::fs::write(src.join("small"), b"tiny").unwrap();
        let mut b = ManifestBuilder::new(CS);
        b.add_path(&src).await.unwrap();
        let (m, sources) = b.build().unwrap();
        (TransferSender::new(m, sources, CS).unwrap(), data)
    }

    fn view(
        s: &TransferSender,
        offset: u64,
        prefix: Option<String>,
        boff: u64,
        bprefix: Option<String>,
    ) -> TransferStateView {
        TransferStateView {
            id: s.manifest().id.clone(),
            state: TransferState::Active,
            chunk_size: CS,
            total_bytes: s.manifest().total_bytes,
            bytes_verified: 0,
            files: vec![
                FileStateView {
                    fid: 0,
                    state: FileState::Receiving,
                    size: s.manifest().files[0].size,
                    verified_offset: offset,
                    root: None,
                    prefix,
                    token: None,
                },
                FileStateView {
                    fid: 1,
                    state: FileState::Waiting,
                    size: 4,
                    verified_offset: 0,
                    root: None,
                    prefix: None,
                    token: None,
                },
            ],
            bundle: Some(StreamStateView {
                state: FileState::Receiving,
                size: 2048,
                verified_offset: boff,
                root: None,
                prefix: bprefix,
                token: None,
            }),
        }
    }

    #[tokio::test]
    async fn resume_decisions() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, data) = setup(tmp.path()).await;
        let mut enc = s.file_encoder(0, 0).await.unwrap();
        let mut sink = Vec::new();
        enc.read_to_end(&mut sink).await.unwrap();
        let hashes = enc.into_hashes();
        s.record_hashes(0, hashes.clone());
        assert!(s.root(0).is_some());

        let off = 2 * u64::from(CS);
        let good = crate::transfer::hash_to_hex(&prefix_hash(off, CS, &hashes[..2]));
        let plan = s
            .plan_resume(&view(&s, off, Some(good.clone()), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files, vec![(0, ResumeAction::Resume { from: off })]);
        assert_eq!(plan.bundle, Some(ResumeAction::Resume { from: 0 }));

        let bad = crate::transfer::hash_to_hex(&[9; 32]);
        let plan = s
            .plan_resume(&view(&s, off, Some(bad), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Restart);
        let plan = s
            .plan_resume(&view(&s, off + 1, Some(good.clone()), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Restart);

        // Resuming the encoder from the offset reproduces the rest.
        let full_root = s.root(0).unwrap();
        let mut enc = s.file_encoder(0, off).await.unwrap();
        let mut tail = Vec::new();
        enc.read_to_end(&mut tail).await.unwrap();
        assert_eq!(enc.root(data.len() as u64), Some(full_root));
        s.record_hashes(0, enc.into_hashes());
        assert_eq!(s.root(0), Some(full_root));

        // Same size, new mtime: restart. New size: cancel.
        let path = tmp.path().join("src").join("big");
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000))
            .unwrap();
        drop(f);
        let plan = s
            .plan_resume(&view(&s, off, Some(good.clone()), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Restart);
        assert!(s.file_encoder(0, 0).await.is_err());
        std::fs::write(&path, b"shorter").unwrap();
        let plan = s
            .plan_resume(&view(&s, off, Some(good), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Cancel);
        assert!(plan.must_cancel());
    }

    /// Chunk hashes of `bytes` as a `PUT` computes them.
    fn chunk_hashes(bytes: &[u8]) -> Vec<ChunkHash> {
        bytes
            .chunks(CS as usize)
            .map(|c| *blake3::hash(c).as_bytes())
            .collect()
    }

    /// A sender that lost the hashes below the receiver's offset (killed mid-`PUT`)
    /// rebuilds them from the unchanged source and resumes when the receiver's prefix
    /// matches; a receiver prefix that differs still restarts at 0 (SR-21).
    #[tokio::test]
    async fn lost_hashes_are_rebuilt_from_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, data) = setup(tmp.path()).await;
        assert!(s.hashes(0).is_empty(), "nothing recorded yet");
        let off = 2 * u64::from(CS);
        let good = prefix_hash(off, CS, &chunk_hashes(&data)[..2]);
        let good = crate::transfer::hash_to_hex(&good);
        let plan = s
            .plan_resume(&view(&s, off, Some(good.clone()), 0, None))
            .await
            .unwrap();
        assert_eq!(
            plan.files[0].1,
            ResumeAction::Restart,
            "lost, not yet rebuilt"
        );
        assert_eq!(
            plan.rebuild,
            vec![LostHashes {
                stream: StreamRef::File(0),
                offset: off
            }]
        );
        let plan = s
            .plan_resume_rebuilding(&view(&s, off, Some(good), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Resume { from: off });
        assert_eq!(s.hashes(0), &chunk_hashes(&data)[..2]);
        // The resumed encoder carries on from the rebuilt hashes to the right root.
        let mut enc = s.file_encoder(0, off).await.unwrap();
        let mut tail = Vec::new();
        enc.read_to_end(&mut tail).await.unwrap();
        let full = root_hash(data.len() as u64, CS, &chunk_hashes(&data));
        assert_eq!(enc.root(data.len() as u64), Some(full));

        let again = tmp.path().join("again");
        std::fs::create_dir(&again).unwrap();
        let (mut s, _) = setup(&again).await;
        let bad = crate::transfer::hash_to_hex(&[5; 32]);
        let plan = s
            .plan_resume_rebuilding(&view(&s, off, Some(bad), 0, None))
            .await
            .unwrap();
        assert_eq!(plan.files[0].1, ResumeAction::Restart);
    }

    /// A running encoder's hashes are copied into the sender a tail at a time, and a
    /// restarted encoder's replace them.
    #[tokio::test]
    async fn sync_hashes_follows_the_encoder() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, data) = setup(tmp.path()).await;
        let all = chunk_hashes(&data);
        s.sync_hashes(StreamRef::File(0), &all[..1]);
        s.sync_hashes(StreamRef::File(0), &all[..3]);
        assert_eq!(s.hashes(0), &all[..3]);
        let unsaved = s.take_unsaved();
        assert_eq!(unsaved.len(), 1);
        assert_eq!((unsaved[0].start, unsaved[0].hashes.len()), (0, 3));
        s.sync_hashes(StreamRef::File(0), &all[..4]);
        let unsaved = s.take_unsaved();
        assert_eq!((unsaved[0].start, &unsaved[0].hashes[..]), (3, &all[3..4]));
        assert_eq!(s.take_unsaved(), Vec::new(), "nothing new");
        s.sync_hashes(StreamRef::File(0), &[[1; 32]]);
        assert_eq!(s.hashes(0), &[[1; 32]]);
        assert_eq!(
            s.take_unsaved()[0].start,
            0,
            "a replaced list is saved from 0"
        );
        // The same hashes again (a `PUT` end) change nothing.
        s.record_hashes(0, vec![[1; 32]]);
        assert_eq!(s.take_unsaved(), Vec::new());
    }

    /// The same for the bundle, which is regenerated from 0 (protocol.md 7.7).
    #[tokio::test]
    async fn lost_bundle_hashes_are_rebuilt() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        std::fs::create_dir(&src).unwrap();
        for i in 0..3u64 {
            let bytes = crate::transfer::chunk::tests::TestRng(20 + i).bytes(100_000);
            std::fs::write(src.join(format!("m{i}")), bytes).unwrap();
        }
        let mut b = ManifestBuilder::new(CS);
        b.add_path(&src).await.unwrap();
        let (m, sources) = b.build().unwrap();
        let total = m.bundle.unwrap().size;
        assert!(total > 2 * u64::from(CS));
        let mut first = TransferSender::new(m.clone(), sources.clone(), CS).unwrap();
        let mut enc = first.bundle_encoder(0).unwrap();
        let mut body = Vec::new();
        enc.read_to_end(&mut body).await.unwrap();
        let all = enc.into_hashes();

        let mut s = TransferSender::new(m, sources, CS).unwrap();
        let off = 2 * u64::from(CS);
        let remote = TransferStateView {
            id: s.manifest().id.clone(),
            state: TransferState::Active,
            chunk_size: CS,
            total_bytes: s.manifest().total_bytes,
            bytes_verified: 0,
            files: (0..3)
                .map(|fid| FileStateView {
                    fid,
                    state: FileState::Waiting,
                    size: 100_000,
                    verified_offset: 0,
                    root: None,
                    prefix: None,
                    token: None,
                })
                .collect(),
            bundle: Some(StreamStateView {
                state: FileState::Receiving,
                size: total,
                verified_offset: off,
                root: None,
                prefix: Some(crate::transfer::hash_to_hex(&prefix_hash(
                    off,
                    CS,
                    &all[..2],
                ))),
                token: None,
            }),
        };
        let plan = s.plan_resume_rebuilding(&remote).await.unwrap();
        assert_eq!(plan.bundle, Some(ResumeAction::Resume { from: off }));
        assert_eq!(s.bundle_hashes(), &all[..2]);
    }

    #[tokio::test]
    async fn bundle_from_files_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = setup(tmp.path()).await;
        let mut enc = s.bundle_encoder(0).unwrap();
        let mut body = Vec::new();
        enc.read_to_end(&mut body).await.unwrap();
        s.record_bundle_hashes(enc.into_hashes());
        assert!(s.bundle_root().is_some());
        let total = s.manifest().bundle.unwrap().size;
        assert_eq!(body.len() as u64, total + 32);
    }

    /// Opens a temp file, as a host's `content://` opener would.
    #[derive(Debug)]
    struct FileOpener(std::path::PathBuf);

    impl crate::transfer::SourceOpener for FileOpener {
        fn open(&self) -> std::io::Result<std::fs::File> {
            std::fs::File::open(&self.0)
        }
    }

    fn stream(path: &std::path::Path, name: &str, mtime_ms: i64) -> crate::transfer::StreamSource {
        crate::transfer::StreamSource {
            name: name.into(),
            size: std::fs::metadata(path).unwrap().len(),
            mtime_ms,
            mime: Some("application/octet-stream".into()),
            opener: Arc::new(FileOpener(path.to_path_buf())),
        }
    }

    /// SR-21 for a stream source: size always, mtime only when known, file ID when the
    /// platform has one; the encoder reads through the opener from the resume offset.
    #[tokio::test]
    async fn stream_source_resume_check() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("shared.bin");
        let data = crate::transfer::chunk::tests::TestRng(9).bytes(4 * CS as usize + 7);
        std::fs::write(&path, &data).unwrap();
        let mut b = ManifestBuilder::new(CS);
        b.add_stream(stream(&path, "photo.jpg", 1_700_000_000_000))
            .await
            .unwrap();
        let (m, sources) = b.build().unwrap();
        assert_eq!(m.files[0].path, ["photo.jpg"]);
        assert_eq!(m.files[0].mtime, 1_700_000_000_000);
        assert_eq!(m.files[0].mime.as_deref(), Some("application/octet-stream"));
        assert!(sources[0].source.is_stream());
        let mut s = TransferSender::new(m, sources, CS).unwrap();
        let mut enc = s.file_encoder(0, 0).await.unwrap();
        let mut all = Vec::new();
        enc.read_to_end(&mut all).await.unwrap();
        s.record_hashes(0, enc.into_hashes());
        let full = s.root(0).unwrap();
        let off = 2 * u64::from(CS);
        let mut tail = s.file_encoder(0, off).await.unwrap();
        let mut rest = Vec::new();
        tail.read_to_end(&mut rest).await.unwrap();
        assert_eq!(
            tail.root(data.len() as u64),
            Some(full),
            "resume reproduces the root"
        );
        assert_eq!(s.check_source(0).await.unwrap(), SourceStatus::Unchanged);

        // A known mtime that moves: restart. A size change: cancel.
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(5))
            .unwrap();
        drop(f);
        assert_eq!(s.check_source(0).await.unwrap(), SourceStatus::Modified);
        assert!(s.file_encoder(0, 0).await.is_err());
        std::fs::write(&path, b"short").unwrap();
        assert_eq!(s.check_source(0).await.unwrap(), SourceStatus::Resized);

        // Unknown mtime (0): touching the file does not count as a change.
        std::fs::write(&path, &data).unwrap();
        let mut b = ManifestBuilder::new(CS);
        b.add_stream(stream(&path, "doc.pdf", 0)).await.unwrap();
        let (m, sources) = b.build().unwrap();
        assert_eq!(sources[0].mtime_ms, 0);
        let s = TransferSender::new(m, sources, CS).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(9))
            .unwrap();
        drop(f);
        assert_eq!(s.check_source(0).await.unwrap(), SourceStatus::Unchanged);
    }

    /// A stream's name is one SR-17 component; a declared size that differs from the opened
    /// file is refused at offer time.
    #[tokio::test]
    async fn stream_names_follow_sr17() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("x");
        std::fs::write(&path, b"abc").unwrap();
        for bad in [
            "",
            "..",
            "a/b",
            "a\\b",
            "CON",
            "x.txt.",
            "evil\u{202E}gpj.exe",
            "a:b",
        ] {
            let mut b = ManifestBuilder::new(CS);
            let r = b.add_stream(stream(&path, bad, 0)).await;
            assert!(matches!(r, Err(Error::PathInvalid(_))), "{bad:?} accepted");
        }
        let mut b = ManifestBuilder::new(CS);
        let mut src = stream(&path, "ok.txt", 0);
        src.size = 4;
        assert!(matches!(b.add_stream(src).await, Err(Error::BadRequest(_))));
        let mut b = ManifestBuilder::new(CS);
        b.add_stream(stream(&path, "ok.txt", 0)).await.unwrap();
        let (m, _) = b.build().unwrap();
        assert!(m.files[0].in_bundle, "small streams travel in the bundle");
    }
}
