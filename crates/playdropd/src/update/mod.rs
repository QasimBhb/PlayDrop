//! The update check and the update itself, desktop side (`docs/design/update-check.md`,
//! SR-45 to SR-48, `docs/ipc.md` 1.9).
//!
//! Every decision about what GitHub answered lives in `playdrop_core::update` (signature,
//! version, file name, the once-a-day rule, the redirect rule); this module only asks,
//! remembers and acts:
//!
//! - [`fetch`]: the anonymous HTTPS client, two GETs per check, redirects by hand.
//! - [`kind`]: how this copy was installed (`.deb`, Windows installer, Flatpak, other).
//! - [`store`]: `update.json`, the times, "Later" and the last signed answer.
//! - [`install`]: `pkexec apt-get` or the Windows installer, behind a trait for tests.
//! - [`restart`]: putting the new daemon in place after a `.deb` install.
//! - `hooks` (debug and test builds only): `PLAYDROP_UPDATE_BASE` and
//!   `PLAYDROP_UPDATE_TEST_KEY`.
//!
//! [`Updater`] owns the state machine. A check runs only when a UI asks
//! ([`Updater::check`]); nothing here starts one from a timer, at start or at login. With
//! the setting off no request is made at all.

pub mod fetch;
#[cfg(any(debug_assertions, test))]
mod hooks;
/// Release builds: no test hooks at all, so neither the code that reads them nor the
/// variable names are in the program (`hooks.rs`).
#[cfg(not(any(debug_assertions, test)))]
mod hooks {
    /// Always empty.
    #[derive(Default)]
    pub struct Overrides {
        pub base: Option<String>,
        pub test_origin: Option<String>,
        pub extra_key: Option<[u8; 32]>,
    }

    /// Reads nothing.
    pub fn read(_env: impl Fn(&str) -> Option<String>) -> Overrides {
        Overrides::default()
    }
}
pub mod install;
pub mod kind;
pub mod restart;
pub mod store;
#[cfg(test)]
mod tests;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use playdrop_core::update::{
    self as core_update, Asset, InstallKind, SignedSums, UpdateError, Version, MAX_SIG_LEN,
    MAX_SUMS_LEN, NOT_A_RELEASE, REQUEST_TIMEOUT_MS, TRUSTED_KEYS,
};
use playdrop_ipc::{UpdateOffer, UpdateStage, UpdateState, UpdateStatus};
use sha2::{Digest as _, Sha256};

use self::fetch::{FetchConfig, FetchError, Fetcher, RedirectRule};
use self::install::{InstallFailure, Installer, Relaunch};
use self::kind::Install;
use self::store::Remembered;

/// Where every release lives; the core's URLs all start with it.
pub const RELEASES: &str = "https://github.com/QasimBhb/PlayDrop/releases";

/// The note under an offer a Flatpak cannot install.
pub const FLATPAK_REASON: &str =
    "Flatpak updates come from where it was installed (a software centre or `flatpak update`).";
/// The note under an offer an unpackaged copy cannot install.
pub const OTHER_REASON: &str =
    "This copy was not installed from a PlayDrop package, so it cannot update itself; get the new version from the release page.";
/// The note when the release lacks this computer's file.
const NO_FILE_REASON: &str =
    "This release has no file for this computer; get it from the release page.";
/// The note when the applet is installed but the release has no applet package: installing
/// the main package alone would make the package manager remove the applet.
const NO_APPLET_REASON: &str =
    "This release has no COSMIC applet package, and updating without it would remove the applet; get the release from its page.";
/// The failure when the install step cannot tell whether the applet is installed.
const APPLET_UNKNOWN: &str =
    "PlayDrop could not tell whether the COSMIC applet is installed, and updating without it could remove it, so nothing was changed. Try again in a moment.";
/// The answer to "Cancel" once the files are with the system installer.
const CANNOT_CANCEL: &str =
    "The update is already with the system installer and cannot be cancelled from PlayDrop.";

/// Download progress is announced at most once per this many bytes.
const PROGRESS_STEP: u64 = 256 * 1024;

/// Current Unix time in milliseconds.
pub fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

/// How the updater is set up: where it keeps things, whom it asks, what it trusts.
pub struct Setup {
    /// The daemon's data dir, for `update.json`.
    pub data_dir: PathBuf,
    /// Downloads go to fresh folders inside this one (mode 0700).
    pub downloads_dir: PathBuf,
    /// The version running now, or `None` for a build that is not a plain release: such a
    /// build has no update check and makes no request ([`Version::current`]).
    pub running: Option<Version>,
    /// How this copy was installed; asked once, when first needed (it may run `dpkg`). Only
    /// what the offer says depends on it.
    pub install: Arc<dyn Fn() -> Install + Send + Sync>,
    /// Whether the COSMIC applet's package is installed, asked again by the install step
    /// itself, right before the download: the answer decides whether `apt-get` gets the
    /// applet's package too, so it must be current, and a question that fails stops the
    /// update rather than reading as "no" ([`kind::applet_installed_now`]).
    pub applet_installed: Arc<dyn Fn() -> Result<bool, String> + Send + Sync>,
    /// Replaces [`RELEASES`] in every URL (test hooks only; [`RELEASES`] otherwise).
    pub releases_base: String,
    /// Keys a signature may come from: [`TRUSTED_KEYS`], plus a test key in debug builds.
    pub keys: Vec<[u8; 32]>,
    /// The HTTP client's settings.
    pub fetch: FetchConfig,
    /// The install step.
    pub installer: Arc<dyn Installer>,
    /// The clock, Unix milliseconds.
    pub now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
}

impl Setup {
    /// The real thing: GitHub, the pinned keys, the system's installer and clock. In debug
    /// builds the test hooks may change the base URL and add a key.
    pub fn system(data_dir: &Path) -> Self {
        let hooks = hooks::read(|name| std::env::var(name).ok());
        let mut keys = TRUSTED_KEYS.to_vec();
        keys.extend(hooks.extra_key);
        let releases_base = hooks.base.unwrap_or_else(|| RELEASES.to_owned());
        let redirects = RedirectRule {
            test_origin: hooks.test_origin,
        };
        let https_only = redirects.test_origin.is_none();
        Self {
            data_dir: data_dir.to_owned(),
            downloads_dir: default_downloads_dir(data_dir),
            running: Version::current(),
            install: Arc::new(|| kind::decide(&kind::SystemProbe)),
            applet_installed: Arc::new(kind::applet_installed_now),
            releases_base,
            keys,
            fetch: FetchConfig {
                timeout: Duration::from_millis(REQUEST_TIMEOUT_MS),
                download_deadline: fetch::DOWNLOAD_DEADLINE,
                min_progress: fetch::MIN_PROGRESS,
                redirects,
                proxy: fetch::proxy_from_env(|name| std::env::var(name).ok()),
                https_only,
            },
            installer: Arc::new(install::SystemInstaller),
            now_ms: Arc::new(unix_ms),
        }
    }
}

/// A setup for daemon tests that never reaches a network: the base is a closed port on
/// loopback, no key verifies anything, and the installer refuses.
#[cfg(test)]
pub fn test_setup(data_dir: &Path) -> Setup {
    struct Refuse;
    impl Installer for Refuse {
        fn install_debs(&self, _: &[PathBuf]) -> Result<(), InstallFailure> {
            Err(InstallFailure("tests install nothing".into()))
        }
        fn run_windows_installer(&self, _: &Path, _: Relaunch) -> Result<(), InstallFailure> {
            Err(InstallFailure("tests install nothing".into()))
        }
    }
    Setup {
        data_dir: data_dir.to_owned(),
        downloads_dir: data_dir.join("updates"),
        // A plain version, so the daemon's tests see the check whatever the workspace
        // version is.
        running: Some(Version {
            major: 0,
            minor: 0,
            patch: 1,
        }),
        install: Arc::new(|| Install {
            kind: InstallKind::Other,
            applet: false,
        }),
        applet_installed: Arc::new(|| Ok(false)),
        releases_base: "http://127.0.0.1:1/QasimBhb/PlayDrop/releases".to_owned(),
        keys: Vec::new(),
        fetch: FetchConfig {
            timeout: Duration::from_millis(500),
            download_deadline: fetch::DOWNLOAD_DEADLINE,
            min_progress: fetch::MIN_PROGRESS,
            redirects: RedirectRule {
                test_origin: Some("http://127.0.0.1:1".to_owned()),
            },
            proxy: fetch::ProxyChoice::Direct,
            https_only: false,
        },
        installer: Arc::new(Refuse),
        now_ms: Arc::new(unix_ms),
    }
}

/// `%LOCALAPPDATA%\PlayDrop\updates` on Windows (where the installer runs from); the data
/// dir's `updates` folder elsewhere.
///
/// On Windows this is beside the data dir, not inside it: the data dir has a protected
/// owner-only DACL (SR-28), and an installer elevated over the shoulder (an administrator's
/// password typed for a standard user) runs as that administrator, who must be able to
/// read the file. Folders here keep the ACL they inherit from `%LOCALAPPDATA%` (the user,
/// `SYSTEM` and Administrators) and nothing is added, so nobody wider than that can read
/// them.
fn default_downloads_dir(data_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        if let Some(base) = directories::BaseDirs::new() {
            return base.data_local_dir().join("PlayDrop").join("updates");
        }
    }
    data_dir.join("updates")
}

/// A request the updater turns down, worded for the user (the daemon answers it as
/// `invalid_state`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Refused(pub String);

/// A verified offer: the release, its files, and whether this copy can install it.
#[derive(Debug, Clone)]
struct Pending {
    offer: UpdateOffer,
    asset: Option<Asset>,
    /// The release's applet package, for a `.deb` install whether or not the applet looked
    /// installed when the offer was made: the install step asks again and takes it then.
    applet: Option<Asset>,
}

/// The update the user started and has not ended yet.
struct Run {
    /// Tells the run's state changes from a later run's.
    id: u64,
    /// Set by "Cancel"; the download stops at its next read.
    cancel: Arc<AtomicBool>,
}

struct Inner {
    state: UpdateState,
    remembered: Remembered,
    pending: Option<Pending>,
    /// Why the last check failed, while it is newer than the last answer.
    last_failure: Option<String>,
    /// The running update, if any. Its state changes apply only while it is this one, so
    /// a cancelled download that is still winding down cannot write over what came after.
    run: Option<Run>,
}

/// The update state machine, one per daemon.
pub struct Updater {
    setup: Setup,
    fetcher: Arc<Fetcher>,
    enabled: AtomicBool,
    install: OnceLock<Install>,
    inner: Mutex<Inner>,
    /// One check at a time; a second caller waits and then sees the first one's answer.
    checking: tokio::sync::Mutex<()>,
    emit: Box<dyn Fn(UpdateStatus) + Send + Sync>,
    /// Runs once a `.deb` update is installed and `Restarting` was announced.
    on_installed: OnceLock<Box<dyn Fn() + Send + Sync>>,
    /// The next [`Run::id`].
    next_run: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Updater {
    /// Builds the updater from what `update.json` remembers. Touches no network. `emit`
    /// receives every state change; `enabled` is the `update_check` setting.
    pub fn new(
        setup: Setup,
        enabled: bool,
        emit: Box<dyn Fn(UpdateStatus) + Send + Sync>,
    ) -> Arc<Self> {
        let remembered = store::load(&setup.data_dir);
        // Files from an update that never finished (the daemon stopped mid-download, or
        // the Windows installer ran from them) are not needed by anyone now.
        remove_downloads(&setup.downloads_dir);
        let fetcher = Arc::new(Fetcher::new(setup.fetch.clone()));
        let updater = Arc::new(Self {
            setup,
            fetcher,
            enabled: AtomicBool::new(enabled),
            install: OnceLock::new(),
            inner: Mutex::new(Inner {
                state: UpdateState::Idle,
                remembered,
                pending: None,
                last_failure: None,
                run: None,
            }),
            checking: tokio::sync::Mutex::new(()),
            emit,
            on_installed: OnceLock::new(),
            next_run: AtomicU64::new(1),
        });
        updater.restore();
        updater
    }

    /// What to run once a `.deb` update is installed (the daemon's restart). Set once.
    pub fn set_on_installed(&self, f: Box<dyn Fn() + Send + Sync>) {
        let _ = self.on_installed.set(f);
    }

    /// The state for a UI, without contacting anyone.
    pub fn status(&self) -> UpdateStatus {
        let inner = lock(&self.inner);
        self.status_of(&inner)
    }

    /// The `update_check` setting changed. Turning it off hides any offer at once; a
    /// download already running finishes, since the user asked for it.
    pub fn set_enabled(&self, on: bool) {
        if self.enabled.swap(on, Ordering::SeqCst) != on {
            self.announce();
        }
    }

    /// A UI opened (`force: false`) or "Check now" (`force: true`). Answers once the check
    /// is over, or at once when none is due, the setting is off, an update runs, or this
    /// build is not a release (then nothing is ever asked).
    pub async fn check(self: &Arc<Self>, force: bool) -> UpdateStatus {
        if !self.enabled.load(Ordering::SeqCst) || self.setup.running.is_none() {
            return self.status();
        }
        let _one = self.checking.lock().await;
        let now = (self.setup.now_ms)();
        {
            // Tested and set under one lock, so an "Update" pressed in between cannot
            // start a download that this check then writes over.
            let mut inner = lock(&self.inner);
            if in_progress(&inner.state) {
                return self.status_of(&inner);
            }
            let r = &inner.remembered;
            if !force && !core_update::check_due(r.last_answer_ms, r.last_failure_ms, now) {
                return self.status_of(&inner);
            }
            inner.state = UpdateState::Checking;
        }
        self.announce();
        tracing::info!(url = %self.sums_url(), force, "checking for a newer PlayDrop");
        let me = Arc::clone(self);
        let (sums_url, sig_url) = (self.sums_url(), self.sig_url());
        let answer = tokio::task::spawn_blocking(move || {
            // Asked here, off the async threads: the first time it may run `dpkg-query`.
            me.install();
            ask(&me.fetcher, &sums_url, &sig_url, &me.setup.keys)
        })
        .await
        .unwrap_or_else(|e| Err(CheckFailure::Read(e.to_string())));
        self.checked(answer, now);
        self.status()
    }

    /// "Later": hides the offer until the next check that runs.
    pub fn later(&self) -> UpdateStatus {
        let now = (self.setup.now_ms)();
        let changed = {
            let mut inner = lock(&self.inner);
            let offered = match &inner.state {
                UpdateState::Available { offer }
                | UpdateState::Failed {
                    offer: Some(offer), ..
                } => Some(offer.version.clone()),
                _ => None,
            };
            match offered {
                Some(version) => {
                    tracing::info!(version, "update put off until the next check");
                    inner.remembered.later_version = Some(version);
                    inner.remembered.later_at_ms = Some(now);
                    inner.state = UpdateState::Idle;
                    self.save(&inner.remembered);
                    true
                }
                None => false,
            }
        };
        if changed {
            self.announce();
        }
        self.status()
    }

    /// "Update": starts the download and install of the offered release in the
    /// background and answers with the first progress state. `relaunch` is what the
    /// Windows installer starts again afterwards.
    pub fn start_install(self: &Arc<Self>, relaunch: Relaunch) -> Result<UpdateStatus, Refused> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Err(Refused(
                "Update checks are off in Settings; turn them on first.".to_owned(),
            ));
        }
        if self.setup.running.is_none() {
            return Err(Refused(NOT_A_RELEASE.to_owned()));
        }
        let cancel = Arc::new(AtomicBool::new(false));
        let id = self.next_run.fetch_add(1, Ordering::SeqCst);
        let pending = {
            let mut inner = lock(&self.inner);
            if in_progress(&inner.state) {
                return Err(Refused("An update is already running.".to_owned()));
            }
            let offered = matches!(
                inner.state,
                UpdateState::Available { .. } | UpdateState::Failed { offer: Some(_), .. }
            );
            let pending = match (&inner.pending, offered) {
                (Some(p), true) => p.clone(),
                _ => return Err(Refused("No update is offered right now.".to_owned())),
            };
            if !pending.offer.can_install {
                return Err(Refused(
                    pending
                        .offer
                        .reason
                        .clone()
                        .unwrap_or_else(|| OTHER_REASON.to_owned()),
                ));
            }
            inner.state = UpdateState::Downloading {
                version: pending.offer.version.clone(),
                done: 0,
                total: 0,
            };
            inner.run = Some(Run {
                id,
                cancel: Arc::clone(&cancel),
            });
            pending
        };
        self.announce();
        tracing::info!(version = %pending.offer.version, "update started by the user");
        let me = Arc::clone(self);
        tokio::spawn(async move { me.run_install(id, cancel, pending, relaunch).await });
        Ok(self.status())
    }

    /// "Cancel" during the download: stops it, offers the release again at once, and the
    /// download removes its files as it stops. An install already handed to the system
    /// (its password prompt, or the Windows installer) cannot be stopped from here.
    pub fn cancel(&self) -> Result<UpdateStatus, Refused> {
        {
            let mut inner = lock(&self.inner);
            match inner.state {
                UpdateState::Downloading { .. } | UpdateState::Verifying { .. } => {}
                UpdateState::Installing { .. } | UpdateState::Restarting { .. } => {
                    return Err(Refused(CANNOT_CANCEL.to_owned()));
                }
                _ => return Err(Refused("No update is downloading.".to_owned())),
            }
            if let Some(run) = inner.run.take() {
                run.cancel.store(true, Ordering::SeqCst);
            }
            inner.state = match &inner.pending {
                Some(p) => UpdateState::Available {
                    offer: p.offer.clone(),
                },
                None => UpdateState::Idle,
            };
        }
        tracing::info!("update download cancelled by the user");
        self.announce();
        Ok(self.status())
    }

    /// The offered release's page, for "What is new".
    pub fn page_url(&self) -> Option<String> {
        let inner = lock(&self.inner);
        inner.pending.as_ref().map(|p| p.offer.page_url.clone())
    }

    fn install(&self) -> Install {
        *self.install.get_or_init(|| {
            let found = (self.setup.install)();
            tracing::info!(kind = ?found.kind, applet = found.applet, "install kind");
            found
        })
    }

    fn sums_url(&self) -> String {
        self.rebase(core_update::SUMS_URL)
    }

    fn sig_url(&self) -> String {
        self.rebase(core_update::SIG_URL)
    }

    /// Points a release URL at the configured base (only ever different in test builds).
    fn rebase(&self, url: &str) -> String {
        url.strip_prefix(RELEASES).map_or_else(
            || url.to_owned(),
            |rest| format!("{}{rest}", self.setup.releases_base),
        )
    }

    /// Picks up the last signed answer from `update.json`, verifying it again, so an offer
    /// survives a restart without a new request. "Later" keeps it hidden.
    fn restore(&self) {
        let mut guard = lock(&self.inner);
        let inner = &mut *guard;
        let (Some(sums), Some(sig)) = (&inner.remembered.sums, &inner.remembered.sig) else {
            return;
        };
        let Ok(signed) =
            core_update::verify_sums_with(sums.as_bytes(), sig.as_bytes(), &self.setup.keys)
        else {
            return;
        };
        let pending = self.pending_for(&signed);
        let hidden = pending
            .as_ref()
            .is_some_and(|p| inner.remembered.later_version.as_deref() == Some(&*p.offer.version));
        inner.state = match &pending {
            Some(p) if !hidden => UpdateState::Available {
                offer: p.offer.clone(),
            },
            _ => UpdateState::Idle,
        };
        inner.pending = pending;
    }

    /// What this copy is offered by a verified checksum file, if anything.
    fn pending_for(&self, signed: &SignedSums) -> Option<Pending> {
        let install = self.install();
        let found = signed.offer(self.setup.running?, install.kind)?;
        let applet = if install.kind == InstallKind::Deb {
            signed.applet_asset()
        } else {
            None
        };
        let reason = match install.kind {
            InstallKind::Flatpak => Some(FLATPAK_REASON),
            InstallKind::Deb | InstallKind::Windows if found.asset.is_none() => {
                Some(NO_FILE_REASON)
            }
            InstallKind::Deb if install.applet && applet.is_none() => Some(NO_APPLET_REASON),
            InstallKind::Deb | InstallKind::Windows => None,
            InstallKind::Android | InstallKind::Ios | InstallKind::Other => Some(OTHER_REASON),
        };
        let rebased = |a: Asset| Asset {
            url: self.rebase(&a.url),
            ..a
        };
        Some(Pending {
            offer: UpdateOffer {
                version: found.version.to_string(),
                page_url: self.rebase(&found.page_url),
                can_install: reason.is_none(),
                reason: reason.map(str::to_owned),
            },
            asset: found.asset.map(rebased),
            applet: applet.map(rebased),
        })
    }

    /// Records a check's outcome: the times, the answer, and what to show.
    fn checked(&self, answer: Result<(SignedSums, String, String), CheckFailure>, now: u64) {
        {
            let mut guard = lock(&self.inner);
            let inner = &mut *guard;
            match answer {
                Ok((signed, sums, sig)) => {
                    let r = &mut inner.remembered;
                    r.last_answer_ms = Some(now);
                    r.last_failure_ms = None;
                    // The check ran, so "Later" has had its day.
                    r.later_version = None;
                    r.later_at_ms = None;
                    r.sums = Some(sums);
                    r.sig = Some(sig);
                    inner.last_failure = None;
                    if in_progress(&inner.state) {
                        // An update started meanwhile owns the state and its offer; the
                        // answer is remembered and shown once it ends.
                        self.save(&inner.remembered);
                        return;
                    }
                    let pending = self.pending_for(&signed);
                    tracing::info!(
                        latest = %signed.version(),
                        offered = pending.is_some(),
                        "update check answered"
                    );
                    inner.state = match &pending {
                        Some(p) => UpdateState::Available {
                            offer: p.offer.clone(),
                        },
                        None => UpdateState::UpToDate,
                    };
                    inner.pending = pending;
                }
                Err(failure) => {
                    tracing::info!(reason = %failure, "update check failed");
                    inner.remembered.last_failure_ms = Some(now);
                    inner.last_failure = Some(failure.sentence());
                    if in_progress(&inner.state) {
                        self.save(&inner.remembered);
                        return;
                    }
                    // An offer verified earlier stays valid; the failure shows in Settings.
                    let hidden = inner.pending.as_ref().is_some_and(|p| {
                        inner.remembered.later_version.as_deref() == Some(&*p.offer.version)
                    });
                    inner.state = match &inner.pending {
                        Some(p) if !hidden => UpdateState::Available {
                            offer: p.offer.clone(),
                        },
                        _ => UpdateState::Failed {
                            stage: UpdateStage::Check,
                            message: failure.sentence(),
                            offer: None,
                        },
                    };
                }
            }
            self.save(&inner.remembered);
        }
        self.announce();
    }

    /// Downloads, verifies and installs `pending` as run `id`. Every failure removes what
    /// was downloaded and leaves the running version alone.
    async fn run_install(
        self: Arc<Self>,
        id: u64,
        cancel: Arc<AtomicBool>,
        pending: Pending,
        relaunch: Relaunch,
    ) {
        let version = pending.offer.version.clone();
        let kind = self.install().kind;
        let Some(files) = self.files_to_fetch(id, kind, &pending).await else {
            return;
        };
        let dir = match self.fresh_download_dir(&version, id) {
            Ok(dir) => dir,
            Err(e) => {
                tracing::warn!(error = %e, "no folder for the download");
                self.fail(
                    id,
                    UpdateStage::Download,
                    format!("The update could not be saved on this computer: {e}"),
                    &pending,
                    None,
                );
                return;
            }
        };
        let me = Arc::clone(&self);
        let fetched = {
            let dir = dir.clone();
            let files = files.clone();
            let version = version.clone();
            tokio::task::spawn_blocking(move || {
                me.download_all(id, &cancel, &dir, &files, &version)
            })
            .await
            .unwrap_or_else(|e| Err(DownloadFailure::Fetch(FetchError::Other(e.to_string()))))
        };
        let paths = match fetched {
            Ok(paths) => paths,
            Err(DownloadFailure::Fetch(FetchError::Cancelled)) => {
                // "Cancel" already put the offer back on screen.
                remove_dir(&dir);
                return;
            }
            Err(failure) => {
                tracing::info!(reason = ?failure, "update download failed");
                let (stage, message) = failure.stage_and_sentence();
                self.fail(id, stage, message, &pending, Some(&dir));
                return;
            }
        };
        // From here on the update cannot be cancelled: the system prompt comes next.
        if !self.run_state(
            id,
            UpdateState::Installing {
                version: version.clone(),
            },
        ) {
            remove_dir(&dir);
            return;
        }
        let installer = Arc::clone(&self.setup.installer);
        let outcome = {
            let paths = paths.clone();
            let dir = dir.clone();
            tokio::task::spawn_blocking(move || {
                // Right before the files are handed over: make them read-only and hash
                // them once more from an open descriptor (design note section 6).
                seal_and_recheck(&dir, &paths, &files)?;
                match kind {
                    InstallKind::Windows => match paths.first() {
                        Some(file) => installer.run_windows_installer(file, relaunch),
                        None => Err(InstallFailure("Nothing was downloaded.".to_owned())),
                    },
                    _ => installer.install_debs(&paths),
                }
            })
            .await
            .unwrap_or_else(|e| Err(InstallFailure(e.to_string())))
        };
        self.finish_install(id, kind, &pending, &dir, outcome);
    }

    /// Ends run `id` with the install step's `outcome`.
    fn finish_install(
        &self,
        id: u64,
        kind: InstallKind,
        pending: &Pending,
        dir: &Path,
        outcome: Result<(), InstallFailure>,
    ) {
        let version = pending.offer.version.clone();
        match outcome {
            Err(InstallFailure(message)) => {
                self.fail(id, UpdateStage::Install, message, pending, Some(dir));
            }
            // A Windows installer that replaced PlayDrop stopped this daemon before it
            // ended, so one that ended while this daemon runs did not replace it.
            Ok(()) if kind == InstallKind::Windows => {
                self.fail(
                    id,
                    UpdateStage::Install,
                    install::inno_exit_sentence(0),
                    pending,
                    Some(dir),
                );
            }
            Ok(()) => {
                remove_dir(dir);
                {
                    let mut inner = lock(&self.inner);
                    inner.pending = None;
                    inner.run = None;
                    inner.state = UpdateState::Restarting {
                        version: version.clone(),
                    };
                }
                self.announce();
                tracing::info!(version, "update installed; starting the new daemon");
                if let Some(restart) = self.on_installed.get() {
                    restart();
                }
            }
        }
    }

    /// The files run `id` downloads: the installer's, and for a `.deb` the applet's too
    /// when the applet is installed now (asked again here, not taken from the offer, and a
    /// failed question stops the update). `None` when the run ended here.
    async fn files_to_fetch(
        &self,
        id: u64,
        kind: InstallKind,
        pending: &Pending,
    ) -> Option<Vec<Asset>> {
        let mut files: Vec<Asset> = pending.asset.iter().cloned().collect();
        if kind != InstallKind::Deb {
            return Some(files);
        }
        let probe = Arc::clone(&self.setup.applet_installed);
        let applet = tokio::task::spawn_blocking(move || probe())
            .await
            .unwrap_or_else(|e| Err(e.to_string()));
        match applet {
            Ok(false) => Some(files),
            Ok(true) => {
                if let Some(asset) = &pending.applet {
                    files.push(asset.clone());
                    Some(files)
                } else {
                    self.fail(
                        id,
                        UpdateStage::Install,
                        NO_APPLET_REASON.to_owned(),
                        pending,
                        None,
                    );
                    None
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "could not tell whether the applet is installed");
                self.fail(
                    id,
                    UpdateStage::Install,
                    APPLET_UNKNOWN.to_owned(),
                    pending,
                    None,
                );
                None
            }
        }
    }

    /// Fetches each file into `dir`, hashing as it streams, then checks every hash
    /// against its signed line. Blocks. The whole download shares one deadline.
    fn download_all(
        &self,
        id: u64,
        cancel: &AtomicBool,
        dir: &Path,
        files: &[Asset],
        version: &str,
    ) -> Result<Vec<PathBuf>, DownloadFailure> {
        let deadline = Instant::now() + self.setup.fetch.download_deadline;
        let mut paths = Vec::with_capacity(files.len());
        let mut digests = Vec::with_capacity(files.len());
        let mut before = 0u64;
        for asset in files {
            tracing::info!(url = %asset.url, "downloading update file");
            // `name` passed the core's file-name rule: letters, digits, `.`, `-`, `_`,
            // never a path or an option.
            let path = dir.join(&asset.name);
            let mut out = std::io::BufWriter::new(private_file(&path).map_err(|e| {
                DownloadFailure::Fetch(FetchError::Other(format!(
                    "could not save the download: {e}"
                )))
            })?);
            let mut last = 0u64;
            let mut progress = |done: u64, total: Option<u64>| {
                if done == 0 || done - last >= PROGRESS_STEP || Some(done) == total {
                    last = done;
                    self.run_state(
                        id,
                        UpdateState::Downloading {
                            version: version.to_owned(),
                            done: before + done,
                            total: before + total.unwrap_or(0),
                        },
                    );
                }
            };
            let got = self
                .fetcher
                .download(&asset.url, &mut out, &mut progress, deadline, cancel)
                .map_err(DownloadFailure::Fetch)?;
            drop(out);
            before += got.len;
            paths.push(path);
            digests.push(got.sha256);
        }
        self.run_state(
            id,
            UpdateState::Verifying {
                version: version.to_owned(),
            },
        );
        for (asset, digest) in files.iter().zip(&digests) {
            if !asset.matches(digest) {
                tracing::warn!(file = %asset.name, "download does not match its signed checksum");
                return Err(DownloadFailure::Mismatch);
            }
        }
        Ok(paths)
    }

    /// A new, empty, private folder for run `id`'s download.
    fn fresh_download_dir(&self, version: &str, id: u64) -> std::io::Result<PathBuf> {
        private_dir_all(&self.setup.downloads_dir)?;
        let dir = self
            .setup
            .downloads_dir
            .join(format!("{version}-{}-{id}", (self.setup.now_ms)()));
        private_dir(&dir)?;
        Ok(dir)
    }

    /// Ends run `id` as failed: removes `dir`, keeps the offer so "Update" can be tried
    /// again, and says what happened. A run that was cancelled only cleans up.
    fn fail(
        &self,
        id: u64,
        stage: UpdateStage,
        message: String,
        pending: &Pending,
        dir: Option<&Path>,
    ) {
        if let Some(dir) = dir {
            remove_dir(dir);
        }
        let current = {
            let mut inner = lock(&self.inner);
            let current = inner.run.as_ref().is_some_and(|r| r.id == id);
            if current {
                inner.run = None;
                inner.state = UpdateState::Failed {
                    stage,
                    message,
                    offer: Some(pending.offer.clone()),
                };
            }
            current
        };
        if current {
            tracing::info!(stage = ?stage, "update failed; the current version keeps running");
            self.announce();
        }
    }

    /// Sets `state` for run `id` if that run is still the current one; `false` when it
    /// was cancelled (or replaced), in which case nothing changes.
    fn run_state(&self, id: u64, state: UpdateState) -> bool {
        {
            let mut inner = lock(&self.inner);
            if inner.run.as_ref().is_none_or(|r| r.id != id) {
                return false;
            }
            inner.state = state;
        }
        self.announce();
        true
    }

    fn announce(&self) {
        (self.emit)(self.status());
    }

    fn status_of(&self, inner: &Inner) -> UpdateStatus {
        let r = &inner.remembered;
        // An answer clears the failure time, so one that is set is newer than the answer.
        let failure_is_news = r.last_failure_ms.is_some();
        let enabled = self.enabled.load(Ordering::SeqCst);
        let state = if enabled || in_progress(&inner.state) {
            inner.state.clone()
        } else {
            UpdateState::Off
        };
        UpdateStatus {
            state,
            last_checked_ms: r.last_answer_ms,
            last_failed_ms: r.last_failure_ms.filter(|_| failure_is_news),
            last_failure: inner.last_failure.clone().filter(|_| failure_is_news),
            running_version: self
                .setup
                .running
                .map_or_else(|| env!("CARGO_PKG_VERSION").to_owned(), |v| v.to_string()),
            unavailable: self
                .setup
                .running
                .is_none()
                .then(|| NOT_A_RELEASE.to_owned()),
        }
    }

    fn save(&self, remembered: &Remembered) {
        if let Err(e) = store::save(&self.setup.data_dir, remembered) {
            tracing::warn!(error = %e, "update.json not saved");
        }
    }
}

/// States in which an update the user started is running.
fn in_progress(state: &UpdateState) -> bool {
    matches!(
        state,
        UpdateState::Downloading { .. }
            | UpdateState::Verifying { .. }
            | UpdateState::Installing { .. }
            | UpdateState::Restarting { .. }
    )
}

/// Why a check gave nothing to act on.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
enum CheckFailure {
    #[error("request failed: {0}")]
    Fetch(FetchError),
    #[error("refused: {0}")]
    Verify(UpdateError),
    #[error("{0}")]
    Read(String),
}

impl CheckFailure {
    /// One plain sentence for Settings.
    fn sentence(&self) -> String {
        match self {
            Self::Fetch(FetchError::TooLarge) => {
                "GitHub's answer was larger than allowed, so it was not used.".to_owned()
            }
            Self::Fetch(FetchError::Status(404)) => {
                "The latest release has no signed update information.".to_owned()
            }
            Self::Fetch(e) => fetch_sentence(e),
            Self::Verify(UpdateError::NotSigned | UpdateError::BadSignatureEncoding) => {
                "The update information is not signed by PlayDrop, so nothing is offered."
                    .to_owned()
            }
            Self::Verify(_) => {
                "The update information could not be read, so nothing is offered.".to_owned()
            }
            Self::Read(e) => format!("The check did not finish: {e}"),
        }
    }
}

/// Why a download ended without an install.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DownloadFailure {
    Fetch(FetchError),
    Mismatch,
}

impl DownloadFailure {
    fn stage_and_sentence(&self) -> (UpdateStage, String) {
        match self {
            Self::Mismatch => (
                UpdateStage::Verify,
                "The download did not match its signed checksum and was deleted.".to_owned(),
            ),
            Self::Fetch(FetchError::TooLarge) => (
                UpdateStage::Download,
                "The download was larger than allowed and was deleted.".to_owned(),
            ),
            Self::Fetch(FetchError::WrongLength) => (
                UpdateStage::Download,
                "The download was not the length the server promised and was deleted.".to_owned(),
            ),
            Self::Fetch(FetchError::TooSlow) => (
                UpdateStage::Download,
                "The download was too slow and was stopped. Try again later.".to_owned(),
            ),
            Self::Fetch(e) => (UpdateStage::Download, fetch_sentence(e)),
        }
    }
}

/// The sentence shared by both stages for network trouble.
fn fetch_sentence(e: &FetchError) -> String {
    match e {
        FetchError::Timeout => "GitHub did not answer in time. Try again later.".to_owned(),
        FetchError::Connect(_) => {
            "Could not reach GitHub. Check the internet connection and try again.".to_owned()
        }
        FetchError::RedirectRefused => {
            "GitHub sent the request to a place PlayDrop does not trust, so it stopped.".to_owned()
        }
        FetchError::TooManyRedirects => {
            "GitHub redirected the request too many times, so PlayDrop stopped.".to_owned()
        }
        FetchError::TooLarge => "The answer was larger than allowed.".to_owned(),
        FetchError::WrongLength => "The answer was cut short.".to_owned(),
        FetchError::TooSlow => "GitHub answered too slowly. Try again later.".to_owned(),
        FetchError::Cancelled => "Cancelled.".to_owned(),
        FetchError::Status(code) => format!("GitHub answered with an error ({code})."),
        FetchError::Other(e) => format!("The answer could not be read: {e}"),
    }
}

/// The two GETs of a check, then the signature. Blocks. Returns the verified file and the
/// texts to remember.
fn ask(
    fetcher: &Fetcher,
    sums_url: &str,
    sig_url: &str,
    keys: &[[u8; 32]],
) -> Result<(SignedSums, String, String), CheckFailure> {
    let sums = fetcher
        .get_small(sums_url, MAX_SUMS_LEN)
        .map_err(CheckFailure::Fetch)?;
    let sig = fetcher
        .get_small(sig_url, MAX_SIG_LEN)
        .map_err(CheckFailure::Fetch)?;
    let signed = core_update::verify_sums_with(&sums, &sig, keys).map_err(CheckFailure::Verify)?;
    // A file that verified parsed as UTF-8 text, and a signature is base64.
    let sums = String::from_utf8(sums).map_err(|e| CheckFailure::Read(e.to_string()))?;
    let sig = String::from_utf8(sig).map_err(|e| CheckFailure::Read(e.to_string()))?;
    Ok((signed, sums, sig))
}

/// Makes the downloaded files read-only (0400, folder 0500 on Unix) and hashes each once
/// more from an open descriptor, right before they go to the system installer. The first
/// hash was taken while streaming; this one narrows the gap in which another program of
/// the same user could swap a file by path (design note section 6 says what remains).
fn seal_and_recheck(dir: &Path, paths: &[PathBuf], files: &[Asset]) -> Result<(), InstallFailure> {
    let changed = || {
        InstallFailure(
            "The downloaded update changed on disk before it could be installed, so it was deleted; nothing was changed."
                .to_owned(),
        )
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let seal = |path: &Path, mode: u32| {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        };
        for path in paths {
            seal(path, 0o400).map_err(|_| changed())?;
        }
        seal(dir, 0o500).map_err(|_| changed())?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    for (path, asset) in paths.iter().zip(files) {
        let mut file = std::fs::File::open(path).map_err(|_| changed())?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf).map_err(|_| changed())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        if !asset.matches(hasher.finalize().as_slice()) {
            tracing::warn!(file = %asset.name, "update file changed after it was verified");
            return Err(changed());
        }
    }
    Ok(())
}

fn remove_dir(dir: &Path) {
    // A sealed folder (0500) must be writable again before its files can be removed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    match std::fs::remove_dir_all(dir) {
        Ok(()) => tracing::debug!(dir = %dir.display(), "update files removed"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(dir = %dir.display(), error = %e, "update files not removed"),
    }
}

/// Removes every leftover download folder inside `downloads_dir`.
fn remove_downloads(downloads_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(downloads_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            remove_dir(&path);
        } else if let Err(e) = std::fs::remove_file(&path) {
            tracing::debug!(error = %e, "leftover update file not removed");
        }
    }
}

#[cfg(unix)]
fn private_dir_all(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn private_dir_all(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Creates `dir`, which must not exist yet, mode 0700.
#[cfg(unix)]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new().mode(0o700).create(dir)
}

#[cfg(not(unix))]
fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir(dir)
}

/// Creates a new file, mode 0600; refuses one that exists.
#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
}
