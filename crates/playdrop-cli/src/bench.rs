//! `playdrop bench`: tuned HTTP/2 against raw framing on the same TLS 1.3 connection
//! (PLAN.md "Wire": the Phase 1 benchmark; protocol.md section 10, last bullet).
//!
//! Both modes move the same protocol.md 7.4 chunk records (`chunk || BLAKE3(chunk)`): the
//! client hashes each chunk as it reads it and the server verifies each one, so the only
//! difference is the framing.
//!
//! - `h2`: one `PUT` whose body is the records, with the section 10 windows.
//! - `raw`: the TLS stream carries `[u8 type][u32 len]` records: type 1 is one chunk record,
//!   type 2 ends the run, and the server answers type 3 once everything is verified.
//!
//! Each run uses fresh throwaway identities in "Add device" mode, so no pairing is needed.

#![allow(
    clippy::cast_precision_loss,
    reason = "byte counts become MiB/s figures for display only"
)]

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Instant;

use anyhow::{anyhow, bail, Context as _};
use bytes::Bytes;
use playdrop_core::identity::{DeviceId, DeviceIdentity};
use playdrop_core::net::{
    client_builder, send_all, server::is_forbidden_port, server_builder, BodyReader,
};
use playdrop_core::session::Limits;
use playdrop_core::tls;
use playdrop_core::transfer::{ChunkDecoder, ChunkEncoder};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};

const MIB: f64 = 1024.0 * 1024.0;
const REC_CHUNK: u8 = 1;
const REC_END: u8 = 2;
const REC_ACK: u8 = 3;

/// Which side to run.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Role {
    /// Accept connections and verify what arrives.
    Server,
    /// Connect, send, and time it.
    Client,
}

/// Which framing to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Mode {
    /// One HTTP/2 PUT per run with the tuned windows.
    H2,
    /// `[u8 type][u32 len]` records straight on the TLS stream.
    Raw,
}

/// Options of `playdrop bench`.
#[derive(Debug, clap::Args)]
pub struct BenchArgs {
    /// Server or client.
    #[arg(long, value_enum)]
    role: Role,
    /// Address to listen on (server) or connect to (client).
    #[arg(long)]
    addr: SocketAddr,
    /// Bytes per run, like 1GiB, 500MiB or 1048576.
    #[arg(long, default_value = "1GiB", value_parser = parse_size)]
    size: u64,
    /// h2 or raw. The server and client must agree.
    #[arg(long, value_enum, default_value = "h2")]
    mode: Mode,
    /// Runs (client).
    #[arg(long, default_value_t = 3)]
    runs: u32,
    /// Chunk size in bytes (power of two, 64 KiB to 16 MiB).
    #[arg(long, default_value = "1MiB", value_parser = parse_size)]
    chunk: u64,
    /// Experiment only: HTTP/2 `MAX_FRAME_SIZE` instead of the protocol's 64 KiB. Set it
    /// on both sides.
    #[arg(long, value_parser = parse_size)]
    frame_size: Option<u64>,
    /// Experiment only: HTTP/2 stream and connection window instead of 8 and 32 MiB.
    #[arg(long, value_parser = parse_size)]
    window: Option<u64>,
}

/// HTTP/2 overrides for tuning experiments.
#[derive(Debug, Clone, Copy, Default)]
struct Tuning {
    frame: Option<u32>,
    window: Option<u32>,
}

impl Tuning {
    fn server(self) -> h2::server::Builder {
        let mut b = server_builder(&Limits::default());
        if let Some(f) = self.frame {
            b.max_frame_size(f);
        }
        if let Some(w) = self.window {
            b.initial_window_size(w)
                .initial_connection_window_size(w.saturating_mul(4));
        }
        b
    }

    fn client(self) -> h2::client::Builder {
        let mut b = client_builder(&Limits::default());
        if let Some(f) = self.frame {
            b.max_frame_size(f);
        }
        if let Some(w) = self.window {
            b.initial_window_size(w)
                .initial_connection_window_size(w.saturating_mul(4));
        }
        b
    }
}

fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim();
    let (num, mult) = [
        ("GiB", 1u64 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
        ("B", 1),
    ]
    .iter()
    .find_map(|(suf, m)| s.strip_suffix(suf).map(|n| (n, *m)))
    .unwrap_or((s, 1));
    num.trim()
        .parse::<u64>()
        .map(|n| n * mult)
        .map_err(|e| format!("bad size {s:?}: {e}"))
}

/// Runs the benchmark side.
pub async fn run(args: BenchArgs) -> anyhow::Result<()> {
    if is_forbidden_port(args.addr.port()) {
        bail!("port {} is reserved for another app", args.addr.port());
    }
    let chunk = u32::try_from(args.chunk).context("chunk size")?;
    let tuning = Tuning {
        frame: args
            .frame_size
            .map(u32::try_from)
            .transpose()
            .context("frame size")?,
        window: args
            .window
            .map(u32::try_from)
            .transpose()
            .context("window")?,
    };
    if tuning
        .frame
        .is_some_and(|f| !(16_384..=16_777_215).contains(&f))
    {
        bail!("HTTP/2 frame size must be 16 KiB to 16 MiB - 1");
    }
    match args.role {
        Role::Server => server(args.addr, args.mode, chunk, tuning).await,
        Role::Client => {
            let mut rates = Vec::new();
            for run in 1..=args.runs {
                let rate = client_run(args.addr, args.mode, args.size, chunk, tuning).await?;
                println!(
                    "run {run}: {:?} {:.0} MiB in {:.3} s = {rate:.1} MiB/s",
                    args.mode,
                    args.size as f64 / MIB,
                    args.size as f64 / MIB / rate
                );
                rates.push(rate);
            }
            let mean = rates.iter().sum::<f64>() / rates.len().max(1) as f64;
            let min = rates.iter().copied().fold(f64::INFINITY, f64::min);
            let max = rates.iter().copied().fold(0.0, f64::max);
            println!(
                "{:?}: mean {mean:.1} MiB/s, min {min:.1}, max {max:.1} over {} run(s)",
                args.mode,
                rates.len()
            );
            Ok(())
        }
    }
}

async fn server(addr: SocketAddr, mode: Mode, chunk: u32, tuning: Tuning) -> anyhow::Result<()> {
    let identity = Arc::new(DeviceIdentity::generate()?);
    let pins: Arc<dyn tls::PinLookup> = Arc::new(std::collections::BTreeSet::<DeviceId>::new());
    let window = Arc::new(AtomicBool::new(true));
    let config = tls::build_server_config(&identity, Arc::clone(&pins), Arc::clone(&window));
    let listener = TcpListener::bind(addr).await?;
    println!("bench server ({mode:?}) on {}", listener.local_addr()?);
    loop {
        let (tcp, remote) = listener.accept().await?;
        let (config, pins, window) = (Arc::clone(&config), Arc::clone(&pins), Arc::clone(&window));
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let r = async {
                let (stream, _peer) = tls::accept(config, pins.as_ref(), &window, tcp).await?;
                match mode {
                    Mode::H2 => serve_h2(stream, chunk, tuning).await,
                    Mode::Raw => serve_raw(stream).await,
                }
            }
            .await;
            match r {
                Ok(bytes) => println!("{remote}: verified {:.1} MiB", bytes as f64 / MIB),
                Err(e) => println!("{remote}: {e:#}"),
            }
        });
    }
}

async fn serve_h2<IO>(stream: IO, chunk: u32, tuning: Tuning) -> anyhow::Result<u64>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut conn = tuning.server().handshake::<_, Bytes>(stream).await?;
    let mut total = 0;
    while let Some(next) = conn.accept().await {
        let (req, mut respond) = next?;
        let len: u64 = req
            .headers()
            .get("content-length-plain")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| anyhow!("missing length"))?;
        let body = BodyReader::new(req.into_body());
        let verify = async {
            let mut dec = ChunkDecoder::new(body, 0, len - 1, chunk);
            let mut n = 0u64;
            while let Some(c) = dec.next_chunk().await? {
                n += c.data.len() as u64;
            }
            Ok::<u64, playdrop_core::Error>(n)
        };
        // Keep the connection driven while the body is read.
        let n = tokio::select! {
            r = verify => r?,
            _ = std::future::poll_fn(|cx| conn.poll_closed(cx)) => bail!("connection closed"),
        };
        total += n;
        let mut head = http::Response::new(());
        *head.status_mut() = http::StatusCode::OK;
        respond.send_response(head, true)?;
    }
    Ok(total)
}

async fn serve_raw<IO>(mut stream: IO) -> anyhow::Result<u64>
where
    IO: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut total = 0u64;
    let mut buf = Vec::new();
    loop {
        let mut head = [0u8; 5];
        stream.read_exact(&mut head).await?;
        let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        match head[0] {
            REC_CHUNK => {
                if len <= 32 || len > (16 << 20) + 32 {
                    bail!("bad record length {len}");
                }
                buf.resize(len, 0);
                stream.read_exact(&mut buf).await?;
                let (data, hash) = buf.split_at(len - 32);
                if blake3::hash(data).as_bytes() != hash {
                    bail!("chunk hash mismatch");
                }
                total += data.len() as u64;
            }
            REC_END => {
                stream.write_all(&[REC_ACK, 0, 0, 0, 0]).await?;
                stream.flush().await?;
                return Ok(total);
            }
            t => bail!("unknown record type {t}"),
        }
    }
}

/// `len` bytes from a 16 MiB seeded buffer, repeated.
struct Source {
    pattern: Bytes,
    pos: usize,
    left: u64,
}

impl Source {
    fn new(len: u64) -> Self {
        let mut v = vec![0u8; 16 << 20];
        let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
        for c in v.chunks_mut(8) {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            c.copy_from_slice(&x.to_le_bytes()[..c.len()]);
        }
        Self {
            pattern: Bytes::from(v),
            pos: 0,
            left: len,
        }
    }
}

impl AsyncRead for Source {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = &mut *self;
        let room = buf.remaining().min(this.pattern.len() - this.pos);
        let n = usize::try_from(this.left).map_or(room, |l| room.min(l));
        buf.put_slice(&this.pattern[this.pos..this.pos + n]);
        this.pos = (this.pos + n) % this.pattern.len();
        this.left -= n as u64;
        Poll::Ready(Ok(()))
    }
}

async fn client_run(
    addr: SocketAddr,
    mode: Mode,
    size: u64,
    chunk: u32,
    tuning: Tuning,
) -> anyhow::Result<f64> {
    let identity = DeviceIdentity::generate()?;
    let config = tls::build_client_config_unpaired(&identity);
    let tcp = TcpStream::connect(addr).await?;
    tcp.set_nodelay(true)?;
    let (stream, _peer) = tls::connect_tcp(config, None, tcp).await?;
    let mut enc = ChunkEncoder::new(Source::new(size), 0, size, chunk, Vec::new())?;
    match mode {
        Mode::H2 => {
            let (send, conn) = tuning.client().handshake::<_, Bytes>(stream).await?;
            let driver = tokio::spawn(async move {
                let _ = conn.await;
            });
            let started = Instant::now();
            let req = http::Request::builder()
                .method("PUT")
                .uri("/bench")
                .header("content-length-plain", size.to_string())
                .body(())?;
            let mut send = send.ready().await?;
            let (resp, mut body) = send.send_request(req, false)?;
            let upload = async {
                use futures_util::StreamExt as _;
                while let Some(rec) = enc.next().await {
                    send_all(&mut body, rec?, false).await?;
                }
                send_all(&mut body, Bytes::new(), true).await?;
                Ok::<(), anyhow::Error>(())
            };
            let (up, resp) = tokio::join!(upload, resp);
            up?;
            let status = resp?.status();
            let elapsed = started.elapsed();
            driver.abort();
            if status != http::StatusCode::OK {
                bail!("server answered {status}");
            }
            Ok(size as f64 / MIB / elapsed.as_secs_f64())
        }
        Mode::Raw => {
            let mut stream = stream;
            let started = Instant::now();
            {
                use futures_util::StreamExt as _;
                while let Some(rec) = enc.next().await {
                    let rec = rec?;
                    let len = u32::try_from(rec.len())?;
                    let mut head = [REC_CHUNK, 0, 0, 0, 0];
                    head[1..].copy_from_slice(&len.to_be_bytes());
                    stream.write_all(&head).await?;
                    stream.write_all(&rec).await?;
                }
            }
            stream.write_all(&[REC_END, 0, 0, 0, 0]).await?;
            stream.flush().await?;
            let mut ack = [0u8; 5];
            stream.read_exact(&mut ack).await?;
            if ack[0] != REC_ACK {
                bail!("bad ack");
            }
            Ok(size as f64 / MIB / started.elapsed().as_secs_f64())
        }
    }
}
