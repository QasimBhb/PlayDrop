//! The update check against a stand-in HTTP server on 127.0.0.1 with a throwaway key
//! (`docs/design/update-check.md` section 8): the request head byte for byte (SR-45), the
//! once-a-day rule and the switch, every SR-46 refusal, every SR-48 limit, "Later", and
//! the install step with a fake installer (SR-47). Nothing here contacts GitHub.
//!
//! The vectors were made once with OpenSSL (`playdropd` has no Ed25519 signer):
//! `openssl genpkey -algorithm ed25519`, then for each checksum file
//! `openssl pkeyutl -sign -inkey key.pem -rawin -in SUMS | base64 -w0`. The private half
//! was not kept.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use playdrop_core::update::{
    key_from_hex, Version, CHECK_EVERY_MS, MAX_SIG_LEN, MAX_SUMS_LEN, RETRY_AFTER_FAILURE_MS,
};
use playdrop_ipc::{UpdateStage, UpdateState, UpdateStatus};
use tokio::sync::mpsc;

use super::fetch::{FetchConfig, MinProgress, ProxyChoice, RedirectRule};
use super::install::{InstallFailure, Installer, Relaunch, DISMISSED};
use super::kind::Install;
use super::{Setup, Updater, FLATPAK_REASON};
use playdrop_core::update::InstallKind;

/// Public half of the throwaway key.
const TEST_KEY: &str = "b8449cbb7fd6040eafaf5d2c576877eb783f8d36d9adccdbfb739b8f4a68ec15";

/// The release files of the test release 1.2.3.
const DEB: &[u8] = b"PlayDrop 1.2.3 deb, test bytes\n";
const APPLET: &[u8] = b"PlayDrop applet 1.2.3 deb, test bytes\n";
const EXE: &[u8] = b"PlayDrop 1.2.3 Windows installer, test bytes\n";

/// Release 1.2.3 with the `.deb`, the Windows installer and the applet.
const SUMS_A: &str = "\
7de919f3f0a0592205ad23c3c8a687deea32ed8791072b5ae8a36c6512e694dd  playdrop-1.2.3-linux-amd64.deb
65ee7c114a378fbca6bf345e1cb6b02519ad160edddcff8d0ce1f2a36a4e83c3  playdrop-1.2.3-windows-x64-setup.exe
39350d66db9ec0e3d3e84878e59279372242a867aec9825d317cd6dade26d308  playdrop-cosmic-applet-1.2.3-linux-amd64.deb
";
const SIG_A: &str =
    "jPXfmI3UM+OgNiyhnQd+nZ77e1U1TRxTT60cwGSsxlTogo5ATEf/BUeQVVfU+beKNAnjInn8T/XooZCOSbypDw==";

/// Release 1.2.3 with no `.deb` and no applet: the `.deb`'s bytes are signed only as the
/// Android file.
const SUMS_C: &str = "\
65ee7c114a378fbca6bf345e1cb6b02519ad160edddcff8d0ce1f2a36a4e83c3  playdrop-1.2.3-windows-x64-setup.exe
7de919f3f0a0592205ad23c3c8a687deea32ed8791072b5ae8a36c6512e694dd  playdrop-1.2.3-android.apk
";
const SIG_C: &str =
    "E0qkqXIjvU9lV/XbgrZ3bBRTu6Ck0znWPdsGT+HiK44rXiqYqoghlC6zInkbhVwYXWsim5jLVJwQjdnov5uFDw==";

/// Release 1.2.3 with the `.deb` and the Windows installer but no applet package, as a
/// release after the transitional `playdrop-cosmic-applet` is dropped will be. Signed with a
/// second throwaway key, made the same way, which [`Rig`] also trusts (a key list, as in
/// a rotation).
const SUMS_D: &str = "\
7de919f3f0a0592205ad23c3c8a687deea32ed8791072b5ae8a36c6512e694dd  playdrop-1.2.3-linux-amd64.deb
65ee7c114a378fbca6bf345e1cb6b02519ad160edddcff8d0ce1f2a36a4e83c3  playdrop-1.2.3-windows-x64-setup.exe
";
const SIG_D: &str =
    "xDilYY8NHkM+512YblL1zkNdGL8mIlaNZYNCJ8ydOfIfVZPCY3yjThxC20PKFXy9/c76enSYCVBSd4k2QUUVCA==";
/// Public half of the second throwaway key, which signed only [`SUMS_D`].
const TEST_KEY_D: &str = "64e01fd69ed8c8b2eafcc86e1423b7bf4268842448d0f5a0b26af01bf66e3a75";

const PATH_BASE: &str = "/QasimBhb/PlayDrop/releases";
const SUMS_PATH: &str = "/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS";
const SIG_PATH: &str = "/QasimBhb/PlayDrop/releases/latest/download/SHA256SUMS.sig";
const DEB_PATH: &str = "/QasimBhb/PlayDrop/releases/download/v1.2.3/playdrop-1.2.3-linux-amd64.deb";
const APPLET_PATH: &str =
    "/QasimBhb/PlayDrop/releases/download/v1.2.3/playdrop-cosmic-applet-1.2.3-linux-amd64.deb";
const EXE_PATH: &str =
    "/QasimBhb/PlayDrop/releases/download/v1.2.3/playdrop-1.2.3-windows-x64-setup.exe";

/// One answer of the stand-in.
#[derive(Clone)]
enum Reply {
    /// 200 with this body.
    Body(Vec<u8>),
    /// 200 that promises `promised` bytes, sends `body` and closes.
    Short { body: Vec<u8>, promised: usize },
    /// A status with an empty body.
    Status(u16),
    /// 302 to this location.
    Redirect(String),
    /// Waits this long before answering with the body.
    Slow(Duration, Vec<u8>),
    /// Answers at once, then sends the body one byte per interval.
    Trickle(Duration, Vec<u8>),
}

/// A plain HTTP/1.1 server on 127.0.0.1 that records every request head.
struct StandIn {
    port: u16,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
    heads: Arc<Mutex<Vec<String>>>,
    connections: Arc<AtomicUsize>,
}

impl StandIn {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let routes: Arc<Mutex<HashMap<String, Reply>>> = Arc::default();
        let heads: Arc<Mutex<Vec<String>>> = Arc::default();
        let connections = Arc::new(AtomicUsize::new(0));
        let (r, h, c) = (
            Arc::clone(&routes),
            Arc::clone(&heads),
            Arc::clone(&connections),
        );
        // The thread lives as long as the test process; each test has its own port.
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                c.fetch_add(1, Ordering::SeqCst);
                let (r, h) = (Arc::clone(&r), Arc::clone(&h));
                std::thread::spawn(move || serve(stream, &r, &h));
            }
        });
        Self {
            port,
            routes,
            heads,
            connections,
        }
    }

    fn origin(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn route(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.to_owned(), reply);
    }

    /// The release: signed sums and every file.
    fn release(&self, sums: &str, sig: &str) {
        self.route(SUMS_PATH, Reply::Body(sums.as_bytes().to_vec()));
        self.route(SIG_PATH, Reply::Body(sig.as_bytes().to_vec()));
        self.route(DEB_PATH, Reply::Body(DEB.to_vec()));
        self.route(APPLET_PATH, Reply::Body(APPLET.to_vec()));
        self.route(EXE_PATH, Reply::Body(EXE.to_vec()));
    }

    fn connections(&self) -> usize {
        self.connections.load(Ordering::SeqCst)
    }

    fn heads(&self) -> Vec<String> {
        self.heads.lock().unwrap().clone()
    }

    fn paths(&self) -> Vec<String> {
        self.heads()
            .iter()
            .filter_map(|h| h.split(' ').nth(1).map(str::to_owned))
            .collect()
    }
}

fn serve(
    mut stream: TcpStream,
    routes: &Mutex<HashMap<String, Reply>>,
    heads: &Mutex<Vec<String>>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") && head.len() < 16 * 1024 {
        match stream.read(&mut byte) {
            Ok(1) => head.push(byte[0]),
            _ => return,
        }
    }
    let head = String::from_utf8_lossy(&head).into_owned();
    let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
    heads.lock().unwrap().push(head);
    let reply = routes
        .lock()
        .unwrap()
        .get(&path)
        .cloned()
        .unwrap_or(Reply::Status(404));
    if let Reply::Trickle(every, body) = &reply {
        let head = format!(
            "HTTP/1.1 200 X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        for b in body {
            std::thread::sleep(*every);
            if stream.write_all(&[*b]).is_err() {
                return;
            }
        }
        return;
    }
    let (status, body, promised, location) = match reply {
        Reply::Trickle(..) => unreachable!("answered above"),
        Reply::Body(b) => (200, b.clone(), b.len(), None),
        Reply::Short { body, promised } => (200, body, promised, None),
        Reply::Status(s) => (s, Vec::new(), 0, None),
        Reply::Redirect(to) => (302, Vec::new(), 0, Some(to)),
        Reply::Slow(wait, b) => {
            std::thread::sleep(wait);
            (200, b.clone(), b.len(), None)
        }
    };
    let mut out =
        format!("HTTP/1.1 {status} X\r\ncontent-length: {promised}\r\nconnection: close\r\n");
    if let Some(to) = location {
        let _ = write!(out, "location: {to}\r\n");
    }
    out.push_str("\r\n");
    let _ = stream.write_all(out.as_bytes());
    let _ = stream.write_all(&body);
    let _ = stream.flush();
}

/// A fake install step: records what it was asked and answers as told.
#[derive(Default)]
struct FakeInstaller {
    calls: Mutex<Vec<Vec<PathBuf>>>,
    /// `pkexec` exit code to fail with; `None` succeeds.
    fail_with: Option<i32>,
    /// What the Windows installer was asked to start again.
    relaunches: Mutex<Vec<Relaunch>>,
    /// The Windows installer's exit code (0 when unset).
    windows_exit: Option<u32>,
}

impl Installer for FakeInstaller {
    fn install_debs(&self, files: &[PathBuf]) -> Result<(), InstallFailure> {
        // The files are there, whole, when the prompt would show.
        for f in files {
            assert!(f.is_absolute() && f.is_file(), "{}", f.display());
        }
        self.calls.lock().unwrap().push(files.to_vec());
        match self.fail_with {
            None => Ok(()),
            Some(code) => Err(InstallFailure(super::install::pkexec_failure(Some(code)))),
        }
    }

    fn run_windows_installer(&self, file: &Path, relaunch: Relaunch) -> Result<(), InstallFailure> {
        assert!(file.is_absolute() && file.is_file(), "{}", file.display());
        self.calls.lock().unwrap().push(vec![file.to_owned()]);
        self.relaunches.lock().unwrap().push(relaunch);
        match self.windows_exit.unwrap_or(0) {
            0 => Ok(()),
            code => Err(InstallFailure(super::install::inno_exit_sentence(code))),
        }
    }
}

/// An updater pointed at the stand-in, with a clock the test moves.
struct Rig {
    server: StandIn,
    tmp: tempfile::TempDir,
    clock: Arc<AtomicU64>,
    installer: Arc<FakeInstaller>,
    events: mpsc::UnboundedReceiver<UpdateStatus>,
    updater: Arc<Updater>,
    restarts: Arc<AtomicUsize>,
}

#[derive(Clone)]
struct Options {
    /// `None`: a build that is not a release.
    running: Option<&'static str>,
    install: Install,
    /// What the install step's own applet question answers; `None` repeats
    /// `install.applet`.
    applet_now: Option<Result<bool, String>>,
    enabled: bool,
    timeout: Duration,
    download_deadline: Duration,
    min_progress: MinProgress,
    fail_with: Option<i32>,
    windows_exit: Option<u32>,
    /// Replaces the fake installer.
    installer: Option<Arc<dyn Installer>>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            running: Some("1.2.2"),
            install: Install {
                kind: InstallKind::Deb,
                applet: false,
            },
            applet_now: None,
            enabled: true,
            timeout: Duration::from_secs(5),
            download_deadline: super::fetch::DOWNLOAD_DEADLINE,
            min_progress: super::fetch::MIN_PROGRESS,
            fail_with: None,
            windows_exit: None,
            installer: None,
        }
    }
}

const START: u64 = 1_800_000_000_000;

impl Rig {
    fn new(o: Options) -> Self {
        let server = StandIn::start();
        let tmp = tempfile::tempdir().unwrap();
        Self::with(server, tmp, o)
    }

    fn with(server: StandIn, tmp: tempfile::TempDir, o: Options) -> Self {
        let clock = Arc::new(AtomicU64::new(START));
        let installer = Arc::new(FakeInstaller {
            fail_with: o.fail_with,
            windows_exit: o.windows_exit,
            ..FakeInstaller::default()
        });
        let (tx, events) = mpsc::unbounded_channel();
        let c = Arc::clone(&clock);
        let install = o.install;
        let applet_now = o.applet_now.unwrap_or(Ok(install.applet));
        let origin = server.origin();
        let setup = Setup {
            data_dir: tmp.path().join("data"),
            downloads_dir: tmp.path().join("data").join("updates"),
            running: o.running.map(|v| Version::parse(v).unwrap()),
            install: Arc::new(move || install),
            applet_installed: Arc::new(move || applet_now.clone()),
            releases_base: format!("{origin}{PATH_BASE}"),
            keys: vec![
                key_from_hex(TEST_KEY).unwrap(),
                key_from_hex(TEST_KEY_D).unwrap(),
            ],
            fetch: FetchConfig {
                timeout: o.timeout,
                download_deadline: o.download_deadline,
                min_progress: o.min_progress,
                redirects: RedirectRule {
                    test_origin: Some(origin),
                },
                proxy: ProxyChoice::Direct,
                https_only: false,
            },
            installer: o
                .installer
                .unwrap_or_else(|| Arc::clone(&installer) as Arc<dyn Installer>),
            now_ms: Arc::new(move || c.load(Ordering::SeqCst)),
        };
        std::fs::create_dir_all(tmp.path().join("data")).unwrap();
        let updater = Updater::new(
            setup,
            o.enabled,
            Box::new(move |s| {
                let _ = tx.send(s);
            }),
        );
        let restarts = Arc::new(AtomicUsize::new(0));
        let r = Arc::clone(&restarts);
        updater.set_on_installed(Box::new(move || {
            r.fetch_add(1, Ordering::SeqCst);
        }));
        Self {
            server,
            tmp,
            clock,
            installer,
            events,
            updater,
            restarts,
        }
    }

    fn advance(&self, ms: u64) {
        self.clock.fetch_add(ms, Ordering::SeqCst);
    }

    async fn check(&self, force: bool) -> UpdateState {
        self.updater.check(force).await.state
    }

    /// Waits for an announced state `done` accepts.
    async fn wait_for(&mut self, done: impl Fn(&UpdateState) -> bool) -> UpdateState {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let s = self.events.recv().await.expect("updater gone");
                if done(&s.state) {
                    return s.state;
                }
            }
        })
        .await
        .expect("state in time")
    }

    /// Starts a `.deb` install and waits for its end.
    async fn install(&mut self) -> UpdateState {
        self.updater
            .start_install(Relaunch::Window)
            .expect("install starts");
        self.wait_for(|s| {
            matches!(
                s,
                UpdateState::Restarting { .. } | UpdateState::Failed { .. }
            )
        })
        .await
    }

    fn downloads_left(&self) -> usize {
        std::fs::read_dir(self.tmp.path().join("data").join("updates")).map_or(0, Iterator::count)
    }
}

fn available_version(state: &UpdateState) -> Option<(&str, bool)> {
    match state {
        UpdateState::Available { offer } => Some((offer.version.as_str(), offer.can_install)),
        _ => None,
    }
}

fn last_failure(rig: &Rig) -> String {
    rig.updater
        .status()
        .last_failure
        .expect("a failure is reported")
}

fn failed(state: &UpdateState) -> Option<(UpdateStage, &str)> {
    match state {
        UpdateState::Failed { stage, message, .. } => Some((*stage, message.as_str())),
        _ => None,
    }
}

/// SR-45: the request is the fixed path, `Host` and `User-Agent: PlayDrop`, nothing else.
#[tokio::test]
async fn the_request_head_is_fixed_byte_for_byte() {
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    assert_eq!(
        available_version(&rig.check(false).await),
        Some(("1.2.3", true))
    );
    let heads = rig.server.heads();
    let port = rig.server.port;
    assert_eq!(
        heads,
        vec![
            format!(
                "GET {SUMS_PATH} HTTP/1.1\r\nuser-agent: PlayDrop\r\nhost: 127.0.0.1:{port}\r\n\r\n"
            ),
            format!(
                "GET {SIG_PATH} HTTP/1.1\r\nuser-agent: PlayDrop\r\nhost: 127.0.0.1:{port}\r\n\r\n"
            ),
        ]
    );
    println!("request head:\n{}", heads[0]);
}

/// SR-45: with the switch off nothing leaves the computer, not even with "Check now".
#[tokio::test]
async fn the_switch_off_makes_no_connection() {
    let rig = Rig::new(Options {
        enabled: false,
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    assert_eq!(rig.check(false).await, UpdateState::Off);
    assert_eq!(rig.check(true).await, UpdateState::Off);
    assert_eq!(rig.server.connections(), 0);
    assert!(rig.updater.start_install(Relaunch::Window).is_err());
    // Switched on, the next opening checks.
    rig.updater.set_enabled(true);
    assert!(available_version(&rig.check(false).await).is_some());
    assert_eq!(rig.server.connections(), 2);
    // Switched off again, the offer is hidden at once.
    rig.updater.set_enabled(false);
    assert_eq!(rig.updater.status().state, UpdateState::Off);
}

/// SR-45: once a day; "Check now" skips the wait.
#[tokio::test]
async fn a_second_check_within_a_day_makes_no_connection_unless_forced() {
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    assert_eq!(rig.server.connections(), 2);
    rig.advance(CHECK_EVERY_MS - 1);
    assert!(available_version(&rig.check(false).await).is_some());
    assert_eq!(rig.server.connections(), 2, "not due yet");
    rig.check(true).await;
    assert_eq!(rig.server.connections(), 4, "forced");
    rig.advance(CHECK_EVERY_MS);
    rig.check(false).await;
    assert_eq!(rig.server.connections(), 6, "due again");
    let status = rig.updater.status();
    assert_eq!(status.last_checked_ms, Some(START + 2 * CHECK_EVERY_MS - 1));
    assert_eq!(status.running_version, "1.2.2");
}

/// SR-45: a failed check may be tried again only after an hour.
#[tokio::test]
async fn a_failure_is_retried_only_after_an_hour() {
    let rig = Rig::new(Options::default());
    // Nothing published: GitHub answers 404.
    let (stage, message) = failed(&rig.check(false).await)
        .map(|(s, m)| (s, m.to_owned()))
        .unwrap();
    assert_eq!(stage, UpdateStage::Check);
    assert!(
        message.contains("no signed update information"),
        "{message}"
    );
    assert_eq!(
        rig.server.connections(),
        1,
        "the signature is not asked for"
    );
    assert_eq!(rig.updater.status().last_failed_ms, Some(START));
    rig.server.release(SUMS_A, SIG_A);
    rig.advance(RETRY_AFTER_FAILURE_MS - 1);
    rig.check(false).await;
    assert_eq!(rig.server.connections(), 1);
    rig.advance(1);
    assert!(available_version(&rig.check(false).await).is_some());
    assert_eq!(rig.server.connections(), 3);
    assert_eq!(rig.updater.status().last_failed_ms, None);
}

/// SR-46: a changed byte in the checksum file or in the signature is refused.
#[tokio::test]
async fn a_flipped_byte_in_the_sums_or_the_signature_is_refused() {
    let mut sums = SUMS_A.as_bytes().to_vec();
    sums[3] ^= 1;
    let rig = Rig::new(Options::default());
    rig.server.route(SUMS_PATH, Reply::Body(sums));
    rig.server
        .route(SIG_PATH, Reply::Body(SIG_A.as_bytes().to_vec()));
    let state = rig.check(true).await;
    assert_eq!(failed(&state).map(|f| f.0), Some(UpdateStage::Check));
    assert!(
        failed(&state).unwrap().1.contains("not signed"),
        "{state:?}"
    );

    // One base64 character changed: a different signature.
    let sig = SIG_A.replacen('j', "k", 1);
    rig.server.release(SUMS_A, &sig);
    let state = rig.check(true).await;
    assert!(
        failed(&state).unwrap().1.contains("not signed"),
        "{state:?}"
    );
    // The real key list alone refuses the test signature too.
    assert!(playdrop_core::update::verify_sums(SUMS_A.as_bytes(), SIG_A.as_bytes()).is_err());
}

/// SR-46: the same or an older version is never offered.
#[tokio::test]
async fn the_same_or_an_older_version_is_not_offered() {
    for running in ["1.2.3", "1.3.0"] {
        let mut rig = Rig::new(Options {
            running: Some(running),
            ..Options::default()
        });
        rig.server.release(SUMS_A, SIG_A);
        assert_eq!(rig.check(false).await, UpdateState::UpToDate, "{running}");
        assert!(rig.updater.start_install(Relaunch::Window).is_err());
        // Nothing to install, so nothing was downloaded.
        assert_eq!(rig.server.paths().len(), 2);
        while rig.events.try_recv().is_ok() {}
    }
}

/// SR-46: a changed byte in the downloaded file is refused and the file deleted.
#[tokio::test]
async fn a_flipped_byte_in_the_file_is_refused_and_deleted() {
    let mut rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    let mut deb = DEB.to_vec();
    deb[0] ^= 1;
    rig.server.route(DEB_PATH, Reply::Body(deb));
    rig.check(false).await;
    let state = rig.install().await;
    assert_eq!(
        failed(&state),
        Some((
            UpdateStage::Verify,
            "The download did not match its signed checksum and was deleted."
        ))
    );
    assert_eq!(rig.downloads_left(), 0);
    assert!(
        rig.installer.calls.lock().unwrap().is_empty(),
        "never installed"
    );
    // The offer stays, so Update can be pressed again.
    assert!(matches!(state, UpdateState::Failed { offer: Some(_), .. }));
}

/// SR-46: bytes signed under another name do not pass for this file.
#[tokio::test]
async fn a_file_listed_under_another_name_is_refused() {
    // The Windows installer's bytes served as the .deb: its hash is signed, but for
    // another name.
    let mut rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(DEB_PATH, Reply::Body(EXE.to_vec()));
    rig.check(false).await;
    assert_eq!(
        failed(&rig.install().await).map(|f| f.0),
        Some(UpdateStage::Verify)
    );
    assert_eq!(rig.downloads_left(), 0);

    // The .deb's bytes are signed only as the Android file: nothing to install here.
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_C, SIG_C);
    let state = rig.check(false).await;
    assert_eq!(available_version(&state), Some(("1.2.3", false)));
    assert!(rig.updater.start_install(Relaunch::Window).is_err());
}

/// SR-48: answers over their limits end the check.
#[tokio::test]
async fn oversized_sums_and_signature_are_refused() {
    let rig = Rig::new(Options::default());
    rig.server
        .route(SUMS_PATH, Reply::Body(vec![b'a'; MAX_SUMS_LEN + 1]));
    rig.server
        .route(SIG_PATH, Reply::Body(SIG_A.as_bytes().to_vec()));
    let state = rig.check(true).await;
    assert!(
        failed(&state).unwrap().1.contains("larger than allowed"),
        "{state:?}"
    );
    assert_eq!(rig.server.paths(), vec![SUMS_PATH.to_owned()]);

    rig.server
        .route(SUMS_PATH, Reply::Body(SUMS_A.as_bytes().to_vec()));
    rig.server
        .route(SIG_PATH, Reply::Body(vec![b'A'; MAX_SIG_LEN + 1]));
    let state = rig.check(true).await;
    assert!(
        failed(&state).unwrap().1.contains("larger than allowed"),
        "{state:?}"
    );

    // A server that promises nothing and sends too much is cut off at the limit too.
    rig.server.route(
        SUMS_PATH,
        Reply::Short {
            body: vec![b'a'; MAX_SUMS_LEN + 10],
            promised: MAX_SUMS_LEN + 10,
        },
    );
    let state = rig.check(true).await;
    assert!(
        failed(&state).unwrap().1.contains("larger than allowed"),
        "{state:?}"
    );
}

/// SR-48: a download shorter than promised is refused and deleted.
#[tokio::test]
async fn a_download_of_the_wrong_length_is_refused() {
    let mut rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(
        DEB_PATH,
        Reply::Short {
            body: DEB[..10].to_vec(),
            promised: DEB.len(),
        },
    );
    rig.check(false).await;
    let state = rig.install().await;
    assert_eq!(
        failed(&state).map(|f| f.0),
        Some(UpdateStage::Download),
        "{state:?}"
    );
    assert_eq!(rig.downloads_left(), 0);
    assert!(rig.installer.calls.lock().unwrap().is_empty());
}

/// SR-48: redirects are followed at most five times, only over HTTPS to GitHub (or, in
/// this test build, to the stand-in itself).
#[tokio::test]
async fn redirects_are_limited_and_checked() {
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    let origin = rig.server.origin();
    // Five hops are fine.
    rig.server
        .route(SUMS_PATH, Reply::Redirect(format!("{origin}/hop1")));
    for i in 1..5 {
        rig.server.route(
            &format!("/hop{i}"),
            Reply::Redirect(format!("/hop{}", i + 1)),
        );
    }
    rig.server
        .route("/hop5", Reply::Body(SUMS_A.as_bytes().to_vec()));
    assert!(available_version(&rig.check(true).await).is_some());
    // Six are not.
    rig.server
        .route("/hop5", Reply::Redirect(format!("{origin}/hop6")));
    rig.server
        .route("/hop6", Reply::Body(SUMS_A.as_bytes().to_vec()));
    let before = rig.server.connections();
    rig.check(true).await;
    // The offer from the check above stays; the failure is reported beside it.
    assert!(last_failure(&rig).contains("too many times"));
    assert_eq!(
        rig.server.connections() - before,
        6,
        "the sixth target is never asked"
    );

    // Another host (the same machine under another name and port), and plain HTTP to
    // GitHub, are refused before they are contacted.
    for to in [
        format!("http://localhost:{}/x", rig.server.port),
        "http://github.com/QasimBhb/PlayDrop/releases/x".to_owned(),
        "https://evil.example/SHA256SUMS".to_owned(),
    ] {
        rig.server.route(SUMS_PATH, Reply::Redirect(to.clone()));
        let before = rig.server.connections();
        let state = rig.check(true).await;
        assert!(available_version(&state).is_some(), "{state:?}");
        assert!(last_failure(&rig).contains("does not trust"), "{to}");
        assert_eq!(rig.server.connections() - before, 1, "{to}");
    }
}

/// SR-48: a server slower than the time limit ends the check.
#[tokio::test]
async fn a_slow_server_times_out() {
    let rig = Rig::new(Options {
        timeout: Duration::from_millis(300),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(
        SUMS_PATH,
        Reply::Slow(Duration::from_secs(3), SUMS_A.as_bytes().to_vec()),
    );
    let started = std::time::Instant::now();
    let state = rig.check(true).await;
    assert!(
        failed(&state).unwrap().1.contains("did not answer in time"),
        "{state:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(2));
}

/// "Later" hides the offer until the next check that runs, also across a restart.
#[tokio::test]
async fn later_hides_the_offer_until_the_next_due_check() {
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    assert!(available_version(&rig.check(false).await).is_some());
    assert_eq!(rig.updater.later().state, UpdateState::Idle);
    rig.advance(CHECK_EVERY_MS / 2);
    assert_eq!(rig.check(false).await, UpdateState::Idle);
    assert_eq!(rig.server.connections(), 2);

    // A restart of the daemon within the day keeps it hidden, without asking anyone.
    let Rig {
        server, tmp, clock, ..
    } = rig;
    let rig = Rig::with(server, tmp, Options::default());
    rig.clock
        .store(clock.load(Ordering::SeqCst), Ordering::SeqCst);
    assert_eq!(rig.updater.status().state, UpdateState::Idle);
    assert_eq!(rig.check(false).await, UpdateState::Idle);
    assert_eq!(rig.server.connections(), 2);

    rig.advance(CHECK_EVERY_MS);
    assert!(available_version(&rig.check(false).await).is_some());
    assert_eq!(rig.server.connections(), 4);
}

/// A verified offer survives a restart of the daemon without a new request.
#[tokio::test]
async fn an_offer_survives_a_restart_and_is_verified_again() {
    let rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    let Rig { server, tmp, .. } = rig;
    let data = tmp.path().join("data");
    let rig = Rig::with(server, tmp, Options::default());
    assert_eq!(
        available_version(&rig.updater.status().state),
        Some(("1.2.3", true))
    );
    assert_eq!(rig.server.connections(), 2);
    drop(rig.updater);

    // A tampered memory is not trusted.
    let path = data.join(super::store::UPDATE_FILE);
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replace("1.2.3", "9.9.9");
    std::fs::write(&path, text).unwrap();
    let rig = Rig::with(rig.server, rig.tmp, Options::default());
    assert_eq!(rig.updater.status().state, UpdateState::Idle);
}

/// SR-47: the `.deb` goes to `pkexec apt-get` from a fresh private folder, with the
/// `playdrop-cosmic-applet` package (transitional from 0.12.0) when it is installed and
/// listed; success announces `Restarting`, restarts once and removes the files.
#[tokio::test]
async fn a_deb_update_installs_both_packages_and_restarts() {
    let mut rig = Rig::new(Options {
        install: Install {
            kind: InstallKind::Deb,
            applet: true,
        },
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    // Nothing is downloaded before "Update".
    assert_eq!(rig.server.paths().len(), 2);
    let state = rig.install().await;
    assert_eq!(
        state,
        UpdateState::Restarting {
            version: "1.2.3".into()
        }
    );
    assert_eq!(rig.restarts.load(Ordering::SeqCst), 1);
    let calls = rig.installer.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    let names: Vec<String> = calls[0]
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        [
            "playdrop-1.2.3-linux-amd64.deb",
            "playdrop-cosmic-applet-1.2.3-linux-amd64.deb"
        ]
    );
    let folder = calls[0][0].parent().unwrap().to_owned();
    assert!(folder.starts_with(rig.tmp.path().join("data").join("updates")));
    assert!(!folder.exists(), "files removed after the install");
    assert_eq!(
        rig.server.paths()[2..],
        [DEB_PATH.to_owned(), APPLET_PATH.to_owned()]
    );
}

/// SR-47: a dismissed password prompt leaves the old version and says so.
#[tokio::test]
async fn a_dismissed_prompt_fails_with_the_plain_sentence() {
    let mut rig = Rig::new(Options {
        fail_with: Some(126),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    let state = rig.install().await;
    assert_eq!(failed(&state), Some((UpdateStage::Install, DISMISSED)));
    assert_eq!(
        rig.restarts.load(Ordering::SeqCst),
        0,
        "the old version keeps running"
    );
    assert_eq!(rig.downloads_left(), 0);
    // "Update" may be pressed again; "Later" hides it.
    assert!(matches!(state, UpdateState::Failed { offer: Some(_), .. }));
    assert_eq!(rig.updater.later().state, UpdateState::Idle);
}

/// The files wait for the prompt read-only in a folder private to the user (0400 in 0500,
/// set right before the hand-over), and are removed afterwards all the same.
#[cfg(unix)]
#[tokio::test]
async fn the_download_folder_is_private_and_sealed() {
    use std::os::unix::fs::PermissionsExt as _;

    struct Peek(Mutex<Vec<u32>>);
    impl Installer for Peek {
        fn install_debs(&self, files: &[PathBuf]) -> Result<(), InstallFailure> {
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            let mut seen = self.0.lock().unwrap();
            seen.push(mode(files[0].parent().unwrap()));
            seen.push(mode(&files[0]));
            seen.push(mode(files[0].parent().unwrap().parent().unwrap()));
            Err(InstallFailure("stop here".into()))
        }
        fn run_windows_installer(&self, _: &Path, _: Relaunch) -> Result<(), InstallFailure> {
            unreachable!()
        }
    }
    let peek = Arc::new(Peek(Mutex::default()));
    let mut rig = Rig::new(Options {
        installer: Some(Arc::clone(&peek) as Arc<dyn Installer>),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    assert_eq!(
        failed(&rig.install().await).map(|f| f.0),
        Some(UpdateStage::Install)
    );
    assert_eq!(*peek.0.lock().unwrap(), vec![0o500, 0o400, 0o700]);
    assert_eq!(rig.downloads_left(), 0, "a sealed folder is still removed");
}

/// A file swapped on disk after the streamed hash passed is caught by the second hash,
/// taken right before the hand-over, and never reaches the installer.
#[test]
fn a_file_changed_after_verifying_is_not_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("d");
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("playdrop-1.2.3-linux-amd64.deb");
    std::fs::write(&path, b"not the signed bytes").unwrap();
    let signed = playdrop_core::update::verify_sums_with(
        SUMS_A.as_bytes(),
        SIG_A.as_bytes(),
        &[key_from_hex(TEST_KEY).unwrap()],
    )
    .unwrap();
    let asset = signed.asset("playdrop-1.2.3-linux-amd64.deb").unwrap();
    let err = super::seal_and_recheck(
        &dir,
        std::slice::from_ref(&path),
        std::slice::from_ref(&asset),
    )
    .unwrap_err();
    assert!(err.0.contains("changed on disk"), "{}", err.0);
    super::remove_dir(&dir);
    assert!(!dir.exists());
    // The real bytes pass.
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(&path, DEB).unwrap();
    super::seal_and_recheck(&dir, &[path], &[asset]).unwrap();
    super::remove_dir(&dir);
}

/// A Flatpak gets the notice and the release page, never an install.
#[tokio::test]
async fn a_flatpak_is_told_where_updates_come_from() {
    let rig = Rig::new(Options {
        install: Install {
            kind: InstallKind::Flatpak,
            applet: false,
        },
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    let UpdateState::Available { offer } = rig.check(false).await else {
        panic!("no offer");
    };
    assert!(!offer.can_install);
    assert_eq!(offer.reason.as_deref(), Some(FLATPAK_REASON));
    assert_eq!(
        offer.page_url,
        format!("{}{PATH_BASE}/tag/v1.2.3", rig.server.origin())
    );
    assert_eq!(
        rig.updater.start_install(Relaunch::Window).unwrap_err().0,
        FLATPAK_REASON
    );
    assert_eq!(rig.updater.page_url(), Some(offer.page_url));
}

/// The applet package installed but missing from the release (a release after the
/// transitional package is dropped). Beside this build that package can only be the
/// transitional one, which depends on `playdrop (>= 0.12.0)` and holds nothing back, so the
/// main package is installed alone and the applet package is not even asked about.
#[tokio::test]
async fn an_installed_applet_package_missing_from_the_release_does_not_block_the_install() {
    let mut rig = Rig::new(Options {
        install: Install {
            kind: InstallKind::Deb,
            applet: true,
        },
        // Would stop the update if it were asked.
        applet_now: Some(Err("must not be asked".into())),
        ..Options::default()
    });
    rig.server.release(SUMS_D, SIG_D);
    assert_eq!(
        available_version(&rig.check(false).await),
        Some(("1.2.3", true))
    );
    assert!(matches!(
        rig.install().await,
        UpdateState::Restarting { .. }
    ));
    let calls = rig.installer.calls.lock().unwrap().clone();
    let names: Vec<String> = calls[0]
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["playdrop-1.2.3-linux-amd64.deb"]);
    assert_eq!(rig.server.paths()[2..], [DEB_PATH.to_owned()]);
}

/// Windows: the installer runs from the verified file and the daemon waits for it. A
/// daemon that sees it end was not replaced, so it says why, keeps the offer and removes
/// the file; the switch names what to start again.
#[tokio::test]
async fn a_windows_installer_that_ends_without_replacing_the_daemon_is_a_failure() {
    for (exit, relaunch, words) in [
        (None, Relaunch::Window, "without replacing"),
        (Some(2), Relaunch::Daemon, "cancelled"),
        (Some(1), Relaunch::Window, "permission prompt"),
        (Some(4), Relaunch::Window, "exit code 4"),
    ] {
        let mut rig = Rig::new(Options {
            install: Install {
                kind: InstallKind::Windows,
                applet: false,
            },
            windows_exit: exit,
            ..Options::default()
        });
        rig.server.release(SUMS_A, SIG_A);
        rig.check(false).await;
        rig.updater.start_install(relaunch).unwrap();
        let seen_installing = rig
            .wait_for(|s| matches!(s, UpdateState::Installing { .. }))
            .await;
        assert!(matches!(seen_installing, UpdateState::Installing { .. }));
        let state = rig
            .wait_for(|s| matches!(s, UpdateState::Failed { .. }))
            .await;
        let (at, message) = failed(&state).unwrap();
        assert_eq!(at, UpdateStage::Install);
        assert!(message.contains(words), "{message}");
        assert!(matches!(state, UpdateState::Failed { offer: Some(_), .. }));
        let calls = rig.installer.calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1);
        assert!(calls[0][0].ends_with("playdrop-1.2.3-windows-x64-setup.exe"));
        assert_eq!(*rig.installer.relaunches.lock().unwrap(), vec![relaunch]);
        assert_eq!(rig.downloads_left(), 0, "the installer file is removed");
        assert_eq!(rig.restarts.load(Ordering::SeqCst), 0);
        // "Update" can be pressed again.
        assert!(rig.updater.start_install(relaunch).is_ok());
    }
}

/// An install that blocks until the test lets it go.
struct Gate {
    entered: AtomicBool,
    release: Mutex<bool>,
    wake: std::sync::Condvar,
}

impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: AtomicBool::new(false),
            release: Mutex::new(false),
            wake: std::sync::Condvar::new(),
        })
    }

    fn open(&self) {
        *self.release.lock().unwrap() = true;
        self.wake.notify_all();
    }

    fn wait(&self) {
        self.entered.store(true, Ordering::SeqCst);
        let mut open = self.release.lock().unwrap();
        while !*open {
            open = self.wake.wait(open).unwrap();
        }
    }
}

impl Installer for Gate {
    fn install_debs(&self, _: &[PathBuf]) -> Result<(), InstallFailure> {
        self.wait();
        Err(InstallFailure(DISMISSED.to_owned()))
    }
    fn run_windows_installer(&self, _: &Path, _: Relaunch) -> Result<(), InstallFailure> {
        unreachable!()
    }
}

/// A check while an update runs neither asks GitHub nor writes over the update's state,
/// so a second "Update" cannot start; and a check's answer that lands during an update
/// leaves the update's state alone.
#[tokio::test]
async fn a_check_never_writes_over_a_running_update() {
    let gate = Gate::new();
    let mut rig = Rig::new(Options {
        installer: Some(Arc::clone(&gate) as Arc<dyn Installer>),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    rig.updater.start_install(Relaunch::Window).unwrap();
    rig.wait_for(|s| matches!(s, UpdateState::Installing { .. }))
        .await;
    let before = rig.server.connections();
    let state = rig.check(true).await;
    assert!(matches!(state, UpdateState::Installing { .. }), "{state:?}");
    assert_eq!(
        rig.server.connections(),
        before,
        "no request during an update"
    );
    assert!(rig.updater.start_install(Relaunch::Window).is_err());

    // The answer of a check that began before the update arrives now.
    let signed = playdrop_core::update::verify_sums_with(
        SUMS_A.as_bytes(),
        SIG_A.as_bytes(),
        &[key_from_hex(TEST_KEY).unwrap()],
    )
    .unwrap();
    rig.updater
        .checked(Ok((signed, SUMS_A.to_owned(), SIG_A.to_owned())), START + 5);
    assert!(matches!(
        rig.updater.status().state,
        UpdateState::Installing { .. }
    ));
    assert!(rig.updater.start_install(Relaunch::Window).is_err());
    assert_eq!(rig.updater.status().last_checked_ms, Some(START + 5));
    gate.open();
    let state = rig
        .wait_for(|s| matches!(s, UpdateState::Failed { .. }))
        .await;
    assert_eq!(failed(&state), Some((UpdateStage::Install, DISMISSED)));
}

/// "Cancel" stops a download at once, puts the offer back, removes the files and lets
/// "Update" start again; once the system installer has the files it refuses.
#[tokio::test]
async fn cancel_stops_a_download_but_not_an_install() {
    let mut rig = Rig::new(Options::default());
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(
        DEB_PATH,
        Reply::Trickle(Duration::from_millis(100), DEB.to_vec()),
    );
    rig.check(false).await;
    assert!(rig.updater.cancel().is_err(), "nothing to cancel yet");
    rig.updater.start_install(Relaunch::Window).unwrap();
    rig.wait_for(|s| matches!(s, UpdateState::Downloading { done, .. } if *done > 0))
        .await;
    let status = rig.updater.cancel().unwrap();
    assert_eq!(available_version(&status.state), Some(("1.2.3", true)));
    // The download notices within a read, then removes its folder; nothing is installed.
    for _ in 0..300 {
        if rig.downloads_left() == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(rig.downloads_left(), 0);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(rig.installer.calls.lock().unwrap().is_empty());
    assert_eq!(
        available_version(&rig.updater.status().state),
        Some(("1.2.3", true)),
        "the cancelled run does not write over the offer"
    );

    // Once the files are with the installer, "Cancel" says so and changes nothing.
    let gate = Gate::new();
    let mut rig = Rig::new(Options {
        installer: Some(Arc::clone(&gate) as Arc<dyn Installer>),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    rig.updater.start_install(Relaunch::Window).unwrap();
    rig.wait_for(|s| matches!(s, UpdateState::Installing { .. }))
        .await;
    let refused = rig.updater.cancel().unwrap_err();
    assert!(refused.0.contains("cannot be cancelled"), "{}", refused.0);
    gate.open();
    rig.wait_for(|s| matches!(s, UpdateState::Failed { .. }))
        .await;
}

/// A download that trickles below the minimum rate, or outlasts the overall deadline, is
/// stopped and removed.
#[tokio::test]
async fn a_download_too_slow_is_stopped() {
    let mut rig = Rig::new(Options {
        min_progress: MinProgress {
            bytes: 1024,
            window: Duration::from_millis(300),
        },
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(
        DEB_PATH,
        Reply::Trickle(Duration::from_millis(50), DEB.to_vec()),
    );
    rig.check(false).await;
    let state = rig.install().await;
    assert_eq!(
        failed(&state),
        Some((
            UpdateStage::Download,
            "The download was too slow and was stopped. Try again later."
        ))
    );
    assert_eq!(rig.downloads_left(), 0);

    let mut rig = Rig::new(Options {
        download_deadline: Duration::from_millis(300),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.server.route(
        DEB_PATH,
        Reply::Trickle(Duration::from_millis(50), DEB.to_vec()),
    );
    rig.check(false).await;
    let started = std::time::Instant::now();
    let state = rig.install().await;
    assert_eq!(failed(&state).map(|f| f.0), Some(UpdateStage::Download));
    assert!(failed(&state).unwrap().1.contains("too slow"), "{state:?}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(rig.installer.calls.lock().unwrap().is_empty());
}

/// The applet is asked about again when "Update" is pressed: installed now means its
/// package goes along even when the offer was made without it, and a question that fails
/// stops the update before anything is downloaded.
#[tokio::test]
async fn the_applet_is_asked_about_again_at_install_time() {
    // Looked absent when offered, installed by the time of "Update".
    let mut rig = Rig::new(Options {
        applet_now: Some(Ok(true)),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    assert!(matches!(
        rig.install().await,
        UpdateState::Restarting { .. }
    ));
    let calls = rig.installer.calls.lock().unwrap().clone();
    assert_eq!(calls[0].len(), 2, "{calls:?}");

    // The question failed.
    let mut rig = Rig::new(Options {
        install: Install {
            kind: InstallKind::Deb,
            applet: true,
        },
        applet_now: Some(Err("dpkg-query failed (exit code 2)".into())),
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    rig.check(false).await;
    let state = rig.install().await;
    let (at, message) = failed(&state).unwrap();
    assert_eq!(at, UpdateStage::Install);
    assert!(
        message.contains("could not tell whether the COSMIC applet package"),
        "{message}"
    );
    assert_eq!(rig.server.paths().len(), 2, "nothing downloaded");
    assert!(rig.installer.calls.lock().unwrap().is_empty());
}

/// A build that is not a release never asks anyone and says why.
#[tokio::test]
async fn a_build_that_is_not_a_release_has_no_update_check() {
    let rig = Rig::new(Options {
        running: None,
        ..Options::default()
    });
    rig.server.release(SUMS_A, SIG_A);
    let status = rig.updater.check(true).await;
    assert_eq!(status.state, UpdateState::Idle);
    assert_eq!(
        status.unavailable.as_deref(),
        Some(playdrop_core::update::NOT_A_RELEASE)
    );
    assert_eq!(status.running_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(rig.server.connections(), 0);
    assert!(rig.updater.start_install(Relaunch::Window).is_err());
    // A release build carries no such sentence.
    let rig = Rig::new(Options::default());
    assert_eq!(rig.updater.status().unavailable, None);
}

#[test]
fn the_core_urls_all_start_with_the_releases_base() {
    use playdrop_core::update::{DOWNLOAD_BASE, RELEASE_PAGE_BASE, SIG_URL, SUMS_URL};
    assert_eq!(
        SUMS_URL,
        format!("{}/latest/download/SHA256SUMS", super::RELEASES)
    );
    assert_eq!(
        SIG_URL,
        format!("{}/latest/download/SHA256SUMS.sig", super::RELEASES)
    );
    assert_eq!(DOWNLOAD_BASE, format!("{}/download", super::RELEASES));
    assert_eq!(RELEASE_PAGE_BASE, format!("{}/tag", super::RELEASES));
}
