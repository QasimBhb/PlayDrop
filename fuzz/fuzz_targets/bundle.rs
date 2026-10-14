//! Tar bundle reader, protocol.md 7.4 and 7.7 (threat-model.md SR-19, SR-22, SR-33).
//!
//! Input (structured with `arbitrary`): a small manifest of bundle and non-bundle files, a
//! resume offset, how the body is split, and either corruptions applied to the genuine
//! archive or a raw byte string sent in its place.
//!
//! The archive bytes go through the receiver's real path: chunk records decoded and
//! BLAKE3-checked by `ChunkDecoder` (from an in-memory `AsyncRead`, never a file), then the
//! verified bytes into `BundleReader`, which checks every header against the manifest.
//! Without corruption the reader must accept the archive and return exactly the member
//! bytes; with corruption or raw input it may refuse but must never panic or emit data
//! outside a member.
#![no_main]

use std::collections::BTreeMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};

use arbitrary::Arbitrary;
use bytes::Bytes;
use libfuzzer_sys::fuzz_target;
use playdrop_core::transfer::bundle::member_header;
use playdrop_core::transfer::{
    BundleEvent, BundleInfo, BundleLayout, BundleReader, ChunkDecoder, ChunkEncoder, FileEntry,
    Manifest,
};
use tokio::io::{AsyncRead, AsyncReadExt, ReadBuf};

const MAX_MEMBERS: usize = 6;
const MAX_MEMBER_SIZE: u64 = 3000;

#[derive(Arbitrary, Debug)]
struct Member {
    size: u16,
    mtime: i64,
    in_bundle: bool,
    fill: u8,
}

#[derive(Arbitrary, Debug)]
struct Input {
    members: Vec<Member>,
    /// Resume offset, in chunks.
    start_chunk: u8,
    /// Chunk size is `512 << (chunk_shift % 8)`: the real minimum is 64 KiB, but the decoder
    /// does not care and small chunks reach more records per input.
    chunk_shift: u8,
    /// Read sizes for the body reader and feed sizes for the bundle reader.
    splits: Vec<u16>,
    /// `(position, xor)` pairs applied to the archive before it is chunk-encoded, so the
    /// hashes still match and the bundle reader sees the damage.
    corrupt: Vec<(u32, u8)>,
    /// One byte flipped in the encoded records, which the hash check must catch.
    record_corrupt: Option<(u32, u8)>,
    /// Sent instead of the genuine archive when present.
    raw: Option<Vec<u8>>,
}

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("tokio runtime")
    })
}

/// An in-memory body that returns at most the next split size per read, like an HTTP/2
/// body arriving in DATA frames.
struct Chunked<'a> {
    data: &'a [u8],
    splits: &'a [u16],
    i: usize,
}

impl AsyncRead for Chunked<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let step = match self.splits.get(self.i % self.splits.len().max(1)) {
            Some(&s) => usize::from(s).max(1),
            None => usize::MAX,
        };
        self.i += 1;
        let n = step.min(buf.remaining()).min(self.data.len());
        let (head, tail) = self.data.split_at(n);
        buf.put_slice(head);
        self.data = tail;
        Poll::Ready(Ok(()))
    }
}

fn content(m: &Member, i: usize) -> Vec<u8> {
    (0..u64::from(m.size).min(MAX_MEMBER_SIZE))
        .map(|k| {
            m.fill
                .wrapping_add((k as u8).wrapping_mul(31))
                .wrapping_add(i as u8)
        })
        .collect()
}

fn build(members: &[Member]) -> (Manifest, Vec<Vec<u8>>) {
    let mut files = Vec::new();
    let mut contents = Vec::new();
    for (i, m) in members.iter().take(MAX_MEMBERS).enumerate() {
        let c = content(m, i);
        files.push(FileEntry {
            fid: u32::try_from(i).unwrap(),
            path: vec![format!("f{i}")],
            size: c.len() as u64,
            mtime: m.mtime,
            mime: None,
            in_bundle: m.in_bundle,
        });
        contents.push(c);
    }
    let mut manifest = Manifest {
        id: "0123456789abcdef0123456789abcdef".into(),
        chunk_size: 64 * 1024,
        total_bytes: files.iter().map(|f| f.size).sum(),
        note: None,
        files,
        dirs: Vec::new(),
        bundle: None,
    };
    if manifest.bundle_members().next().is_some() {
        manifest.bundle = Some(BundleInfo {
            size: playdrop_core::transfer::bundle_size(&manifest),
        });
    }
    manifest.validate().expect("generated manifest is valid");
    (manifest, contents)
}

fn archive(layout: &BundleLayout, contents: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(usize::try_from(layout.total()).unwrap());
    for m in layout.members() {
        out.extend_from_slice(&member_header(m));
        out.extend_from_slice(&contents[m.fid as usize]);
        out.resize(usize::try_from(m.padded_end()).unwrap(), 0);
    }
    out.resize(usize::try_from(layout.total()).unwrap(), 0);
    out
}

async fn encode(body: &[u8], start: u64, chunk_size: u32) -> Vec<u8> {
    let prior = vec![[0u8; 32]; usize::try_from(start / u64::from(chunk_size)).unwrap()];
    let end = start + body.len() as u64;
    let mut enc = ChunkEncoder::new(body, start, end, chunk_size, prior).expect("aligned");
    let mut out = Vec::new();
    enc.read_to_end(&mut out).await.expect("in-memory encode");
    out
}

/// Feeds verified bytes to the bundle reader and checks every event against the layout.
/// Returns the member bytes seen, by fid, or `None` once the reader refuses the archive.
fn feed(
    reader: &mut BundleReader,
    layout: &BundleLayout,
    data: Bytes,
    seen: &mut BTreeMap<u32, Vec<u8>>,
    ended: &mut Vec<u32>,
) -> Option<()> {
    let before = reader.position();
    let len = data.len() as u64;
    let events = reader.feed(data).ok()?;
    assert_eq!(reader.position(), before + len, "position tracks fed bytes");
    assert!(reader.position() <= layout.total());
    for ev in events {
        match ev {
            BundleEvent::MemberStart { fid } => {
                assert!(layout.member(fid).is_some());
                seen.insert(fid, Vec::new());
            }
            BundleEvent::Data { fid, offset, bytes } => {
                let m = layout.member(fid).expect("data for a bundle member");
                assert!(
                    offset + bytes.len() as u64 <= m.size,
                    "data past member end"
                );
                let buf = seen.entry(fid).or_default();
                // A member resumed mid-data starts at its offset; afterwards data is contiguous.
                if buf.is_empty() && offset != 0 {
                    buf.resize(usize::try_from(offset).unwrap(), 0);
                }
                assert_eq!(buf.len() as u64, offset, "data is contiguous");
                buf.extend_from_slice(&bytes);
            }
            BundleEvent::MemberEnd { fid } => {
                let m = layout.member(fid).expect("end of a bundle member");
                assert_eq!(seen.get(&fid).map_or(0, Vec::len) as u64, m.size);
                ended.push(fid);
            }
        }
    }
    Some(())
}

fuzz_target!(|input: Input| {
    let (manifest, contents) = build(&input.members);
    let layout = Arc::new(BundleLayout::new(&manifest).expect("layout of a valid manifest"));
    let total = layout.total();
    let chunk_size = 512u32 << (input.chunk_shift % 8);
    let cs = u64::from(chunk_size);
    let start = (u64::from(input.start_chunk) * cs).min(total / cs * cs);

    let genuine = archive(&layout, &contents);
    assert_eq!(genuine.len() as u64, total);
    let clean = input.raw.is_none() && input.corrupt.is_empty();
    let mut body = match &input.raw {
        Some(raw) => raw.clone(),
        None => genuine[usize::try_from(start).unwrap()..].to_vec(),
    };
    for &(pos, x) in &input.corrupt {
        if !body.is_empty() {
            let i = pos as usize % body.len();
            body[i] ^= x;
        }
    }

    runtime().block_on(async {
        let mut records = encode(&body, start, chunk_size).await;
        if let Some((pos, x)) = input.record_corrupt {
            if !records.is_empty() && x != 0 {
                let i = pos as usize % records.len();
                records[i] ^= x;
            }
        }
        let flipped = input.record_corrupt.is_some_and(|(_, x)| x != 0) && !records.is_empty();

        let mut reader = BundleReader::new(layout.clone(), start).expect("chunk-aligned start");
        let mut seen = BTreeMap::new();
        let mut ended = Vec::new();
        let mut ok = true;
        if body.is_empty() {
            // A zero-length range is never sent (check_range); only the reader is exercised.
            ok = feed(&mut reader, &layout, Bytes::new(), &mut seen, &mut ended).is_some();
        } else {
            let end_inclusive = start + body.len() as u64 - 1;
            let mut dec = ChunkDecoder::new(
                Chunked {
                    data: &records,
                    splits: &input.splits,
                    i: 0,
                },
                start,
                end_inclusive,
                chunk_size,
            );
            loop {
                match dec.next_chunk().await {
                    Ok(Some(chunk)) => {
                        assert_eq!(chunk.offset, reader.position(), "chunks arrive in order");
                        assert!(chunk.data.len() as u64 <= cs);
                        // Feed each verified chunk in the pieces the split list gives.
                        let mut data = chunk.data;
                        let mut k = 0;
                        while ok && !data.is_empty() {
                            let step = input
                                .splits
                                .get(k)
                                .map_or(data.len(), |&s| usize::from(s).max(1))
                                .min(data.len());
                            k += 1;
                            let piece = data.split_to(step);
                            ok = feed(&mut reader, &layout, piece, &mut seen, &mut ended).is_some();
                        }
                        if !ok {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(_) => {
                        assert!(flipped, "decoder refused untouched records");
                        ok = false;
                        break;
                    }
                }
            }
        }

        if clean && !flipped {
            assert!(ok, "genuine archive refused");
            assert!(reader.is_complete(), "genuine archive not complete");
            for m in layout.members() {
                // Members wholly before the resume offset were delivered on an earlier PUT.
                if m.header_offset < start && m.data_end() <= start {
                    continue;
                }
                let got = seen.get(&m.fid).expect("member seen");
                let from = usize::try_from(m.bytes_below(start)).unwrap();
                assert_eq!(
                    &got[from..],
                    &contents[m.fid as usize][from..],
                    "member {} bytes",
                    m.fid
                );
                assert!(ended.contains(&m.fid));
            }
        }
    });
});
