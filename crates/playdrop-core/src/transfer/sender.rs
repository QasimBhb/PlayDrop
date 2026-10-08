//! Send side of the transfer plane: per-file chunk hashes, encoders that start at any
//! verified offset, and the resume decision of protocol.md 7.6 (SR-21).

use std::io::SeekFrom;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::AsyncSeekExt;

use super::bundle::{BundleLayout, BundleWriter, MemberOpener, MemberReader, OpenFuture};
use super::chunk::{chunk_count, prefix_hash, root_hash, ChunkEncoder, ChunkHash};
use super::manifest::{Manifest, SourceInfo};
use super::receiver::{FileState, TransferStateView};
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
            manifest,
            sources: Arc::new(sources),
            bundle_hashes: Vec::new(),
            layout,
            chunk_size,
        })
    }

    /// The manifest being sent.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Re-checks one source against its recorded size, mtime and file ID (SR-21).
    pub async fn check_source(&self, fid: u32) -> Result<SourceStatus> {
        let src = self.source(fid)?;
        let meta = match tokio::fs::symlink_metadata(&src.local_path).await {
            Ok(m) if m.is_file() => m,
            Ok(_) => return Ok(SourceStatus::Resized),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(SourceStatus::Resized),
            Err(e) => return Err(e.into()),
        };
        let now = SourceInfo::from_metadata(src.local_path.clone(), &meta);
        Ok(if now.size != src.size {
            SourceStatus::Resized
        } else if now.mtime_ms != src.mtime_ms || now.file_id != src.file_id {
            SourceStatus::Modified
        } else {
            SourceStatus::Unchanged
        })
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
        if self.check_source(fid).await? != SourceStatus::Unchanged {
            return Err(Error::BadRequest(format!(
                "source of fid {fid} changed since the offer"
            )));
        }
        let prior = self.prior(fid, from)?;
        let mut file = tokio::fs::File::open(&self.source(fid)?.local_path).await?;
        if from > 0 {
            file.seek(SeekFrom::Start(from)).await?;
        }
        ChunkEncoder::new(file, from, size, self.chunk_size, prior)
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
        if let Some(slot) = self.hashes.get_mut(fid as usize) {
            *slot = hashes;
        }
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
        self.bundle_hashes = hashes;
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
    pub async fn plan_resume(&self, remote: &TransferStateView) -> Result<ResumePlan> {
        if remote.id != self.manifest.id {
            return Err(Error::NotFound);
        }
        if remote.chunk_size != self.chunk_size {
            return Err(Error::BadRequest("chunk size changed".into()));
        }
        let mut files = Vec::new();
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
                        SourceStatus::Unchanged => decide(
                            r.verified_offset,
                            f.size,
                            r.prefix.as_deref(),
                            &self.hashes[f.fid as usize],
                            self.chunk_size,
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
                        SourceStatus::Unchanged => decide(
                            rb.verified_offset,
                            layout.total(),
                            rb.prefix.as_deref(),
                            &self.bundle_hashes,
                            self.chunk_size,
                        ),
                    }
                }
            }),
            (Some(_), None) => {
                return Err(Error::BadRequest("remote state lacks the bundle".into()))
            }
            _ => None,
        };
        Ok(ResumePlan { files, bundle })
    }
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
    // An offset at the size means the bytes arrived but the root was never confirmed; a
    // range cannot start there, so the only safe move is a restart.
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

/// Opens bundle members from the recorded source paths.
#[derive(Debug)]
struct FsOpener {
    sources: Arc<Vec<SourceInfo>>,
}

impl MemberOpener for FsOpener {
    fn open(&self, fid: u32, offset: u64) -> OpenFuture {
        let path: Option<PathBuf> = self.sources.get(fid as usize).map(|s| s.local_path.clone());
        Box::pin(async move {
            let path = path.ok_or_else(|| std::io::Error::other("unknown fid"))?;
            let mut f = tokio::fs::File::open(path).await?;
            if offset > 0 {
                f.seek(SeekFrom::Start(offset)).await?;
            }
            Ok(Box::new(f) as MemberReader)
        })
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
}
