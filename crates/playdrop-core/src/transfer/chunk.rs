//! Chunk records, streaming BLAKE3 and the root and prefix hashes of protocol.md 7.4.
//!
//! A body is `[chunk bytes][BLAKE3(chunk bytes), 32 bytes]` repeated; every chunk is
//! `chunkSize` long except possibly the last chunk of the file. Chunks are aligned to file
//! offset 0, so a range starts on a chunk boundary and ends on one or at end of file.

use std::io;
use std::pin::Pin;
use std::task::{ready, Context, Poll};

use bytes::{Bytes, BytesMut};
use futures_util::Stream;
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

use crate::{Error, Result};

/// Length of a chunk hash (BLAKE3, 32 bytes).
pub const HASH_LEN: usize = 32;

/// One chunk's BLAKE3 hash, protocol.md 7.4 `h_i`.
pub type ChunkHash = [u8; HASH_LEN];

const ROOT_TAG: &[u8] = b"playdrop-root-v1";
const PREFIX_TAG: &[u8] = b"playdrop-prefix-v1";

/// Number of chunks covering `len` bytes (protocol.md 7.4).
pub fn chunk_count(len: u64, chunk_size: u32) -> u64 {
    len.div_ceil(u64::from(chunk_size))
}

/// Body length of a `PUT` carrying file bytes `start..=end_inclusive`:
/// `(end - start + 1) + 32 * chunks` (protocol.md 7.4). `start` must be chunk-aligned.
pub fn expected_body_len(start: u64, end_inclusive: u64, chunk_size: u32) -> u64 {
    debug_assert!(start <= end_inclusive);
    let len = end_inclusive - start + 1;
    len + HASH_LEN as u64 * chunk_count(len, chunk_size)
}

/// Checks a `content-range` against protocol.md 7.4: start chunk-aligned, end within the
/// declared size, and end on a chunk boundary or at end of file (so every chunk hash in
/// the range covers a whole chunk). Returns [`Error::BadRequest`] otherwise.
pub fn check_range(start: u64, end_inclusive: u64, size: u64, chunk_size: u32) -> Result<()> {
    let cs = u64::from(chunk_size);
    if size == 0 {
        return Err(Error::BadRequest(
            "zero-length files are never streamed".into(),
        ));
    }
    if start > end_inclusive || end_inclusive >= size {
        return Err(Error::BadRequest(format!(
            "range {start}-{end_inclusive} outside size {size}"
        )));
    }
    if !start.is_multiple_of(cs) {
        return Err(Error::BadRequest(format!(
            "range start {start} not chunk-aligned"
        )));
    }
    let end = end_inclusive + 1;
    if end != size && !end.is_multiple_of(cs) {
        return Err(Error::BadRequest(format!(
            "range end {end_inclusive} neither chunk-aligned nor end of file"
        )));
    }
    Ok(())
}

/// Whole-file root (protocol.md 7.4):
/// `BLAKE3("playdrop-root-v1" || u64 BE fileSize || u32 BE chunkSize || h_0 .. h_last)`.
pub fn root_hash(file_size: u64, chunk_size: u32, hashes: &[ChunkHash]) -> ChunkHash {
    tagged_hash(ROOT_TAG, file_size, chunk_size, hashes)
}

/// Verified-prefix hash (protocol.md 7.4):
/// `BLAKE3("playdrop-prefix-v1" || u64 BE verifiedOffset || u32 BE chunkSize || h_0 .. h_(k-1))`
/// where `hashes` are exactly the `k = verifiedOffset / chunkSize` hashes below the offset.
pub fn prefix_hash(verified_offset: u64, chunk_size: u32, hashes: &[ChunkHash]) -> ChunkHash {
    tagged_hash(PREFIX_TAG, verified_offset, chunk_size, hashes)
}

fn tagged_hash(tag: &[u8], n: u64, chunk_size: u32, hashes: &[ChunkHash]) -> ChunkHash {
    let mut h = blake3::Hasher::new();
    h.update(tag);
    h.update(&n.to_be_bytes());
    h.update(&chunk_size.to_be_bytes());
    for hash in hashes {
        h.update(hash);
    }
    *h.finalize().as_bytes()
}

/// Encodes file bytes into chunk records while reading them (protocol.md 7.4).
///
/// Wraps a reader positioned at `start`, which must be chunk-aligned. Each chunk is hashed
/// as it is read: no pre-read, no second pass. Every hash is kept, together with the hashes
/// of the chunks before `start` supplied by the caller, so [`ChunkEncoder::root`] and the
/// sender's later prefix checks need no re-read (protocol.md 7.6).
///
/// Use it as an [`AsyncRead`] of the record body, or as a [`Stream`] of whole records
/// (`chunk || hash` in one `Bytes`), which avoids a copy when feeding an HTTP/2 body.
#[derive(Debug)]
pub struct ChunkEncoder<R> {
    inner: R,
    chunk_size: u32,
    offset: u64,
    end: u64,
    hashes: Vec<ChunkHash>,
    pending: Option<(BytesMut, usize)>,
    out: Bytes,
}

impl<R: AsyncRead + Unpin> ChunkEncoder<R> {
    /// Creates an encoder for file bytes `start..end` (exclusive end).
    ///
    /// `prior_hashes` must hold exactly the `start / chunk_size` hashes of the chunks before
    /// `start` (empty when starting at 0). Returns [`Error::BadRequest`] on misalignment.
    pub fn new(
        inner: R,
        start: u64,
        end: u64,
        chunk_size: u32,
        mut prior_hashes: Vec<ChunkHash>,
    ) -> Result<Self> {
        let cs = u64::from(chunk_size);
        if chunk_size == 0 || !start.is_multiple_of(cs) || start > end {
            return Err(Error::BadRequest(format!(
                "encoder range {start}..{end} not chunk-aligned"
            )));
        }
        let k = usize::try_from(start / cs).map_err(|_| Error::TooLarge("range".into()))?;
        if prior_hashes.len() != k {
            return Err(Error::BadRequest(format!(
                "{} prior hashes for {k} chunks",
                prior_hashes.len()
            )));
        }
        prior_hashes.reserve(usize::try_from(chunk_count(end - start, chunk_size)).unwrap_or(0));
        Ok(Self {
            inner,
            chunk_size,
            offset: start,
            end,
            hashes: prior_hashes,
            pending: None,
            out: Bytes::new(),
        })
    }

    /// Polls for the next whole record (`chunk || hash`). `None` once `end` is reached.
    pub fn poll_next_record(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<Option<Bytes>>> {
        if self.offset >= self.end && self.pending.is_none() {
            return Poll::Ready(Ok(None));
        }
        let (mut buf, mut filled) = self.pending.take().unwrap_or_else(|| {
            let want = chunk_len(self.offset, self.end, self.chunk_size);
            (BytesMut::zeroed(want + HASH_LEN), 0)
        });
        let want = buf.len() - HASH_LEN;
        while filled < want {
            let mut rb = ReadBuf::new(&mut buf[filled..want]);
            match Pin::new(&mut self.inner).poll_read(cx, &mut rb) {
                Poll::Pending => {
                    self.pending = Some((buf, filled));
                    return Poll::Pending;
                }
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) => {}
            }
            let n = rb.filled().len();
            if n == 0 {
                return Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "source ended before the declared size",
                )));
            }
            filled += n;
        }
        let hash = blake3::hash(&buf[..want]);
        buf[want..].copy_from_slice(hash.as_bytes());
        self.hashes.push(*hash.as_bytes());
        self.offset += want as u64;
        Poll::Ready(Ok(Some(buf.freeze())))
    }

    /// File offset of the next chunk to be read.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// True once every record up to `end` has been produced.
    pub fn is_done(&self) -> bool {
        self.offset >= self.end && self.pending.is_none() && self.out.is_empty()
    }

    /// Every chunk hash known so far, from chunk 0.
    pub fn hashes(&self) -> &[ChunkHash] {
        &self.hashes
    }

    /// Consumes the encoder and returns every chunk hash known so far, from chunk 0.
    pub fn into_hashes(self) -> Vec<ChunkHash> {
        self.hashes
    }

    /// Root over all chunks of a file of `file_size` bytes, once every hash is known.
    pub fn root(&self, file_size: u64) -> Option<ChunkHash> {
        let n = usize::try_from(chunk_count(file_size, self.chunk_size)).ok()?;
        (self.hashes.len() == n).then(|| root_hash(file_size, self.chunk_size, &self.hashes))
    }
}

fn chunk_len(offset: u64, end: u64, chunk_size: u32) -> usize {
    // At most chunk_size, which is a u32, so the cast to usize is lossless.
    #[allow(clippy::cast_possible_truncation)] // bounded by chunk_size (u32)
    let len = (end - offset).min(u64::from(chunk_size)) as usize;
    len
}

impl<R: AsyncRead + Unpin> AsyncRead for ChunkEncoder<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.out.is_empty() {
            match ready!(this.poll_next_record(cx))? {
                Some(record) => this.out = record,
                None => return Poll::Ready(Ok(())),
            }
        }
        let n = this.out.len().min(buf.remaining());
        buf.put_slice(&this.out.split_to(n));
        Poll::Ready(Ok(()))
    }
}

impl<R: AsyncRead + Unpin> Stream for ChunkEncoder<R> {
    type Item = io::Result<Bytes>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if !this.out.is_empty() {
            return Poll::Ready(Some(Ok(std::mem::take(&mut this.out))));
        }
        this.poll_next_record(cx).map(io::Result::transpose)
    }
}

/// One chunk that passed its BLAKE3 check (protocol.md 7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedChunk {
    /// File offset of the first byte.
    pub offset: u64,
    /// The chunk bytes.
    pub data: Bytes,
    /// The verified hash.
    pub hash: ChunkHash,
}

/// Decodes and verifies chunk records from a body (protocol.md 7.4, SR-22).
///
/// Yields chunks in order. The first failed hash returns
/// [`Error::ChunkHashMismatch`] with that chunk's offset, and the decoder stops. A body that
/// ends mid-record returns an `UnexpectedEof` [`Error::Io`]; only whole verified chunks are
/// ever yielded. Bytes past the declared range are rejected with [`Error::BadRequest`].
#[derive(Debug)]
pub struct ChunkDecoder<R> {
    inner: R,
    chunk_size: u32,
    offset: u64,
    end: u64,
    stopped: bool,
}

impl<R: AsyncRead + Unpin> ChunkDecoder<R> {
    /// Creates a decoder for file bytes `start..=end_inclusive` with the given chunk size.
    /// Range rules are the caller's job (see [`check_range`]).
    pub fn new(inner: R, start: u64, end_inclusive: u64, chunk_size: u32) -> Self {
        Self {
            inner,
            chunk_size,
            offset: start,
            end: end_inclusive + 1,
            stopped: false,
        }
    }

    /// File offset of the next expected chunk.
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// Reads and verifies the next record. `Ok(None)` when the range is complete.
    pub async fn next_chunk(&mut self) -> Result<Option<VerifiedChunk>> {
        if self.stopped {
            return Ok(None);
        }
        if self.offset >= self.end {
            self.stopped = true;
            let mut probe = [0u8; 1];
            if self.inner.read(&mut probe).await? != 0 {
                return Err(Error::BadRequest("bytes past the declared range".into()));
            }
            return Ok(None);
        }
        let want = chunk_len(self.offset, self.end, self.chunk_size);
        let mut buf = BytesMut::zeroed(want + HASH_LEN);
        if let Err(e) = self.inner.read_exact(&mut buf).await {
            self.stopped = true;
            return Err(e.into());
        }
        let claimed = buf.split_off(want);
        let actual = blake3::hash(&buf);
        let mut claimed_arr = [0u8; HASH_LEN];
        claimed_arr.copy_from_slice(&claimed);
        // blake3::Hash equality is constant-time; not needed here, but it costs nothing.
        if actual != blake3::Hash::from(claimed_arr) {
            self.stopped = true;
            return Err(Error::ChunkHashMismatch {
                offset: self.offset,
            });
        }
        let chunk = VerifiedChunk {
            offset: self.offset,
            data: buf.freeze(),
            hash: *actual.as_bytes(),
        };
        self.offset += want as u64;
        Ok(Some(chunk))
    }

    /// Returns the inner reader.
    pub fn into_inner(self) -> R {
        self.inner
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use proptest::prelude::*;
    use tokio::io::AsyncReadExt;

    /// Small, fast, seeded generator for test data (`SplitMix64`).
    pub(crate) struct TestRng(pub u64);

    impl TestRng {
        pub(crate) fn next_u64(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^ (z >> 31)
        }

        pub(crate) fn below(&mut self, n: u64) -> u64 {
            self.next_u64() % n
        }

        pub(crate) fn bytes(&mut self, len: usize) -> Vec<u8> {
            let mut v = Vec::with_capacity(len + 8);
            while v.len() < len {
                v.extend_from_slice(&self.next_u64().to_le_bytes());
            }
            v.truncate(len);
            v
        }
    }

    const CS: u32 = 64 * 1024;

    async fn encode(
        data: &[u8],
        start: u64,
        prior: Vec<ChunkHash>,
        cs: u32,
    ) -> (Vec<u8>, Vec<ChunkHash>) {
        let reader = &data[usize::try_from(start).unwrap()..];
        let mut enc = ChunkEncoder::new(reader, start, data.len() as u64, cs, prior).unwrap();
        let mut body = Vec::new();
        enc.read_to_end(&mut body).await.unwrap();
        assert!(enc.is_done());
        (body, enc.into_hashes())
    }

    async fn decode(body: &[u8], start: u64, size: u64, cs: u32) -> Result<Vec<u8>> {
        let mut dec = ChunkDecoder::new(body, start, size - 1, cs);
        let mut out = Vec::new();
        while let Some(c) = dec.next_chunk().await? {
            assert_eq!(c.offset, start + out.len() as u64);
            out.extend_from_slice(&c.data);
        }
        Ok(out)
    }

    #[tokio::test]
    async fn round_trip_random_sizes() {
        let mut rng = TestRng(7);
        for _ in 0..20 {
            let len = usize::try_from(rng.below(5 * u64::from(CS))).unwrap() + 1;
            let data = rng.bytes(len);
            let (body, hashes) = encode(&data, 0, Vec::new(), CS).await;
            let size = data.len() as u64;
            assert_eq!(body.len() as u64, expected_body_len(0, size - 1, CS));
            assert_eq!(hashes.len() as u64, chunk_count(size, CS));
            let out = decode(&body, 0, size, CS).await.unwrap();
            assert_eq!(out, data);
        }
    }

    #[tokio::test]
    async fn resume_encoding_matches_full() {
        let mut rng = TestRng(9);
        let data = rng.bytes(3 * CS as usize + 100);
        let (full_body, full_hashes) = encode(&data, 0, Vec::new(), CS).await;
        let start = 2 * u64::from(CS);
        let (tail_body, tail_hashes) = encode(&data, start, full_hashes[..2].to_vec(), CS).await;
        let rec = CS as usize + HASH_LEN;
        assert_eq!(&full_body[2 * rec..], &tail_body[..]);
        assert_eq!(full_hashes, tail_hashes);
    }

    #[tokio::test]
    async fn flipped_byte_detected_at_its_chunk() {
        let mut rng = TestRng(11);
        let data = rng.bytes(4 * CS as usize + 17);
        let (mut body, _) = encode(&data, 0, Vec::new(), CS).await;
        let rec = CS as usize + HASH_LEN;
        body[2 * rec + 5] ^= 0x40;
        let mut dec = ChunkDecoder::new(&body[..], 0, data.len() as u64 - 1, CS);
        assert_eq!(dec.next_chunk().await.unwrap().unwrap().offset, 0);
        assert_eq!(
            dec.next_chunk().await.unwrap().unwrap().offset,
            u64::from(CS)
        );
        match dec.next_chunk().await {
            Err(Error::ChunkHashMismatch { offset }) => assert_eq!(offset, 2 * u64::from(CS)),
            other => panic!("expected mismatch, got {other:?}"),
        }
        assert!(
            dec.next_chunk().await.unwrap().is_none(),
            "decoder stops after failure"
        );

        // A flipped hash byte fails the same way.
        let (mut body, _) = encode(&data, 0, Vec::new(), CS).await;
        body[rec - 1] ^= 1;
        assert!(matches!(
            decode(&body, 0, data.len() as u64, CS).await,
            Err(Error::ChunkHashMismatch { offset: 0 })
        ));
    }

    #[tokio::test]
    async fn truncated_and_overlong_bodies() {
        let data = TestRng(3).bytes(2 * CS as usize);
        let (body, _) = encode(&data, 0, Vec::new(), CS).await;
        let cut = &body[..body.len() - 10];
        let mut dec = ChunkDecoder::new(cut, 0, data.len() as u64 - 1, CS);
        assert!(dec.next_chunk().await.unwrap().is_some());
        assert!(matches!(dec.next_chunk().await, Err(Error::Io(_))));
        let mut long = body.clone();
        long.push(0);
        assert!(matches!(
            decode(&long, 0, data.len() as u64, CS).await,
            Err(Error::BadRequest(_))
        ));
    }

    #[tokio::test]
    async fn short_source_is_an_error() {
        let data = [0u8; 100];
        let mut enc = ChunkEncoder::new(&data[..], 0, 200, CS, Vec::new()).unwrap();
        let mut body = Vec::new();
        assert_eq!(
            enc.read_to_end(&mut body).await.unwrap_err().kind(),
            io::ErrorKind::UnexpectedEof
        );
    }

    #[test]
    fn root_and_prefix_are_deterministic_and_distinct() {
        let hashes: Vec<ChunkHash> = (0u8..5).map(|i| [i; 32]).collect();
        let size = 4 * u64::from(CS) + 9;
        let r1 = root_hash(size, CS, &hashes);
        assert_eq!(r1, root_hash(size, CS, &hashes));
        assert_ne!(r1, root_hash(size + 1, CS, &hashes));
        assert_ne!(r1, root_hash(size, CS * 2, &hashes));
        let p = prefix_hash(3 * u64::from(CS), CS, &hashes[..3]);
        assert_eq!(p, prefix_hash(3 * u64::from(CS), CS, &hashes[..3]));
        assert_ne!(p, prefix_hash(3 * u64::from(CS), CS, &hashes[..2]));
        // Domain separation: same inputs, different tag.
        assert_ne!(
            root_hash(3 * u64::from(CS), CS, &hashes[..3]),
            prefix_hash(3 * u64::from(CS), CS, &hashes[..3])
        );
        // Exact construction.
        let mut h = blake3::Hasher::new();
        h.update(b"playdrop-root-v1");
        h.update(&size.to_be_bytes());
        h.update(&CS.to_be_bytes());
        for x in &hashes {
            h.update(x);
        }
        assert_eq!(*h.finalize().as_bytes(), r1);
    }

    #[test]
    fn body_len_and_range_rules() {
        let cs = CS;
        let c = u64::from(cs);
        assert_eq!(expected_body_len(0, c - 1, cs), c + 32);
        assert_eq!(expected_body_len(0, c, cs), c + 1 + 64);
        assert_eq!(expected_body_len(c, 3 * c - 1, cs), 2 * c + 64);
        check_range(0, 9, 10, cs).unwrap();
        check_range(c, 2 * c - 1, 5 * c, cs).unwrap();
        check_range(c, 3 * c + 4, 3 * c + 5, cs).unwrap();
        assert!(check_range(1, 9, 10, cs).is_err());
        assert!(check_range(0, 10, 10, cs).is_err());
        assert!(check_range(0, c, 5 * c, cs).is_err());
        assert!(check_range(0, 0, 0, cs).is_err());
    }

    #[tokio::test]
    async fn stream_yields_whole_records() {
        use futures_util::StreamExt;
        let data = TestRng(5).bytes(2 * CS as usize + 1);
        let mut enc = ChunkEncoder::new(&data[..], 0, data.len() as u64, CS, Vec::new()).unwrap();
        let mut lens = Vec::new();
        while let Some(rec) = enc.next().await {
            lens.push(rec.unwrap().len());
        }
        assert_eq!(lens, vec![CS as usize + 32, CS as usize + 32, 1 + 32]);
        assert!(enc.root(data.len() as u64).is_some());
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]
        #[test]
        fn codec_round_trip(seed in any::<u64>(), len in 1usize..300_000, big in any::<bool>(), start_chunk in 0u64..4) {
            let cs = if big { 128 * 1024 } else { 64 * 1024 };
            let data = TestRng(seed).bytes(len);
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let (body, hashes) = encode(&data, 0, Vec::new(), cs).await;
                let out = decode(&body, 0, len as u64, cs).await.unwrap();
                prop_assert_eq!(&out, &data);
                prop_assert_eq!(root_hash(len as u64, cs, &hashes), root_hash(len as u64, cs, &hashes));
                let k = start_chunk.min(chunk_count(len as u64, cs) - 1);
                let start = k * u64::from(cs);
                let (tail, tail_hashes) =
                    encode(&data, start, hashes[..usize::try_from(k).unwrap()].to_vec(), cs).await;
                prop_assert_eq!(&tail_hashes, &hashes);
                let out = decode(&tail, start, len as u64, cs).await.unwrap();
                prop_assert_eq!(&out[..], &data[usize::try_from(start).unwrap()..]);
                Ok(())
            })?;
        }
    }
}
