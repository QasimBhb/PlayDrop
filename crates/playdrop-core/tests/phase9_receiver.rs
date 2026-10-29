//! Phase 9 small-file floor, measured on loopback without a network (Phase 8 handoff
//! section 5, `docs/release/throughput.md` 9.3).
//!
//! An ignored benchmark drives a 5,000-member bundle of 4 to 64 KiB members (seeded, the
//! shape of `bench/make-dataset.sh`'s `mixed-5000`) through `TransferReceiver` in
//! `Directory` mode and in `Staged` mode with a sink that does nothing, and reports wall
//! time and the time per member on the stream thread (staging files, checkpoints) and on
//! the delivery thread. It then reads the same members as a sender would, from real files,
//! through `BundleWriter`, to time the sender's per-member open and read.
//!
//! Run it on ext4 (`/tmp` here; never under the repo) with a release build:
//! `cargo test --release -p playdrop-core --test phase9_receiver -- --ignored --nocapture`.
//! `PD_BENCH_DIR` points it elsewhere (for example `/data/local/tmp` when the test binary
//! is built with `cargo ndk` and run on a phone over `adb shell`). `PD_BENCH_RUNS` sets the
//! runs per mode, `PD_BENCH_BUNDLE_ONLY` skips the streamed reference and the sender, and
//! `PD_BENCH_SYNC_MIB` tries another checkpoint cadence (see [`sync_policy`]). A second
//! table splits the stream and checkpoint time per member into their parts.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use playdrop_core::transfer::bundle::{open_source_member, MemberReader, OpenFuture};
use playdrop_core::transfer::manifest::new_transfer_id;
use playdrop_core::transfer::receiver::MemberTimings;
use playdrop_core::transfer::{
    bundle_size, root_hash, AcceptOptions, BundleInfo, BundleLayout, BundleWriter, ChunkEncoder,
    ChunkHash, DeliverItem, FileEntry, FileState, FreeSpace, Manifest, ManifestBuilder,
    MemberOpener, ReceiveSink, SinkBinding, SinkError, SourceRef, SpaceInfo, SyncPolicy,
    TransferReceiver, TransferSender, TransferState,
};
use tokio::io::AsyncReadExt as _;

/// The host's chunk size (`constants::DEFAULT_CHUNK_SIZE`).
const CS: u32 = 1024 * 1024;
const MEMBERS: usize = 5_000;
const RUNS: usize = 3;

/// The checkpoint cadence: `PD_BENCH_SYNC_MIB` sets the byte threshold (with no time
/// threshold, so the cadence is the same however fast the device is), else the host's
/// default (protocol.md 7.6). For measuring what a different cadence would buy; the host
/// itself always uses the default.
fn sync_policy() -> SyncPolicy {
    std::env::var("PD_BENCH_SYNC_MIB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or_else(SyncPolicy::default, |mib| SyncPolicy {
            bytes: mib << 20,
            batched_bytes: mib << 20,
            interval: Duration::from_secs(3600),
        })
}

/// Runs per mode: `PD_BENCH_RUNS`, else [`RUNS`].
fn runs() -> usize {
    std::env::var("PD_BENCH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(RUNS)
}

/// Where the benchmark writes: `PD_BENCH_DIR`, else `/tmp` (ext4 on the dev machine; the
/// repo volume is not, and data dirs never go under the repo).
fn bench_dir() -> std::path::PathBuf {
    std::env::var_os("PD_BENCH_DIR").map_or_else(|| "/tmp".into(), Into::into)
}

#[derive(Debug)]
struct Plenty;

impl FreeSpace for Plenty {
    fn space(&self, _path: &Path) -> std::io::Result<SpaceInfo> {
        Ok(SpaceInfo {
            available: 1 << 50,
            total: 1 << 50,
        })
    }
}

/// xorshift64, seeded: the same members on every run.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(len + 8);
        while v.len() < len {
            v.extend_from_slice(&self.next().to_le_bytes());
        }
        v.truncate(len);
        v
    }
}

/// `MEMBERS` contents of 4 to 64 KiB.
fn contents() -> Vec<Vec<u8>> {
    let mut rng = Rng(0x0005_EED9);
    (0..MEMBERS)
        .map(|_| {
            let len = 4096 + usize::try_from(rng.next() % (60 * 1024 + 1)).unwrap();
            rng.bytes(len)
        })
        .collect()
}

/// A bundle-only manifest of `contents` under folder `mixed`.
fn manifest(contents: &[Vec<u8>]) -> Manifest {
    let mut m = Manifest {
        id: new_transfer_id(),
        chunk_size: CS,
        total_bytes: contents.iter().map(|c| c.len() as u64).sum(),
        note: None,
        files: contents
            .iter()
            .enumerate()
            .map(|(i, c)| FileEntry {
                fid: u32::try_from(i).unwrap(),
                path: vec!["mixed".into(), format!("f{i:04}.bin")],
                size: c.len() as u64,
                mtime: 1_700_000_000_000,
                mime: None,
                in_bundle: true,
            })
            .collect(),
        dirs: Vec::new(),
        bundle: None,
    };
    m.bundle = Some(BundleInfo {
        size: bundle_size(&m),
    });
    m
}

struct Mem(Arc<Vec<Vec<u8>>>);

impl MemberOpener for Mem {
    fn open(&self, fid: u32, off: u64) -> OpenFuture {
        let d = self.0[fid as usize][usize::try_from(off).unwrap()..].to_vec();
        Box::pin(async move { Ok(Box::new(std::io::Cursor::new(d)) as MemberReader) })
    }
}

/// The bundle bytes and their chunk records, built once.
struct Fixture {
    manifest: Manifest,
    bundle: Vec<u8>,
    records: Vec<u8>,
    root: ChunkHash,
}

async fn fixture(contents: Arc<Vec<Vec<u8>>>) -> Fixture {
    let manifest = manifest(&contents);
    let layout = Arc::new(BundleLayout::new(&manifest).unwrap());
    let mut w = BundleWriter::new(layout, Arc::new(Mem(contents)), 0).unwrap();
    let mut bundle = Vec::new();
    w.read_to_end(&mut bundle).await.unwrap();
    let total = bundle.len() as u64;
    let mut enc = ChunkEncoder::new(&bundle[..], 0, total, CS, Vec::new()).unwrap();
    let mut records = Vec::new();
    enc.read_to_end(&mut records).await.unwrap();
    let hashes: Vec<ChunkHash> = bundle
        .chunks(CS as usize)
        .map(|c| *blake3::hash(c).as_bytes())
        .collect();
    let root = root_hash(total, CS, &hashes);
    Fixture {
        manifest,
        bundle,
        records,
        root,
    }
}

/// A sink that does no work: it takes the file and names a location.
#[derive(Debug, Default)]
struct CheapSink(AtomicUsize);

impl ReceiveSink for CheapSink {
    fn deliver(&self, item: DeliverItem, _file: std::fs::File) -> Result<String, SinkError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(format!("content://bench/{}", item.path.join("/")))
    }

    fn make_dir(&self, _path: Vec<String>) -> Result<(), SinkError> {
        Ok(())
    }
}

struct Run {
    wall: Duration,
    stream_end: Duration,
    t: MemberTimings,
}

/// Receives the whole bundle in one request into a fresh receive root under `/tmp`.
async fn receive_once(f: &Fixture, staged: bool) -> Run {
    let tmp = tempfile::tempdir_in(bench_dir()).unwrap();
    let sink = Arc::new(CheapSink::default());
    let opts = AcceptOptions {
        sink: staged.then(|| SinkBinding {
            sink: sink.clone(),
            peer: "ab".repeat(32),
        }),
        sync: sync_policy(),
        ..AcceptOptions::default()
    };
    let mut m = f.manifest.clone();
    m.id = new_transfer_id();
    let rx = TransferReceiver::accept(tmp.path(), m, Arc::new(Plenty), opts)
        .await
        .unwrap();
    let total = f.bundle.len() as u64;
    let token = rx.state().bundle.unwrap().token.unwrap();
    let t0 = Instant::now();
    let mut up = rx.begin_bundle(&token, 0, total - 1).await.unwrap();
    up.receive(&f.records[..]).await.unwrap();
    let stream_end = t0.elapsed();
    assert_eq!(
        up.finish(Some(f.root)).await.unwrap().state,
        FileState::Verified
    );
    let wall = t0.elapsed();
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    assert_eq!(rx.files_done(), MEMBERS);
    if staged {
        assert_eq!(sink.0.load(Ordering::Relaxed), MEMBERS);
    } else {
        assert_eq!(rx.saved_paths().len(), MEMBERS);
    }
    let t = rx.member_timings();
    drop(rx);
    Run {
        wall,
        stream_end,
        t,
    }
}

/// Bytes in MiB (the benchmark's sizes fit in a `u32`).
fn mib_of(bytes: u64) -> f64 {
    f64::from(u32::try_from(bytes).unwrap()) / (1024.0 * 1024.0)
}

fn ms_per(d: Duration, n: u64) -> f64 {
    d.as_secs_f64() * 1000.0 / f64::from(u32::try_from(n.max(1)).unwrap())
}

/// Opens a member the way the sender's `FsOpener` does: `tokio::fs` open and seek, then
/// reads through the async file (several blocking-pool hops per member).
struct PoolOpener(Vec<SourceRef>);

impl MemberOpener for PoolOpener {
    fn open(&self, fid: u32, off: u64) -> OpenFuture {
        let source = self.0[fid as usize].clone();
        Box::pin(async move {
            let f = source.open_at(off).await?;
            Ok(Box::new(f) as MemberReader)
        })
    }
}

/// Opens members through `bundle::open_source_member` (one blocking call per small
/// member).
struct WholeOpener(Vec<(SourceRef, u64)>);

impl MemberOpener for WholeOpener {
    fn open(&self, fid: u32, off: u64) -> OpenFuture {
        let (source, size) = self.0[fid as usize].clone();
        open_source_member(source, size, off)
    }
}

/// Reads the whole bundle through `opener`, without hashing, in chunk-sized reads as the
/// sender's encoder does, and returns the time.
async fn read_bundle(layout: Arc<BundleLayout>, opener: Arc<dyn MemberOpener>) -> Duration {
    let total = layout.total();
    let t0 = Instant::now();
    let mut w = BundleWriter::new(layout, opener, 0).unwrap();
    let mut buf = vec![0u8; CS as usize];
    let mut n = 0u64;
    loop {
        let got = w.read(&mut buf).await.unwrap();
        if got == 0 {
            break;
        }
        n += got as u64;
    }
    assert_eq!(n, total);
    t0.elapsed()
}

/// `count` streamed files with about the same total bytes as the bundle (the shape of
/// `medium-100`), each sent in its own request, one after the other: the per-byte
/// reference the small-file gate compares against.
async fn receive_streamed(contents: &[Vec<u8>]) -> Duration {
    let tmp = tempfile::tempdir_in(bench_dir()).unwrap();
    let mut m = manifest(contents);
    for f in &mut m.files {
        f.in_bundle = false;
    }
    m.bundle = None;
    let mut bodies = Vec::new();
    for c in contents {
        let size = c.len() as u64;
        let mut enc = ChunkEncoder::new(&c[..], 0, size, CS, Vec::new()).unwrap();
        let mut records = Vec::new();
        enc.read_to_end(&mut records).await.unwrap();
        let hashes: Vec<ChunkHash> = c
            .chunks(CS as usize)
            .map(|x| *blake3::hash(x).as_bytes())
            .collect();
        bodies.push((records, root_hash(size, CS, &hashes)));
    }
    let rx = TransferReceiver::accept(tmp.path(), m, Arc::new(Plenty), AcceptOptions::default())
        .await
        .unwrap();
    let t0 = Instant::now();
    for (fid, (records, root)) in bodies.iter().enumerate() {
        let fid = u32::try_from(fid).unwrap();
        let size = contents[fid as usize].len() as u64;
        let token = rx.state().files[fid as usize].token.clone().unwrap();
        let mut up = rx.begin_file(fid, &token, 0, size - 1).await.unwrap();
        up.receive(&records[..]).await.unwrap();
        assert_eq!(
            up.finish(Some(*root)).await.unwrap().state,
            FileState::Verified
        );
    }
    let wall = t0.elapsed();
    assert_eq!(rx.transfer_state(), TransferState::Complete);
    wall
}

/// 100 contents of 1.0 to 2.3 MiB (seeded), like `medium-100`.
fn medium_contents(total: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng(0x05EE_D100);
    let mut sizes: Vec<u64> = (0..100)
        .map(|_| 1_048_576 + rng.next() % 1_363_149)
        .collect();
    // Scale to the bundle's member bytes so both runs move the same volume.
    let sum: u64 = sizes.iter().sum();
    for s in &mut sizes {
        *s = *s * total / sum;
    }
    sizes
        .into_iter()
        .map(|s| rng.bytes(usize::try_from(s).unwrap()))
        .collect()
}

/// Where the stream and checkpoint time goes, per member (ms), run by run: member files
/// opened (create and truncate), written and closed, the bundle reader, then each part of
/// the checkpoints.
fn print_parts(detail: &[(bool, MemberTimings)], n: u64) {
    println!(
        "{:<10} {:>7} {:>7} {:>7} {:>7} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8}",
        "parts",
        "open",
        "write",
        "close",
        "reader",
        "ckpts",
        "ck cur",
        "ck flush",
        "ck hash",
        "ck rec",
        "ck feed"
    );
    for (staged, t) in detail {
        let reader = t.stream.saturating_sub(t.open + t.write + t.close);
        println!(
            "{:<10} {:>7.4} {:>7.4} {:>7.4} {:>7.4} {:>6} {:>8.4} {:>8.4} {:>8.4} {:>8.4} {:>8.4}",
            if *staged { "Staged" } else { "Directory" },
            ms_per(t.open, n),
            ms_per(t.write, n),
            ms_per(t.close, n),
            ms_per(reader, n),
            t.checkpoints,
            ms_per(t.ck_current, n),
            ms_per(t.ck_flush, n),
            ms_per(t.ck_hashes, n),
            ms_per(t.ck_record, n),
            ms_per(t.ck_feed, n),
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "benchmark; prints its numbers with --nocapture"]
async fn bench_small_member_floor() {
    let contents = Arc::new(contents());
    let f = fixture(Arc::clone(&contents)).await;
    let mib = mib_of(f.bundle.len() as u64);
    let n = MEMBERS as u64;
    println!(
        "{MEMBERS} members of 4 to 64 KiB, bundle {mib:.1} MiB, chunk {CS} B, in {}",
        bench_dir().display()
    );
    println!(
        "{:<10} {:>8} {:>9} {:>9} {:>14} {:>16} {:>16}",
        "mode", "wall s", "stream s", "MiB/s", "stream ms/mbr", "checkpt ms/mbr", "deliver ms/mbr"
    );
    // `PD_BENCH_BUNDLE_ONLY` skips the streamed reference and the sender, for quick A/B
    // runs of the receiver's bundle path.
    let bundle_only = std::env::var_os("PD_BENCH_BUNDLE_ONLY").is_some();
    let member_bytes: u64 = contents.iter().map(|c| c.len() as u64).sum();
    let medium = medium_contents(member_bytes);
    for _ in 0..if bundle_only { 0 } else { runs() } {
        let wall = receive_streamed(&medium).await;
        println!(
            "{:<10} {:>8.3} {:>9} {:>9.1}   (100 streamed files of 1.0 to 2.3 MiB, same bytes)",
            "Streamed",
            wall.as_secs_f64(),
            "",
            mib_of(member_bytes) / wall.as_secs_f64(),
        );
    }
    let mut detail = Vec::new();
    for staged in [false, true] {
        for _ in 0..runs() {
            let r = receive_once(&f, staged).await;
            detail.push((staged, r.t));
            println!(
                "{:<10} {:>8.3} {:>9.3} {:>9.1} {:>14.4} {:>16.4} {:>16.4}",
                if staged { "Staged" } else { "Directory" },
                r.wall.as_secs_f64(),
                r.stream_end.as_secs_f64(),
                mib / r.wall.as_secs_f64(),
                ms_per(r.t.stream, r.t.stream_members),
                ms_per(r.t.checkpoint, n),
                ms_per(r.t.delivery, r.t.delivered),
            );
            assert_eq!(r.t.stream_members, n);
            assert_eq!(r.t.delivered, n);
        }
    }
    print_parts(&detail, n);

    if bundle_only {
        return;
    }
    // Sender side: the same members as real files under /tmp.
    let src = tempfile::tempdir_in(bench_dir()).unwrap();
    let dir = src.path().join("mixed");
    std::fs::create_dir(&dir).unwrap();
    for (i, c) in contents.iter().enumerate() {
        std::fs::write(dir.join(format!("f{i:04}.bin")), c).unwrap();
    }
    let mut b = ManifestBuilder::new(CS);
    b.add_path(&dir).await.unwrap();
    let (m, sources) = b.build().unwrap();
    assert_eq!(m.bundle_members().count(), MEMBERS);
    let layout = Arc::new(BundleLayout::new(&m).unwrap());
    let refs: Vec<SourceRef> = sources.iter().map(|s| s.source.clone()).collect();
    let sized: Vec<(SourceRef, u64)> = sources.iter().map(|s| (s.source.clone(), s.size)).collect();
    println!(
        "{:<34} {:>8} {:>12}",
        "sender (files in page cache)", "s", "ms/member"
    );
    for _ in 0..RUNS {
        let mut sender = TransferSender::new(m.clone(), sources.clone(), CS).unwrap();
        let t0 = Instant::now();
        let mut enc = sender.bundle_encoder(0).unwrap();
        let mut sink = tokio::io::sink();
        tokio::io::copy(&mut enc, &mut sink).await.unwrap();
        let full = t0.elapsed();
        let pool = read_bundle(Arc::clone(&layout), Arc::new(PoolOpener(refs.clone()))).await;
        let whole = read_bundle(Arc::clone(&layout), Arc::new(WholeOpener(sized.clone()))).await;
        println!(
            "{:<34} {:>8.3} {:>12.4}",
            "TransferSender encoder (hashing)",
            full.as_secs_f64(),
            ms_per(full, n)
        );
        println!(
            "{:<34} {:>8.3} {:>12.4}",
            "BundleWriter, tokio::fs per member",
            pool.as_secs_f64(),
            ms_per(pool, n)
        );
        println!(
            "{:<34} {:>8.3} {:>12.4}",
            "BundleWriter, one blocking read",
            whole.as_secs_f64(),
            ms_per(whole, n)
        );
    }
}
