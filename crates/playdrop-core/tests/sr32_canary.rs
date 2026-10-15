//! SR-32 canary test (threat-model.md SR-32, T37; docs/release/sr-checklist.md).
//!
//! One global `tracing` subscriber at TRACE for every target (core, `h2`, `rustls`, `log`
//! records through `tracing-log`) writes into memory while two loopback hosts pair by SAS,
//! unpair, pair again by QR, sync a short and a blob-sized clipboard text, send files, run
//! a remote input session that types text, and produce their diagnostics reports. Every
//! secret the run created is then looked for in the whole log and in both diagnostics
//! reports: private keys, announce keys (every rotation), the SAS, the QR secret, every
//! file token the receiver issued, clipboard and input text, file contents, and full
//! Device IDs (only the 8-character form may appear).
//!
//! The level rule for file names is checked with a reloadable filter: a transfer run at
//! INFO must not log its file name, and one run at DEBUG must, so the test proves the
//! rule rather than an absence of logging.
//!
//! Everything shares one in-memory buffer, so the whole scenario is one `#[test]`. It
//! installs its own subscriber and never calls `common::init_logs`, so `PD_LOG` and
//! `RUST_LOG` have no effect here.

mod common;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use common::{pair, start_host_with, temp_root, Rng, TestHost};
use playdrop_core::host::{
    DeliverItem, HostEvent, PairingPromptKind, ReceiveSink, ReceiveTarget, SinkError,
};
use playdrop_core::session::types::{Input, KeyAction, KeyInput, SessionOp};
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::layer::{Layer as _, SubscriberExt as _};
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt as _;

const T: Duration = Duration::from_secs(20);

type ReloadHandle = reload::Handle<LevelFilter, tracing_subscriber::Registry>;

/// The shared log buffers and the handle that changes their level.
struct Capture {
    /// Every target.
    buf: Arc<Mutex<Vec<u8>>>,
    /// Only targets that start with `playdrop`: the SAS is a bare six-digit number, so it
    /// is looked for here and not among dependency lines full of window sizes and counts.
    core: Arc<Mutex<Vec<u8>>>,
    level: ReloadHandle,
}

impl Capture {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()).into_owned()
    }

    fn core_text(&self) -> String {
        String::from_utf8_lossy(&self.core.lock().unwrap()).into_owned()
    }

    /// Where the next line of the every-target log will start.
    fn offset(&self) -> usize {
        self.buf.lock().unwrap().len()
    }

    /// The every-target log between two offsets.
    fn slice(&self, from: usize, to: usize) -> String {
        String::from_utf8_lossy(&self.buf.lock().unwrap()[from..to]).into_owned()
    }

    fn set_level(&self, level: LevelFilter) {
        self.level.modify(|f| *f = level).unwrap();
    }
}

/// A writer that appends to the shared buffer.
struct BufWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for BufWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Installs the one global subscriber for this test binary: TRACE, every target, no
/// timestamps (so a microsecond field cannot look like a SAS), no colour codes.
fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let (filter, level) = reload::Layer::new(LevelFilter::TRACE);
        let core = Arc::new(Mutex::new(Vec::new()));
        let b2 = Arc::clone(&buf);
        let c2 = Arc::clone(&core);
        tracing_subscriber::registry()
            .with(filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || BufWriter(Arc::clone(&b2)))
                    .with_ansi(false)
                    .without_time()
                    .with_target(true)
                    .with_level(true),
            )
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(move || BufWriter(Arc::clone(&c2)))
                    .with_ansi(false)
                    .without_time()
                    .with_target(true)
                    .with_level(true)
                    .with_filter(
                        tracing_subscriber::filter::Targets::new()
                            .with_target("playdrop", tracing::Level::TRACE),
                    ),
            )
            .try_init()
            .expect("the SR-32 test owns the global subscriber");
        Capture { buf, core, level }
    })
}

/// Strings that must never appear in a log or the diagnostics export.
#[derive(Default)]
struct Canaries {
    /// Label and exact text.
    needles: Vec<(String, String)>,
    /// Label and a token that must not appear with an alphanumeric on either side.
    standalone: Vec<(String, String)>,
    /// Numbers the test itself produced (sizes, ports, offsets). A standalone token equal
    /// to one of them cannot be told apart from it, so that token is not checked.
    known_numbers: std::collections::HashSet<String>,
}

impl Canaries {
    fn add(&mut self, label: impl Into<String>, needle: impl Into<String>) {
        let needle = needle.into();
        assert!(needle.len() >= 6, "canary too short to mean anything");
        if !self.needles.iter().any(|(_, n)| *n == needle) {
            self.needles.push((label.into(), needle));
        }
    }

    /// Secret bytes in every form a careless log line could print them.
    fn add_bytes(&mut self, label: &str, bytes: &[u8]) {
        use base64::engine::general_purpose::{
            STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD,
        };
        use base64::Engine as _;
        self.add(
            format!("{label} (hex)"),
            data_encoding::HEXLOWER.encode(bytes),
        );
        self.add(
            format!("{label} (HEX)"),
            data_encoding::HEXUPPER.encode(bytes),
        );
        self.add(format!("{label} (base64)"), STANDARD.encode(bytes));
        self.add(
            format!("{label} (base64, no pad)"),
            STANDARD_NO_PAD.encode(bytes),
        );
        self.add(format!("{label} (base64url)"), URL_SAFE.encode(bytes));
        self.add(
            format!("{label} (base64url, no pad)"),
            URL_SAFE_NO_PAD.encode(bytes),
        );
        self.add(format!("{label} (Debug bytes)"), format!("{bytes:?}"));
    }

    fn known_number(&mut self, n: impl std::fmt::Display) {
        self.known_numbers.insert(n.to_string());
    }

    /// Every exact-text hit in `text`, with a little context.
    fn leaks(&self, where_: &str, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (label, n) in &self.needles {
            if let Some(i) = text.find(n.as_str()) {
                out.push(format!(
                    "{where_}: {label} at byte {i}: {}",
                    context(text, i, n.len())
                ));
            }
        }
        out
    }

    /// Every standalone-token hit in `text`. Run on core lines and the diagnostics only.
    fn standalone_leaks(&self, where_: &str, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (label, tok) in &self.standalone {
            if self.known_numbers.contains(tok) {
                println!("{label} {tok} equals a number the test produced; not checked this run");
                continue;
            }
            for (i, _) in text.match_indices(tok.as_str()) {
                let before = text[..i].chars().next_back();
                let after = text[i + tok.len()..].chars().next();
                let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
                if !alnum(before) && !alnum(after) {
                    out.push(format!(
                        "{where_}: {label} at byte {i}: {}",
                        context(text, i, tok.len())
                    ));
                }
            }
        }
        out
    }
}

fn context(text: &str, i: usize, len: usize) -> String {
    let line_start = text[..i].rfind('\n').map_or(0, |p| p + 1);
    let line_end = text[i + len..]
        .find('\n')
        .map_or(text.len(), |p| i + len + p);
    let line = &text[line_start..line_end];
    let mut s: String = line.chars().take(400).collect();
    if line.len() > 400 {
        s.push_str("...");
    }
    s
}

/// Finds `name` anywhere beneath `dir`.
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

/// Every file named `name` beneath `dir`.
fn find_all(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            find_all(&p, name, out);
        } else if p.file_name().is_some_and(|n| n == name) {
            out.push(p);
        }
    }
}

/// The private key in `identity.key` (PKCS#8 DER, unsealed in tests): the whole file and
/// the 32-byte Ed25519 seed inside it.
fn harvest_private_key(host: &TestHost, c: &mut Canaries) {
    let path = find_file(&host.data_dir, "identity.key").expect("identity.key exists");
    let der = std::fs::read(path).unwrap();
    assert!(der.len() >= 48, "unexpected PKCS#8 length {}", der.len());
    c.add_bytes(&format!("{} private key seed", host.name), &der[16..48]);
    c.add_bytes(&format!("{} private key DER", host.name), &der);
    if let Ok(text) = std::str::from_utf8(&der) {
        c.add(format!("{} private key file text", host.name), text.trim());
    }
}

/// The local announce key and every peer announce key in `pins.json` right now.
fn harvest_announce_keys(host: &TestHost, c: &mut Canaries) -> usize {
    let path = find_file(&host.data_dir, "pins.json").expect("pins.json exists");
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let mut keys = Vec::new();
    if let Some(k) = v["local_announce_key"]["key"].as_str() {
        keys.push((format!("{} local announce key", host.name), k.to_owned()));
    }
    for p in v["pins"].as_array().into_iter().flatten() {
        if let Some(k) = p["announce_key"]["key"].as_str() {
            keys.push((
                format!("{} copy of a peer announce key", host.name),
                k.to_owned(),
            ));
        }
    }
    let n = keys.len();
    for (label, k) in keys {
        let raw = data_encoding::BASE64URL_NOPAD.decode(k.as_bytes()).unwrap();
        c.add_bytes(&label, &raw);
    }
    n
}

/// A mobile-style sink that copies each verified file out and, while the transfer's
/// staging record still exists, collects the tokens the receiver issued for it.
#[derive(Debug)]
struct TokenSink {
    staging: PathBuf,
    out: PathBuf,
    tokens: Mutex<Vec<String>>,
}

impl TokenSink {
    fn harvest(&self) {
        let mut records = Vec::new();
        find_all(&self.staging, "transfer.json", &mut records);
        let mut got = self.tokens.lock().unwrap();
        for r in records {
            let Ok(bytes) = std::fs::read(&r) else {
                continue;
            };
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            let mut found: Vec<String> = v["tokens"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|t| t.as_str().map(str::to_owned))
                .collect();
            if let Some(b) = v["bundleToken"].as_str() {
                found.push(b.to_owned());
            }
            for t in found {
                if !got.contains(&t) {
                    got.push(t);
                }
            }
        }
    }
}

impl ReceiveSink for TokenSink {
    fn deliver(&self, item: DeliverItem, mut file: std::fs::File) -> Result<String, SinkError> {
        self.harvest();
        let dest = item.path.iter().fold(self.out.clone(), |p, c| p.join(c));
        std::fs::create_dir_all(dest.parent().unwrap())
            .map_err(|e| SinkError::new(e.to_string()))?;
        let mut out = std::fs::File::create(&dest).map_err(|e| SinkError::new(e.to_string()))?;
        std::io::copy(&mut file, &mut out).map_err(|e| SinkError::new(e.to_string()))?;
        Ok(format!("sink://{}", item.path.join("/")))
    }

    fn make_dir(&self, _path: Vec<String>) -> Result<(), SinkError> {
        Ok(())
    }
}

fn hex_canary(rng: &mut Rng, prefix: &str) -> String {
    format!("{prefix}-{:016x}{:016x}", rng.next_u64(), rng.next_u64())
}

fn input_start() -> Input {
    Input::Session {
        op: SessionOp::Start,
    }
}

fn is_session(e: &HostEvent, want: &SessionOp) -> bool {
    matches!(e, HostEvent::Input { input: Input::Session { op }, .. } if op == want)
}

/// Writes a file that starts with `canary` and continues with seeded random bytes.
fn write_canary_file(path: &Path, canary: &str, size: usize, seed: u64) {
    let mut rng = Rng(seed | 1);
    let mut body = vec![0u8; size];
    rng.fill(&mut body);
    body[..canary.len()].copy_from_slice(canary.as_bytes());
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(&body).unwrap();
}

/// Sends `paths` from `from` to `to` and waits until both hosts have emitted the
/// transfer's completion event, which comes after every placement and its log line.
async fn transfer(from: &mut TestHost, to: &mut TestHost, paths: Vec<PathBuf>) {
    to.events.clear();
    from.events.clear();
    let mut h = from.host.send(&to.host.device_id(), paths).await.unwrap();
    let summary = tokio::time::timeout(Duration::from_secs(30), h.wait())
        .await
        .expect("send finished in time")
        .expect("send succeeded");
    for side in [&mut *from, &mut *to] {
        side.events
            .wait_for(
                T,
                |e| matches!(e, HostEvent::TransferComplete { id, .. } if *id == summary.id),
            )
            .await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
    clippy::too_many_lines,
    reason = "one scripted run, read top to bottom"
)]
async fn sr32_no_secret_reaches_the_logs_or_the_diagnostics_export() {
    let cap = capture();
    // Fresh canaries every run, so a stale string elsewhere cannot make the test pass.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap();
    let mut rng = Rng(now.as_secs() ^ (u64::from(now.subsec_nanos()) << 32) | 1);
    let mut c = Canaries::default();

    // alpha: a desktop that accepts input, receiving through a mobile-style sink so the
    // tokens it issues can be read while each transfer is staged. bravo: a plain host.
    let root = temp_root();
    let sink = Arc::new(TokenSink {
        staging: root.path().join("alpha-staging"),
        out: root.path().join("alpha-out"),
        tokens: Mutex::new(Vec::new()),
    });
    let s2 = Arc::clone(&sink);
    let mut alpha = start_host_with(root.path(), "alpha", 0, move |cfg| {
        cfg.accepts_input = true;
        cfg.receive = ReceiveTarget::Staged {
            staging_dir: s2.staging.clone(),
            sink: s2,
        };
    })
    .await;
    let mut bravo = start_host_with(root.path(), "bravo", 0, |_| {}).await;
    let alpha_id = alpha.host.device_id();
    let bravo_id = bravo.host.device_id();
    for id in [alpha_id, bravo_id] {
        c.add("full Device ID (hex)", id.to_hex());
        c.add(
            "full Device ID (base32)",
            data_encoding::BASE32_NOPAD
                .encode(id.spki_hash())
                .to_ascii_lowercase(),
        );
    }
    harvest_private_key(&alpha, &mut c);
    harvest_private_key(&bravo, &mut c);
    harvest_announce_keys(&alpha, &mut c);
    harvest_announce_keys(&bravo, &mut c);

    // 1. SAS pairing; the six digits are a canary.
    let sas = pair(&mut bravo, &mut alpha).await;
    c.standalone.push(("SAS".into(), sas));
    // Announce keys travel over the new session (protocol.md 8.2).
    tokio::time::sleep(Duration::from_millis(300)).await;
    harvest_announce_keys(&alpha, &mut c);
    harvest_announce_keys(&bravo, &mut c);

    // 2. Unpair (rotates both local announce keys), then QR pairing.
    alpha.events.clear();
    bravo.events.clear();
    bravo.host.unpair(&alpha_id).await.unwrap();
    alpha
        .events
        .wait_for(T, |e| matches!(e, HostEvent::Unpaired { .. }))
        .await;
    harvest_announce_keys(&alpha, &mut c);
    harvest_announce_keys(&bravo, &mut c);
    alpha.events.clear();
    bravo.events.clear();
    let uri = alpha.host.show_qr().unwrap();
    let secret = uri
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("secret="))
        .expect("the QR URI carries a secret")
        .to_owned();
    c.add("QR one-time secret", secret);
    bravo.host.scan_qr(&uri).await.unwrap();
    alpha
        .events
        .wait_for(T, |e| {
            matches!(
                e,
                HostEvent::PairingPrompt {
                    prompt: PairingPromptKind::ScannerConfirm,
                    ..
                }
            )
        })
        .await;
    alpha.host.pairing_respond(true);
    for h in [&mut alpha, &mut bravo] {
        h.events
            .wait_for(T, |e| matches!(e, HostEvent::Paired { .. }))
            .await;
        h.events
            .wait_for(T, |e| matches!(e, HostEvent::PeerOnline { .. }))
            .await;
    }
    // Wait until each side has stored the other's announce key (an `announce_key` frame
    // over the new session), so both local keys and both peer copies are canaries.
    let deadline = tokio::time::Instant::now() + T;
    loop {
        let keys = harvest_announce_keys(&alpha, &mut c) + harvest_announce_keys(&bravo, &mut c);
        if keys == 4 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "announce keys never exchanged ({keys} on disk)"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // 3. Clipboard: a short clip inline and a long one (over 32 KiB) through a blob.
    let clip = hex_canary(&mut rng, "canary-clip");
    c.add("clipboard text", clip.clone());
    assert_eq!(
        bravo.host.push_clipboard(&clip, false).unwrap().sent_to,
        [alpha_id]
    );
    alpha
        .events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { text, .. } if text.0.as_deref() == Some(clip.as_str())))
        .await;
    let long_canary = hex_canary(&mut rng, "canary-longclip");
    c.add("long clipboard text", long_canary.clone());
    let long = format!("{}{long_canary}{}", "q".repeat(20_000), "z".repeat(20_000));
    assert!(long.len() > 32 * 1024);
    assert_eq!(
        bravo.host.push_clipboard(&long, false).unwrap().sent_to,
        [alpha_id]
    );
    alpha
        .events
        .wait_for(T, |e| matches!(e, HostEvent::Clipboard { text, .. } if text.0.as_deref() == Some(long.as_str())))
        .await;

    // 4. Files at TRACE into the sink: a streamed file and a bundled one; names and
    // contents are canaries (names may appear at TRACE, contents never).
    let src = root.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    let big_name = format!("{}.bin", hex_canary(&mut rng, "canary-name-trace"));
    let small_name = format!("{}.txt", hex_canary(&mut rng, "canary-name-trace"));
    let big_body = hex_canary(&mut rng, "canary-body");
    let small_body = hex_canary(&mut rng, "canary-body");
    c.add("file contents", big_body.clone());
    c.add("file contents", small_body.clone());
    write_canary_file(&src.join(&big_name), &big_body, 600 * 1024, 11);
    write_canary_file(&src.join(&small_name), &small_body, 2_000, 12);
    transfer(
        &mut bravo,
        &mut alpha,
        vec![src.join(&big_name), src.join(&small_name)],
    )
    .await;
    assert!(sink.out.join(&big_name).is_file() && sink.out.join(&small_name).is_file());

    // 5. Remote input: bravo types a canary into alpha.
    let mut g = alpha.host.grants(&bravo_id).unwrap();
    g.input = true;
    alpha.host.set_grants(&bravo_id, g).await.unwrap();
    alpha.events.clear();
    bravo.host.send_input(&alpha_id, input_start()).unwrap();
    alpha
        .events
        .wait_for(T, |e| is_session(e, &SessionOp::Start))
        .await;
    let typed = hex_canary(&mut rng, "canary-typed");
    c.add("typed input text", typed.clone());
    bravo
        .host
        .send_input(
            &alpha_id,
            Input::Key(KeyInput {
                key: None,
                text: Some(typed.clone()),
                mods: vec![],
                action: KeyAction::Press,
            }),
        )
        .unwrap();
    alpha
        .events
        .wait_for(T, |e| {
            matches!(e, HostEvent::Input { input: Input::Key(k), .. } if k.text.as_deref() == Some(typed.as_str()))
        })
        .await;
    bravo
        .host
        .send_input(
            &alpha_id,
            Input::Session {
                op: SessionOp::Stop,
            },
        )
        .unwrap();
    alpha
        .events
        .wait_for(T, |e| is_session(e, &SessionOp::Stop))
        .await;

    // 6. The level rule for file names: none at INFO, present at DEBUG. alpha -> bravo
    // lands in a plain receive folder, so both receive targets are covered.
    let info_name = format!("{}.bin", hex_canary(&mut rng, "canary-name-info"));
    let debug_name = format!("{}.bin", hex_canary(&mut rng, "canary-name-debug"));
    write_canary_file(
        &src.join(&info_name),
        &hex_canary(&mut rng, "x"),
        300 * 1024,
        13,
    );
    write_canary_file(
        &src.join(&debug_name),
        &hex_canary(&mut rng, "x"),
        300 * 1024,
        14,
    );
    // Each window is bounded by offsets taken while its level is still in force: the
    // start after the level is set, the end after both completion events and before the
    // level changes again, so lines from another level cannot land in it.
    cap.set_level(LevelFilter::INFO);
    let info_from = cap.offset();
    transfer(&mut alpha, &mut bravo, vec![src.join(&info_name)]).await;
    let info_to = cap.offset();
    cap.set_level(LevelFilter::DEBUG);
    let debug_from = cap.offset();
    transfer(&mut alpha, &mut bravo, vec![src.join(&debug_name)]).await;
    let debug_to = cap.offset();
    cap.set_level(LevelFilter::TRACE);
    assert!(bravo.recv_dir.join(&info_name).is_file());
    assert!(bravo.recv_dir.join(&debug_name).is_file());
    let info_part = cap.slice(info_from, info_to);
    let info_leaks: Vec<&str> = info_part
        .lines()
        .filter(|l| l.contains(info_name.as_str()))
        .collect();
    assert!(
        info_leaks.is_empty(),
        "file name logged while the level was INFO: {info_leaks:?}"
    );
    let debug_part = cap.slice(debug_from, debug_to);
    let named: Vec<&str> = debug_part
        .lines()
        .filter(|l| l.contains(debug_name.as_str()))
        .collect();
    assert!(
        !named.is_empty(),
        "a transfer at DEBUG logs its file name; without that this test would prove nothing"
    );
    assert!(
        named.iter().all(|l| l.trim_start().starts_with("DEBUG")),
        "file names only at DEBUG: {named:?}"
    );

    // 7. Every token alpha issued (seen through the sink while staged).
    let tokens = sink.tokens.lock().unwrap().clone();
    assert!(
        tokens.len() >= 2,
        "the streamed file and the bundle each got a token: {}",
        tokens.len()
    );
    for t in tokens {
        c.add("file token", t);
    }

    // 8. The diagnostics export of both hosts.
    let diag_alpha = serde_json::to_string_pretty(&alpha.host.diagnose()).unwrap();
    let diag_bravo = serde_json::to_string_pretty(&bravo.host.diagnose()).unwrap();

    alpha.host.shutdown().await;
    bravo.host.shutdown().await;
    harvest_announce_keys(&alpha, &mut c);
    harvest_announce_keys(&bravo, &mut c);

    let captured = cap.text();
    assert!(
        captured.contains("host started") && captured.contains("paired"),
        "the capture saw the core's own log lines"
    );
    assert!(
        captured.lines().any(|l| l.starts_with("TRACE h2::")),
        "the capture saw dependency TRACE lines too (every target)"
    );
    // The level rule is about INFO and above only: at TRACE the name may appear, and the
    // capture must be able to see it (a positive control for the matcher).
    assert!(
        captured.contains(big_name.as_str()),
        "TRACE run logged its file name"
    );
    // Numbers this run produced that a core line may legitimately carry: file sizes and
    // their sums, clip lengths, ports, and chunk offsets up to the largest file.
    let sizes = [600 * 1024u64, 2_000, 300 * 1024];
    for n in sizes {
        c.known_number(n);
    }
    c.known_number(600 * 1024 + 2_000);
    c.known_number(clip.len());
    c.known_number(long.len());
    c.known_number(alpha.port);
    c.known_number(bravo.port);
    let chunk = u64::from(playdrop_core::constants::DEFAULT_CHUNK_SIZE);
    let mut off = chunk;
    while off <= 600 * 1024 {
        c.known_number(off);
        off += chunk;
    }
    let core_log = cap.core_text();
    assert!(
        core_log.contains("host started") && !core_log.contains(" h2::"),
        "the core-only capture holds playdrop lines and no dependency lines"
    );
    // Positive control for the standalone matcher: a planted SAS is found.
    let sas = c.standalone[0].1.clone();
    if !c.known_numbers.contains(&sas) {
        assert_eq!(
            c.standalone_leaks("self-check", &format!("DEBUG playdrop_core: code={sas} x"))
                .len(),
            1
        );
    }
    let mut leaks = c.leaks("log", &captured);
    leaks.extend(c.standalone_leaks("core log", &core_log));
    for (name, diag) in [
        ("alpha diagnostics", &diag_alpha),
        ("bravo diagnostics", &diag_bravo),
    ] {
        leaks.extend(c.leaks(name, diag));
        leaks.extend(c.standalone_leaks(name, diag));
    }
    println!(
        "SR-32 canary: {} log bytes, {} canaries, {} standalone tokens, {} leaks",
        captured.len(),
        c.needles.len(),
        c.standalone.len(),
        leaks.len()
    );
    assert!(leaks.is_empty(), "SR-32 leaks:\n{}", leaks.join("\n"));
}
