//! Deterministic POSIX ustar bundle for small files, protocol.md 7.7 and SR-19.
//!
//! Members are the `inBundle` files in `fid` order, each named by its decimal `fid`.
//! Headers are regular-file ustar headers with `uid`/`gid` 0, empty `uname`/`gname`, mode
//! 0644 and `mtime` equal to the manifest `mtime` in whole seconds; no pax or GNU headers.
//! Two zero blocks end the archive. Because the bytes are a pure function of the manifest
//! and the file contents, the writer can regenerate any offset for resume, and the reader
//! can start at any verified offset.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf, Take};

use super::manifest::{Manifest, MAX_BUNDLE_MEMBER};
use super::source::SourceRef;
use crate::{Error, Result};

/// Size of one tar block.
pub const BLOCK: u64 = 512;
const TRAILER: u64 = 2 * BLOCK;

/// Largest value an 11-digit octal field holds.
const OCTAL11_MAX: u64 = 0o77_777_777_777;

/// Exact tar length for a manifest's `inBundle` files, computed without reading any file
/// (protocol.md 7.7 `bundle.size`).
pub fn bundle_size(manifest: &Manifest) -> u64 {
    manifest
        .bundle_members()
        .map(|f| BLOCK + f.size.div_ceil(BLOCK) * BLOCK)
        .sum::<u64>()
        + TRAILER
}

/// One bundle member's place in the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleMember {
    /// Manifest `fid`; also the member name.
    pub fid: u32,
    /// Member size.
    pub size: u64,
    /// `mtime` in whole seconds as written in the header.
    pub mtime_secs: u64,
    /// Archive offset of the member's header block.
    pub header_offset: u64,
}

impl BundleMember {
    /// Archive offset of the first data byte.
    pub fn data_offset(&self) -> u64 {
        self.header_offset + BLOCK
    }

    /// Archive offset just past the data.
    pub fn data_end(&self) -> u64 {
        self.data_offset() + self.size
    }

    /// Archive offset just past the zero padding.
    pub fn padded_end(&self) -> u64 {
        self.data_offset() + self.size.div_ceil(BLOCK) * BLOCK
    }

    /// Bytes of this member's data lying below archive offset `pos`.
    pub fn bytes_below(&self, pos: u64) -> u64 {
        pos.saturating_sub(self.data_offset()).min(self.size)
    }
}

/// Where an archive offset falls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Inside member `idx`'s header, `within` bytes in.
    Header {
        /// Member index (not fid).
        idx: usize,
        /// Offset inside the header block.
        within: u64,
    },
    /// Inside member `idx`'s data.
    Data {
        /// Member index (not fid).
        idx: usize,
        /// Offset inside the member file.
        within: u64,
    },
    /// Inside member `idx`'s zero padding.
    Pad {
        /// Member index (not fid).
        idx: usize,
        /// Padding bytes left.
        remaining: u64,
    },
    /// Inside the two-block trailer.
    Trailer {
        /// Trailer bytes left.
        remaining: u64,
    },
    /// At or past the end.
    End,
}

/// Deterministic layout of a manifest's bundle (protocol.md 7.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleLayout {
    members: Vec<BundleMember>,
    total: u64,
}

impl BundleLayout {
    /// Lays out the `inBundle` files of `manifest` in `fid` order.
    pub fn new(manifest: &Manifest) -> Result<Self> {
        let mut members = Vec::new();
        let mut pos = 0u64;
        for f in manifest.bundle_members() {
            if f.size > MAX_BUNDLE_MEMBER {
                return Err(Error::BadRequest(format!(
                    "fid {} too large for ustar",
                    f.fid
                )));
            }
            let m = BundleMember {
                fid: f.fid,
                size: f.size,
                mtime_secs: tar_mtime(f.mtime),
                header_offset: pos,
            };
            pos = m.padded_end();
            members.push(m);
        }
        Ok(Self {
            members,
            total: pos + TRAILER,
        })
    }

    /// Members in archive order.
    pub fn members(&self) -> &[BundleMember] {
        &self.members
    }

    /// The member carrying `fid`, if it is in the bundle (members are in fid order).
    pub fn member(&self, fid: u32) -> Option<&BundleMember> {
        self.members
            .binary_search_by_key(&fid, |m| m.fid)
            .ok()
            .map(|i| &self.members[i])
    }

    /// Exact archive length.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Finds which part of the archive `pos` falls in.
    pub fn locate(&self, pos: u64) -> Position {
        if pos >= self.total {
            return Position::End;
        }
        let idx = self.members.partition_point(|m| m.header_offset <= pos);
        if idx == 0 {
            // No member starts at or before pos: only possible with no members.
            return Position::Trailer {
                remaining: self.total - pos,
            };
        }
        let idx = idx - 1;
        let m = &self.members[idx];
        if pos < m.data_offset() {
            Position::Header {
                idx,
                within: pos - m.header_offset,
            }
        } else if pos < m.data_end() {
            Position::Data {
                idx,
                within: pos - m.data_offset(),
            }
        } else if pos < m.padded_end() {
            Position::Pad {
                idx,
                remaining: m.padded_end() - pos,
            }
        } else {
            Position::Trailer {
                remaining: self.total - pos,
            }
        }
    }

    /// Sum of member data bytes below archive offset `pos` (for `bytesVerified`, 7.5).
    pub fn content_bytes_below(&self, pos: u64) -> u64 {
        self.members.iter().map(|m| m.bytes_below(pos)).sum()
    }
}

/// Manifest `mtime` (Unix ms) to whole seconds for the header, floored and clamped to the
/// range an 11-digit octal field holds.
fn tar_mtime(ms: i64) -> u64 {
    u64::try_from(ms.div_euclid(1000)).map_or(0, |s| s.min(OCTAL11_MAX))
}

fn put_octal(field: &mut [u8], value: u64) {
    // Width minus one octal digits, zero-padded, then NUL.
    let digits = field.len() - 1;
    let s = format!("{value:0digits$o}");
    field[..digits].copy_from_slice(s.as_bytes());
    field[digits] = 0;
}

/// Builds the deterministic ustar header for one member (protocol.md 7.7).
pub fn member_header(m: &BundleMember) -> [u8; 512] {
    let mut h = [0u8; 512];
    let name = m.fid.to_string();
    h[..name.len()].copy_from_slice(name.as_bytes());
    put_octal(&mut h[100..108], 0o644);
    put_octal(&mut h[108..116], 0);
    put_octal(&mut h[116..124], 0);
    put_octal(&mut h[124..136], m.size);
    put_octal(&mut h[136..148], m.mtime_secs);
    h[156] = b'0';
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    put_octal(&mut h[329..337], 0);
    put_octal(&mut h[337..345], 0);
    h[148..156].fill(b' ');
    let sum: u64 = h.iter().map(|&b| u64::from(b)).sum();
    let s = format!("{sum:06o}");
    h[148..154].copy_from_slice(s.as_bytes());
    h[154] = 0;
    h[155] = b' ';
    h
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        return None; // GNU base-256 is not ustar.
    }
    let mut it = field.iter().copied().skip_while(|&b| b == b' ').peekable();
    let mut value: u64 = 0;
    let mut any = false;
    while let Some(&b) = it.peek() {
        if !(b'0'..=b'7').contains(&b) {
            break;
        }
        value = value.checked_mul(8)?.checked_add(u64::from(b - b'0'))?;
        any = true;
        it.next();
    }
    // Only NUL or space may follow the digits.
    if !any || it.any(|b| b != 0 && b != b' ') {
        return None;
    }
    Some(value)
}

fn check_header(h: &[u8], m: &BundleMember) -> Result<()> {
    let bad = |why: String| Err(Error::BundleMismatch(why));
    let fid = m.fid;
    if h.iter().all(|&b| b == 0) {
        return bad(format!("archive ended before member {fid}"));
    }
    let stored = parse_octal(&h[148..156]);
    let computed: u64 = h
        .iter()
        .enumerate()
        .map(|(i, &b)| {
            if (148..156).contains(&i) {
                u64::from(b' ')
            } else {
                u64::from(b)
            }
        })
        .sum();
    if stored != Some(computed) {
        return bad(format!("member {fid} header checksum"));
    }
    if &h[257..263] != b"ustar\0" || &h[263..265] != b"00" {
        return bad(format!("member {fid} is not a ustar header"));
    }
    let typeflag = h[156];
    if typeflag != b'0' && typeflag != 0 {
        return bad(format!(
            "member {fid} is not a regular file (type {:?})",
            char::from(typeflag)
        ));
    }
    let name_end = h[..100].iter().position(|&b| b == 0).unwrap_or(100);
    if h[345] != 0 || h[..name_end] != *fid.to_string().as_bytes() {
        let shown = String::from_utf8_lossy(&h[..name_end]);
        return bad(format!("expected member {fid}, found {shown:?}"));
    }
    if parse_octal(&h[124..136]) != Some(m.size) {
        return bad(format!("member {fid} size differs from the manifest"));
    }
    Ok(())
}

/// What the bundle reader found in a run of verified bytes (protocol.md 7.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BundleEvent {
    /// A header for `fid` validated; its data starts fresh.
    MemberStart {
        /// Manifest fid.
        fid: u32,
    },
    /// File bytes for `fid` at `offset` within that file.
    Data {
        /// Manifest fid.
        fid: u32,
        /// Offset inside the member file.
        offset: u64,
        /// The bytes (a zero-copy slice of the fed buffer).
        bytes: Bytes,
    },
    /// `fid`'s data is complete.
    MemberEnd {
        /// Manifest fid.
        fid: u32,
    },
}

/// Streaming bundle parser that checks every header against the manifest (SR-19).
///
/// Feed it the already-verified chunk bytes, in order, from any start offset that is not
/// inside a header (chunk offsets never are, as chunk sizes are multiples of 512). It never
/// trusts names in the archive: it only checks that each member is the one the manifest
/// predicts. Any deviation fails with [`Error::BundleMismatch`].
#[derive(Debug)]
pub struct BundleReader {
    layout: Arc<BundleLayout>,
    pos: u64,
    header: Vec<u8>,
}

impl BundleReader {
    /// Starts parsing at archive offset `start`.
    pub fn new(layout: Arc<BundleLayout>, start: u64) -> Result<Self> {
        if let Position::Header { within, .. } = layout.locate(start) {
            if within != 0 {
                return Err(Error::BadRequest(format!(
                    "bundle offset {start} is inside a header"
                )));
            }
        }
        if start > layout.total() {
            return Err(Error::BadRequest(format!("bundle offset {start} past end")));
        }
        Ok(Self {
            layout,
            pos: start,
            header: Vec::with_capacity(512),
        })
    }

    /// Archive offset of the next byte expected.
    pub fn position(&self) -> u64 {
        self.pos
    }

    /// True when the whole archive, trailer included, has been consumed.
    pub fn is_complete(&self) -> bool {
        self.pos == self.layout.total()
    }

    /// Consumes the next verified bytes and returns what they contained.
    pub fn feed(&mut self, mut data: Bytes) -> Result<Vec<BundleEvent>> {
        let mut events = Vec::new();
        while !data.is_empty() {
            match self.layout.locate(self.pos) {
                Position::Header { idx, within } => {
                    let take = usize::try_from(BLOCK - within)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    self.header.extend_from_slice(&data.split_to(take));
                    self.pos += take as u64;
                    if self.header.len() == 512 {
                        let m = self.layout.members[idx];
                        check_header(&self.header, &m)?;
                        self.header.clear();
                        events.push(BundleEvent::MemberStart { fid: m.fid });
                        if m.size == 0 {
                            events.push(BundleEvent::MemberEnd { fid: m.fid });
                        }
                    }
                }
                Position::Data { idx, within } => {
                    let m = self.layout.members[idx];
                    let take = usize::try_from(m.size - within)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    events.push(BundleEvent::Data {
                        fid: m.fid,
                        offset: within,
                        bytes: data.split_to(take),
                    });
                    self.pos += take as u64;
                    if self.pos == m.data_end() {
                        events.push(BundleEvent::MemberEnd { fid: m.fid });
                    }
                }
                Position::Pad { remaining, .. } => {
                    // Padding content is ignored on receipt, like mode bits.
                    let take = usize::try_from(remaining)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    let _ = data.split_to(take);
                    self.pos += take as u64;
                }
                Position::Trailer { remaining } => {
                    let take = usize::try_from(remaining)
                        .unwrap_or(usize::MAX)
                        .min(data.len());
                    if data[..take].iter().any(|&b| b != 0) {
                        return Err(Error::BundleMismatch(
                            "extra member or data after the last member".into(),
                        ));
                    }
                    let _ = data.split_to(take);
                    self.pos += take as u64;
                }
                Position::End => {
                    return Err(Error::BundleMismatch(
                        "data past the end of the archive".into(),
                    ));
                }
            }
        }
        Ok(events)
    }
}

/// Boxed member reader returned by a [`MemberOpener`].
pub type MemberReader = Box<dyn AsyncRead + Send + Unpin>;

/// Future returned by [`MemberOpener::open`].
pub type OpenFuture = Pin<Box<dyn Future<Output = io::Result<MemberReader>> + Send>>;

/// Opens a bundle member's source bytes positioned at `offset` (protocol.md 7.7). The
/// sender implements this over its recorded sources.
pub trait MemberOpener: Send + Sync {
    /// Opens `fid` and seeks to `offset`.
    fn open(&self, fid: u32, offset: u64) -> OpenFuture;
}

/// Largest rest of a member that [`open_source_member`] reads whole in one blocking call.
/// Bundle members are at most a few hundred KiB in practice (the manifest's bundle
/// threshold), so this covers nearly all of them while bounding the memory held per
/// member to one small buffer.
pub const READ_WHOLE_MAX: u64 = 256 * 1024;

/// Opens bundle member bytes from `source` at `offset`, for a member of `size` bytes (its
/// manifest size): the sender's [`MemberOpener`] over recorded sources.
///
/// When at most [`READ_WHOLE_MAX`] bytes are left, the source is opened, read and closed
/// in one call on the blocking pool and the bytes are served from memory. Through
/// `tokio::fs` the same member costs a pool hop for the open, the seek, each read and the
/// close, which for thousands of 4 to 64 KiB members is most of the sender's per-member
/// time. Larger rests are opened at `offset` through [`SourceRef::open_at`] and streamed.
///
/// At most the rest up to `size` is read. A source that grew is cut at the manifest size,
/// as [`BundleWriter`] does; one that shrank yields fewer bytes and the writer fails with
/// `UnexpectedEof` (resume re-checks sources, SR-21).
///
/// [`SourceRef::open_at`]: super::source::SourceRef::open_at
pub fn open_source_member(source: SourceRef, size: u64, offset: u64) -> OpenFuture {
    let left = size.saturating_sub(offset);
    Box::pin(async move {
        if left > READ_WHOLE_MAX {
            let f = source.open_at(offset).await?;
            return Ok(Box::new(f) as MemberReader);
        }
        let bytes = tokio::task::spawn_blocking(move || read_rest(&source, offset, left))
            .await
            .map_err(io::Error::other)??;
        Ok(Box::new(io::Cursor::new(bytes)) as MemberReader)
    })
}

/// Opens `source`, seeks to `offset` and reads at most `left` bytes. Blocking; an opener
/// may call foreign code, which is why this runs on the blocking pool.
fn read_rest(source: &SourceRef, offset: u64, left: u64) -> io::Result<Vec<u8>> {
    use std::io::{Read as _, Seek as _};
    let mut f = match source {
        SourceRef::Path(p) => std::fs::File::open(p)?,
        SourceRef::Opener(o) => o.open()?,
    };
    if offset > 0 {
        f.seek(io::SeekFrom::Start(offset))?;
    }
    // `left` is at most READ_WHOLE_MAX, so it fits in usize.
    let mut buf = Vec::with_capacity(usize::try_from(left).unwrap_or(0));
    f.take(left).read_to_end(&mut buf)?;
    Ok(buf)
}

enum Phase {
    Header {
        idx: usize,
        within: usize,
    },
    Opening {
        idx: usize,
        within: u64,
        fut: OpenFuture,
    },
    Data {
        idx: usize,
        reader: Take<MemberReader>,
    },
    Pad {
        remaining: u64,
        next_idx: Option<usize>,
    },
    Trailer {
        remaining: u64,
    },
    Done,
}

/// Deterministic bundle writer: an [`AsyncRead`] of the archive from any byte offset
/// (protocol.md 7.7). Members shorter than the manifest says fail with `UnexpectedEof`;
/// longer ones are cut at the manifest size (resume re-checks sources, SR-21).
pub struct BundleWriter {
    layout: Arc<BundleLayout>,
    opener: Arc<dyn MemberOpener>,
    phase: Phase,
    header: [u8; 512],
    /// An error met after this read already put bytes in the buffer: those bytes are
    /// returned first and the error at the next read (`AsyncRead` returns no bytes along
    /// with an error).
    pending: Option<io::Error>,
}

impl std::fmt::Debug for BundleWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BundleWriter")
            .field("total", &self.layout.total())
            .finish_non_exhaustive()
    }
}

impl BundleWriter {
    /// Creates a writer that starts at archive offset `start`.
    pub fn new(
        layout: Arc<BundleLayout>,
        opener: Arc<dyn MemberOpener>,
        start: u64,
    ) -> Result<Self> {
        if start > layout.total() {
            return Err(Error::BadRequest(format!("bundle offset {start} past end")));
        }
        let mut w = Self {
            layout,
            opener,
            phase: Phase::Done,
            header: [0; 512],
            pending: None,
        };
        w.phase = w.phase_at(start);
        Ok(w)
    }

    fn phase_at(&mut self, pos: u64) -> Phase {
        match self.layout.locate(pos) {
            Position::Header { idx, within } => {
                self.header = member_header(&self.layout.members[idx]);
                Phase::Header {
                    idx,
                    // within < 512
                    within: usize::try_from(within).unwrap_or(0),
                }
            }
            Position::Data { idx, within } => self.opening(idx, within),
            Position::Pad { idx, remaining } => Phase::Pad {
                remaining,
                next_idx: self.next_member(idx),
            },
            Position::Trailer { remaining } => Phase::Trailer { remaining },
            Position::End => Phase::Done,
        }
    }

    fn next_member(&self, idx: usize) -> Option<usize> {
        (idx + 1 < self.layout.members.len()).then_some(idx + 1)
    }

    fn opening(&self, idx: usize, within: u64) -> Phase {
        let m = &self.layout.members[idx];
        Phase::Opening {
            idx,
            within,
            fut: self.opener.open(m.fid, within),
        }
    }

    fn after_data(&mut self, idx: usize) -> Phase {
        let m = self.layout.members[idx];
        let pad = m.padded_end() - m.data_end();
        let next = self.next_member(idx);
        if pad > 0 {
            Phase::Pad {
                remaining: pad,
                next_idx: next,
            }
        } else {
            self.enter_member_or_trailer(next)
        }
    }

    fn enter_member_or_trailer(&mut self, next: Option<usize>) -> Phase {
        match next {
            Some(i) => {
                self.header = member_header(&self.layout.members[i]);
                Phase::Header { idx: i, within: 0 }
            }
            None => Phase::Trailer { remaining: TRAILER },
        }
    }
}

impl AsyncRead for BundleWriter {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(e) = this.pending.take() {
            return Poll::Ready(Err(e));
        }
        let start_len = buf.filled().len();
        // Bytes already written by this call go out first; the error waits a call.
        let fail = |this: &mut Self, buf: &ReadBuf<'_>, e: io::Error| {
            if buf.filled().len() > start_len {
                this.pending = Some(e);
                Poll::Ready(Ok(()))
            } else {
                Poll::Ready(Err(e))
            }
        };
        loop {
            if buf.remaining() == 0 {
                return Poll::Ready(Ok(()));
            }
            let wrote = buf.filled().len() > start_len;
            match &mut this.phase {
                Phase::Done => return Poll::Ready(Ok(())),
                Phase::Header { idx, within } => {
                    let n = (512 - *within).min(buf.remaining());
                    buf.put_slice(&this.header[*within..*within + n]);
                    *within += n;
                    if *within == 512 {
                        let idx = *idx;
                        let m = this.layout.members[idx];
                        this.phase = if m.size == 0 {
                            this.after_data(idx)
                        } else {
                            this.opening(idx, 0)
                        };
                    }
                }
                Phase::Opening { idx, within, fut } => match fut.as_mut().poll(cx) {
                    Poll::Pending => {
                        return if wrote {
                            Poll::Ready(Ok(()))
                        } else {
                            Poll::Pending
                        };
                    }
                    Poll::Ready(Err(e)) => return fail(this, buf, e),
                    Poll::Ready(Ok(reader)) => {
                        let m = this.layout.members[*idx];
                        let left = m.size - *within;
                        this.phase = Phase::Data {
                            idx: *idx,
                            reader: reader.take(left),
                        };
                    }
                },
                Phase::Data { idx, reader } => {
                    let before = buf.filled().len();
                    match Pin::new(&mut *reader).poll_read(cx, buf) {
                        Poll::Pending => {
                            return if wrote {
                                Poll::Ready(Ok(()))
                            } else {
                                Poll::Pending
                            };
                        }
                        Poll::Ready(Err(e)) => return fail(this, buf, e),
                        Poll::Ready(Ok(())) => {}
                    }
                    let n = buf.filled().len() - before;
                    if reader.limit() == 0 {
                        let idx = *idx;
                        this.phase = this.after_data(idx);
                    } else if n == 0 {
                        let e = io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "bundle member shorter than the manifest size",
                        );
                        return fail(this, buf, e);
                    }
                }
                Phase::Pad {
                    remaining,
                    next_idx,
                } => {
                    let n = fill_zeros(buf, remaining);
                    if n == 0 {
                        let next = *next_idx;
                        this.phase = this.enter_member_or_trailer(next);
                    }
                }
                Phase::Trailer { remaining } => {
                    if fill_zeros(buf, remaining) == 0 {
                        this.phase = Phase::Done;
                    }
                }
            }
        }
    }
}

/// Writes up to `remaining` zero bytes into `buf` and returns how many are still left.
fn fill_zeros(buf: &mut ReadBuf<'_>, remaining: &mut u64) -> u64 {
    let n = usize::try_from(*remaining)
        .unwrap_or(usize::MAX)
        .min(buf.remaining());
    buf.initialize_unfilled_to(n).fill(0);
    buf.advance(n);
    *remaining -= n as u64;
    *remaining
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::chunk::tests::TestRng;
    use crate::transfer::manifest::{BundleInfo, FileEntry};

    struct MemOpener(Vec<Vec<u8>>);

    impl MemberOpener for MemOpener {
        fn open(&self, fid: u32, offset: u64) -> OpenFuture {
            let data = self.0[fid as usize][usize::try_from(offset).unwrap()..].to_vec();
            Box::pin(async move { Ok(Box::new(std::io::Cursor::new(data)) as MemberReader) })
        }
    }

    /// Reopens a file by path, as a host's opener would hand over a `content://` item.
    #[derive(Debug)]
    struct PathOpener(std::path::PathBuf);

    impl crate::transfer::source::SourceOpener for PathOpener {
        fn open(&self) -> io::Result<std::fs::File> {
            std::fs::File::open(&self.0)
        }
    }

    fn sources_for(path: &std::path::Path) -> [SourceRef; 2] {
        [
            SourceRef::Path(path.to_path_buf()),
            SourceRef::Opener(Arc::new(PathOpener(path.to_path_buf()))),
        ]
    }

    async fn read_member(source: SourceRef, size: u64, offset: u64) -> io::Result<Vec<u8>> {
        let mut r = open_source_member(source, size, offset).await?;
        let mut out = Vec::new();
        r.read_to_end(&mut out).await?;
        Ok(out)
    }

    /// From an offset, on both sides of `READ_WHOLE_MAX`, for path and opener sources: the
    /// rest of the member, and never more than the manifest size.
    #[tokio::test]
    async fn open_source_member_reads_the_rest_from_any_offset() {
        let dir = tempfile::tempdir().unwrap();
        let mut rng = TestRng(9);
        let big = usize::try_from(READ_WHOLE_MAX).unwrap() + 4096;
        for len in [10_000usize, big] {
            let data = rng.bytes(len);
            let path = dir.path().join(format!("m{len}"));
            std::fs::write(&path, &data).unwrap();
            let size = len as u64;
            // Rests above and below the limit (only below for the small member).
            for offset in [0, 3_000, size - 100] {
                for source in sources_for(&path) {
                    let got = read_member(source, size, offset).await.unwrap();
                    assert_eq!(
                        got,
                        data[usize::try_from(offset).unwrap()..],
                        "{len} {offset}"
                    );
                }
            }
            // A grown source is cut at the manifest size by the small path; the writer
            // cuts the streamed path (checked through `BundleWriter` below).
            let mut grown = data.clone();
            grown.extend_from_slice(b"appended later");
            std::fs::write(&path, &grown).unwrap();
            let got = read_member(SourceRef::Path(path.clone()), size, size - 100)
                .await
                .unwrap();
            assert_eq!(got, data[len - 100..]);
        }
    }

    /// Through `BundleWriter` with `open_source_member`: a source that grew since the offer
    /// is cut at the manifest size, and one that shrank fails with `UnexpectedEof`.
    #[tokio::test]
    async fn bundle_from_sources_cuts_grown_and_refuses_shrunk_members() {
        struct Sources(Vec<(SourceRef, u64)>);
        impl MemberOpener for Sources {
            fn open(&self, fid: u32, offset: u64) -> OpenFuture {
                let (source, size) = self.0[fid as usize].clone();
                open_source_member(source, size, offset)
            }
        }
        let big = usize::try_from(READ_WHOLE_MAX).unwrap() + 4096;
        let (m, contents) = sample(&[1000, 10, big, 700]);
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..contents.len())
            .map(|i| dir.path().join(format!("f{i}")))
            .collect();
        for (p, c) in paths.iter().zip(&contents) {
            let mut grown = c.clone();
            grown.extend_from_slice(b"grew");
            std::fs::write(p, &grown).unwrap();
        }
        let layout = Arc::new(BundleLayout::new(&m).unwrap());
        let sources = |which: usize| {
            Arc::new(Sources(
                paths
                    .iter()
                    .zip(&contents)
                    .enumerate()
                    .map(|(i, (p, c))| (sources_for(p)[(i + which) % 2].clone(), c.len() as u64))
                    .collect(),
            ))
        };
        let want = write_from(&m, &contents, 0).await;
        for which in [0, 1] {
            let mut w = BundleWriter::new(layout.clone(), sources(which), 0).unwrap();
            let mut got = Vec::new();
            w.read_to_end(&mut got).await.unwrap();
            assert!(got == want, "grown sources are cut at the manifest size");
        }
        for shrunk in [0usize, 2] {
            std::fs::write(&paths[shrunk], &contents[shrunk][..500]).unwrap();
            let mut w = BundleWriter::new(layout.clone(), sources(0), 0).unwrap();
            let mut got = Vec::new();
            let e = w.read_to_end(&mut got).await.unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof, "member {shrunk}");
            std::fs::write(&paths[shrunk], &contents[shrunk]).unwrap();
        }
    }

    fn sample(sizes: &[usize]) -> (Manifest, Vec<Vec<u8>>) {
        let mut rng = TestRng(42);
        let contents: Vec<Vec<u8>> = sizes.iter().map(|&s| rng.bytes(s)).collect();
        let files: Vec<FileEntry> = contents
            .iter()
            .enumerate()
            .map(|(i, c)| FileEntry {
                fid: u32::try_from(i).unwrap(),
                path: vec!["b".into(), format!("f{i}")],
                size: c.len() as u64,
                mtime: 1_791_460_950_123 + i64::try_from(i).unwrap(),
                mime: None,
                // Leave one large file out of the bundle to check fid gaps.
                in_bundle: i != 1,
            })
            .collect();
        let mut m = Manifest {
            id: "00112233445566778899aabbccddeeff".into(),
            chunk_size: 64 * 1024,
            total_bytes: contents.iter().map(|c| c.len() as u64).sum(),
            note: None,
            files,
            dirs: vec![],
            bundle: None,
        };
        m.bundle = Some(BundleInfo {
            size: bundle_size(&m),
        });
        m.validate().unwrap();
        (m, contents)
    }

    async fn write_from(m: &Manifest, contents: &[Vec<u8>], start: u64) -> Vec<u8> {
        let layout = Arc::new(BundleLayout::new(m).unwrap());
        let mut w =
            BundleWriter::new(layout, Arc::new(MemOpener(contents.to_vec())), start).unwrap();
        let mut out = Vec::new();
        w.read_to_end(&mut out).await.unwrap();
        out
    }

    fn read_all(
        m: &Manifest,
        bytes: &[u8],
        start: u64,
        piece: usize,
    ) -> Result<Vec<(u32, Vec<u8>)>> {
        let layout = Arc::new(BundleLayout::new(m).unwrap());
        let mut r = BundleReader::new(layout, start)?;
        let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
        for part in bytes.chunks(piece) {
            for ev in r.feed(Bytes::copy_from_slice(part))? {
                match ev {
                    BundleEvent::MemberStart { fid } => out.push((fid, Vec::new())),
                    BundleEvent::Data { fid, offset, bytes } => {
                        if out.last().map(|l| l.0) != Some(fid) {
                            out.push((fid, Vec::new()));
                        }
                        let cur = out.last_mut().unwrap();
                        assert_eq!(cur.1.len() as u64 + start_skew(m, fid, start), offset);
                        cur.1.extend_from_slice(&bytes);
                    }
                    BundleEvent::MemberEnd { .. } => {}
                }
            }
        }
        assert!(r.is_complete());
        Ok(out)
    }

    fn start_skew(m: &Manifest, fid: u32, start: u64) -> u64 {
        let layout = BundleLayout::new(m).unwrap();
        layout
            .members()
            .iter()
            .find(|x| x.fid == fid)
            .map_or(0, |x| x.bytes_below(start))
    }

    #[tokio::test]
    async fn size_prediction_and_round_trip() {
        let (m, contents) = sample(&[5, 300_000, 0, 512, 1000, 70_000]);
        let bytes = write_from(&m, &contents, 0).await;
        assert_eq!(bytes.len() as u64, bundle_size(&m));
        assert_eq!(bytes.len() as u64, m.bundle.unwrap().size);
        assert!(bytes[bytes.len() - 1024..].iter().all(|&b| b == 0));
        for piece in [1, 7, 512, 4096, 1 << 20] {
            let members = read_all(&m, &bytes, 0, piece).unwrap();
            let fids: Vec<u32> = members.iter().map(|x| x.0).collect();
            assert_eq!(fids, vec![0, 2, 3, 4, 5]);
            for (fid, data) in members {
                assert_eq!(data, contents[fid as usize]);
            }
        }
    }

    #[test]
    fn spec_example_size() {
        // protocol.md 7.2: one 2912-byte member gives a 4608-byte bundle.
        let m = BundleMember {
            fid: 2,
            size: 2912,
            mtime_secs: 0,
            header_offset: 0,
        };
        assert_eq!(m.padded_end() + 1024, 4608);
    }

    #[tokio::test]
    async fn restart_mid_member_is_identical() {
        let (m, contents) = sample(&[5, 300_000, 0, 512, 1000, 70_000]);
        let full = write_from(&m, &contents, 0).await;
        let mut rng = TestRng(1);
        let mut offsets: Vec<u64> = (0..40).map(|_| rng.below(full.len() as u64 + 1)).collect();
        offsets.extend([
            0,
            512,
            513,
            1023,
            1024,
            full.len() as u64 - 1024,
            full.len() as u64,
        ]);
        for off in offsets {
            let tail = write_from(&m, &contents, off).await;
            assert_eq!(
                &full[usize::try_from(off).unwrap()..],
                &tail[..],
                "offset {off}"
            );
        }
        // Reader resumes at a block-aligned mid-member offset.
        let layout = BundleLayout::new(&m).unwrap();
        let last = layout.members().last().unwrap();
        let off = last.data_offset() + 1024;
        let members = read_all(&m, &full[usize::try_from(off).unwrap()..], off, 999).unwrap();
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].1, contents[5][1024..]);
    }

    fn fix_checksum(h: &mut [u8]) {
        h[148..156].fill(b' ');
        let sum: u64 = h[..512].iter().map(|&b| u64::from(b)).sum();
        h[148..154].copy_from_slice(format!("{sum:06o}").as_bytes());
        h[154] = 0;
    }

    #[tokio::test]
    async fn rejects_deviations() {
        let (m, contents) = sample(&[5, 300_000, 100]);
        let good = write_from(&m, &contents, 0).await;

        let mut wrong_name = good.clone();
        wrong_name[0] = b'9';
        fix_checksum(&mut wrong_name[..512]);
        assert!(matches!(
            read_all(&m, &wrong_name, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));

        for t in *b"215xgL36" {
            let mut bad_type = good.clone();
            bad_type[156] = t;
            fix_checksum(&mut bad_type[..512]);
            assert!(
                matches!(
                    read_all(&m, &bad_type, 0, 4096),
                    Err(Error::BundleMismatch(_))
                ),
                "type {t}"
            );
        }

        let mut bad_sum = good.clone();
        bad_sum[0] = b'7';
        assert!(matches!(
            read_all(&m, &bad_sum, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));

        let mut bad_size = good.clone();
        bad_size[124..136].copy_from_slice(b"00000000006\0");
        fix_checksum(&mut bad_size[..512]);
        assert!(matches!(
            read_all(&m, &bad_size, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));

        // Zip-slip style name and a prefix field are rejected even with a valid checksum.
        let mut slip = good.clone();
        slip[..100].fill(0);
        slip[..9].copy_from_slice(b"../../.x\0");
        fix_checksum(&mut slip[..512]);
        assert!(matches!(
            read_all(&m, &slip, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));
        let mut prefix = good.clone();
        prefix[345] = b'a';
        fix_checksum(&mut prefix[..512]);
        assert!(matches!(
            read_all(&m, &prefix, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));

        let mut trailer = good.clone();
        let n = trailer.len();
        trailer[n - 3] = 1;
        assert!(matches!(
            read_all(&m, &trailer, 0, 4096),
            Err(Error::BundleMismatch(_))
        ));

        let layout = Arc::new(BundleLayout::new(&m).unwrap());
        let mut r = BundleReader::new(layout, 0).unwrap();
        r.feed(Bytes::from(good.clone())).unwrap();
        assert!(matches!(
            r.feed(Bytes::from_static(b"x")),
            Err(Error::BundleMismatch(_))
        ));
    }

    #[test]
    fn header_fields() {
        let m = BundleMember {
            fid: 12,
            size: 2912,
            mtime_secs: tar_mtime(1_791_460_950_999),
            header_offset: 0,
        };
        let h = member_header(&m);
        assert_eq!(&h[..3], b"12\0");
        assert_eq!(&h[100..108], b"0000644\0");
        assert_eq!(&h[124..136], b"00000005540\0");
        assert_eq!(parse_octal(&h[136..148]), Some(1_791_460_950));
        assert_eq!(&h[108..124], b"0000000\x000000000\x00");
        assert!(h[265..329].iter().all(|&b| b == 0), "empty uname and gname");
        assert_eq!(&h[329..345], b"0000000\x000000000\x00");
        assert_eq!(h[156], b'0');
        check_header(&h, &m).unwrap();
        assert_eq!(tar_mtime(-1), 0);
        assert_eq!(tar_mtime(i64::MAX), OCTAL11_MAX);
    }

    #[tokio::test]
    async fn system_tar_lists_bundle() {
        let Ok(out) = std::process::Command::new("tar").arg("--version").output() else {
            return;
        };
        if !out.status.success() {
            return;
        }
        let (m, contents) = sample(&[5, 300_000, 0, 1000]);
        let bytes = write_from(&m, &contents, 0).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("b.tar");
        std::fs::write(&path, &bytes).unwrap();
        let out = std::process::Command::new("tar")
            .arg("-tf")
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success());
        // Windows' bsdtar ends lines with CRLF: compare the names, not the bytes.
        let listed = String::from_utf8_lossy(&out.stdout);
        let names: Vec<&str> = listed.lines().map(str::trim_end).collect();
        assert_eq!(names, ["0", "2", "3"]);
    }
}
