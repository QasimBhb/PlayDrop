//! Shared helpers for the integration tests: two hosts on 127.0.0.1 with their own data
//! and receive folders, an event log, SAS pairing with auto-confirm, and seeded test data.
//! Everything lives under the system temp dir, never on the project mount.

#![allow(dead_code, clippy::missing_panics_doc)] // each test binary uses a subset

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use playdrop_core::discovery::StaticInterfaces;
use playdrop_core::host::{Host, HostConfig, HostEvent, PairingPromptKind};
use tokio::sync::mpsc;

/// One host under test.
pub struct TestHost {
    pub host: Host,
    pub events: Events,
    pub data_dir: PathBuf,
    pub recv_dir: PathBuf,
    pub port: u16,
    pub name: String,
}

/// Received events, kept so later waits can look back.
pub struct Events {
    rx: mpsc::Receiver<HostEvent>,
    pub seen: Vec<HostEvent>,
}

impl Events {
    /// Waits for the first event (new or already seen since `from`) matching `pred`.
    pub async fn wait_for<F>(&mut self, timeout: Duration, mut pred: F) -> HostEvent
    where
        F: FnMut(&HostEvent) -> bool,
    {
        if let Some(e) = self.seen.iter().find(|e| pred(e)) {
            return e.clone();
        }
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let ev = tokio::time::timeout_at(deadline, self.rx.recv())
                .await
                .expect("timed out waiting for an event")
                .expect("event channel closed");
            self.seen.push(ev.clone());
            if pred(&ev) {
                return ev;
            }
        }
    }

    /// Takes every event queued right now.
    pub fn drain(&mut self) {
        while let Ok(ev) = self.rx.try_recv() {
            self.seen.push(ev);
        }
    }

    /// Forgets what was seen.
    pub fn clear(&mut self) {
        self.drain();
        self.seen.clear();
    }
}

/// A test root under the system temp dir (not the project mount).
pub fn temp_root() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("playdrop-test-")
        .tempdir_in(std::env::temp_dir())
        .expect("temp dir")
}

/// Starts a host named `name` under `root`, on 127.0.0.1:`port` (0 picks one), with
/// discovery off: these tests use last-known dial only.
pub async fn start_host(root: &Path, name: &str, port: u16) -> TestHost {
    start_host_with(root, name, port, |_| {}).await
}

/// [`start_host`] with a chance to adjust the configuration (receive target, stores).
pub async fn start_host_with<F>(root: &Path, name: &str, port: u16, tweak: F) -> TestHost
where
    F: FnOnce(&mut HostConfig),
{
    let data_dir = root.join(name).join("data");
    let recv_dir = root.join(name).join("recv");
    std::fs::create_dir_all(root.join(name)).unwrap();
    let mut cfg = HostConfig::new(&data_dir, &recv_dir, name);
    tweak(&mut cfg);
    cfg.port = port;
    cfg.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
    cfg.discovery = false;
    cfg.interfaces = Box::new(StaticInterfaces::default());
    let (host, rx) = Host::start(cfg).await.expect("host starts");
    let port = host.local_addr().port();
    TestHost {
        host,
        events: Events {
            rx,
            seen: Vec::new(),
        },
        data_dir,
        recv_dir,
        port,
        name: name.to_owned(),
    }
}

impl TestHost {
    /// Stops the host and starts it again from the same folders and port.
    pub async fn restart(self, root: &Path) -> TestHost {
        let (name, port) = (self.name.clone(), self.port);
        self.host.shutdown().await;
        // A killed process leaves nothing behind: the old host, with its open part files
        // and in-memory transfer map, is gone before the new one reloads the same folders.
        drop(self);
        start_host(root, &name, port).await
    }
}

fn sas_of(ev: &HostEvent) -> Option<String> {
    match ev {
        HostEvent::PairingPrompt {
            prompt: PairingPromptKind::Sas { sas },
            ..
        } => Some(sas.0.clone()),
        _ => None,
    }
}

/// Pairs `a` (initiator) with `b` (responder) by SAS, confirming both prompts, and checks
/// both screens showed the same six digits. Returns the SAS.
pub async fn pair(a: &mut TestHost, b: &mut TestHost) -> String {
    b.host.open_add_device_window().unwrap();
    let addr = b.host.local_addr();
    a.host.pair_with(addr).await.expect("pair_with");
    let t = Duration::from_secs(20);
    let ea = a.events.wait_for(t, |e| sas_of(e).is_some()).await;
    let eb = b.events.wait_for(t, |e| sas_of(e).is_some()).await;
    let (sa, sb) = (sas_of(&ea).unwrap(), sas_of(&eb).unwrap());
    assert_eq!(sa, sb, "both screens must show the same SAS");
    assert_eq!(sa.len(), 6);
    a.host.pairing_respond(true);
    b.host.pairing_respond(true);
    a.events
        .wait_for(t, |e| matches!(e, HostEvent::Paired { .. }))
        .await;
    b.events
        .wait_for(t, |e| matches!(e, HostEvent::Paired { .. }))
        .await;
    a.events
        .wait_for(t, |e| matches!(e, HostEvent::PeerOnline { .. }))
        .await;
    b.events
        .wait_for(t, |e| matches!(e, HostEvent::PeerOnline { .. }))
        .await;
    sa
}

/// xorshift64*: fast, seeded, good enough for test data.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n
    }

    pub fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

/// Writes `size` seeded random bytes to `path`, 8 MiB at a time.
pub fn write_random_file(path: &Path, size: u64, seed: u64) {
    use std::io::Write;
    let mut rng = Rng(seed | 1);
    let mut f = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut buf = vec![0u8; 8 * 1024 * 1024];
    let mut left = size;
    while left > 0 {
        let n = usize::try_from(left.min(buf.len() as u64)).unwrap();
        rng.fill(&mut buf[..n]);
        f.write_all(&buf[..n]).unwrap();
        left -= n as u64;
    }
    f.flush().unwrap();
}

/// The protocol.md 7.4 root of a local file, computed independently of the engine.
pub fn file_root(path: &Path, chunk_size: u32) -> String {
    use std::io::Read;
    let size = std::fs::metadata(path).unwrap().len();
    let mut f = std::fs::File::open(path).unwrap();
    let mut buf = vec![0u8; chunk_size as usize];
    let mut hashes = Vec::new();
    let mut left = size;
    while left > 0 {
        let n = usize::try_from(left.min(u64::from(chunk_size))).unwrap();
        f.read_exact(&mut buf[..n]).unwrap();
        hashes.push(*blake3::hash(&buf[..n]).as_bytes());
        left -= n as u64;
    }
    let root = playdrop_core::transfer::root_hash(size, chunk_size, &hashes);
    playdrop_core::transfer::hash_to_hex(&root)
}

/// Seconds as f64, for printing rates.
pub fn mib_per_s(bytes: u64, elapsed: Duration) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    b / (1024.0 * 1024.0) / elapsed.as_secs_f64()
}

/// Captures the core's warnings into the test output, so a failing test shows what the
/// host complained about (the output is only printed for failures); `PD_LOG` raises the
/// level to debug.
pub fn init_logs() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;
    let level = if std::env::var_os("PD_LOG").is_some() {
        tracing::Level::DEBUG
    } else {
        tracing::Level::WARN
    };
    let filter = tracing_subscriber::filter::Targets::new().with_target("playdrop_core", level);
    let _ = tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_test_writer())
        .with(filter)
        .try_init();
}

/// Bytes as MiB, for printing.
pub fn mib(bytes: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    b / (1024.0 * 1024.0)
}
