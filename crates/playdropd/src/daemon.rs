//! The daemon: `Host` lifecycle, event fan-out, request handling, and the glue to the
//! tray, notifications, clipboard, lock state and network watcher.
//!
//! Start-up order (`run`): settings, lock state, the IPC server (owning the bus name or
//! pipe is the single-instance check, so a second daemon says "already running" instead
//! of failing on the port), then the host, autostart files, clipboard watcher, network
//! watcher and tray. Shutdown, from a `Shutdown` request, the tray, SIGTERM or Ctrl-C:
//! `DaemonEvent::ShuttingDown`, `Host::shutdown`, IPC server, tray.
//!
//! This computer's state for peers: the lock watcher also sets presence (`locked` or
//! `active`, protocol.md 5.10) and a poller reads the battery (`battery.rs`, 5.8); both are
//! applied again to a restarted host.
//!
//! Events: one task per host instance drains the core's `HostEvent` channel without pause
//! (the core drops events when its 4,096 buffer fills) and republishes each as
//! `Event::Host` on a broadcast channel. A broadcast send never waits, so a slow UI only
//! lags itself (it misses events and rebuilds from `Status`, `Peers` and `Transfers`).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock, Weak};
use std::time::Duration;

use anyhow::Context as _;
use futures_util::future::BoxFuture;
use playdrop_core::host::{
    Host, HostConfig, HostEvent, LockFlag, LockState as _, PeerSummary, SendSource,
};
use playdrop_core::identity::DeviceId;
use playdrop_core::session::types::PresenceState;
use playdrop_core::Error as CoreError;
use playdrop_ipc::server::{ServeError, Server};
use playdrop_ipc::{
    ClipSkip, DaemonEvent, DiagnosticsReport, ErrorCode, Event, IpcError, Ok as IpcOk, Request,
    RequestHandler, Response, Status, IPC_VERSION,
};
use tokio::sync::{broadcast, mpsc, watch, Notify};
use tokio::task::JoinHandle;

use crate::autostart::{self, Locations};
use crate::battery;
use crate::cli::Args;
use crate::clipboard::{self, ClipContent, LocalClip};
use crate::control::{self, InputRunners};
use crate::input::InputSupport;
use crate::media::{self, MediaSupport};
use crate::notify::Notifier;
use crate::open::OpenError;
use crate::settings::{self, Settings};
use crate::tray::{self, Tray, TrayCommand, TrayDevice, TrayView};
use crate::{lock, network, probe};

/// Events buffered per UI client before it starts missing them.
pub const EVENT_CAPACITY: usize = 1024;

/// How long a stopped host's event drain may take to hand over its last events.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Local clips buffered between the watcher and the push.
const CLIP_QUEUE: usize = 16;

/// The message for SR-6 refusals (`docs/ipc.md` section 5).
const LOCKED_MESSAGE: &str = "desktop is locked";

/// Runs the daemon until it is asked to stop.
#[allow(clippy::too_many_lines, reason = "start-up is one linear sequence")]
pub async fn run(args: Args) -> anyhow::Result<()> {
    let (data_dir, persisted, effective, first_run) = load_settings(&args)?;

    let support = clipboard::support();
    if !support.supported {
        tracing::info!(
            backend = support.backend,
            reason = support.reason.as_deref().unwrap_or("unsupported"),
            "clipboard watching is not available; the window offers a send button instead"
        );
    }
    let (lock_flag, lock_rx) = lock::start().await?;
    let input_support = crate::input::support().await;
    let media_support = media::support().await;
    control::log_support(&input_support, &media_support);
    let notifier = Notifier::new(effective.notifications, effective.receive_dir.clone());
    let locations = match Locations::user() {
        Ok(_) if args.no_autostart => {
            tracing::info!("--no-autostart: autostart and D-Bus activation files left alone");
            None
        }
        Ok(l) => Some(l),
        Err(e) => {
            tracing::warn!(error = %e, "autostart files cannot be managed");
            None
        }
    };
    let daemon = Daemon::new(Options {
        data_dir,
        persisted,
        effective: effective.clone(),
        bind_ip: IpAddr::V6(Ipv6Addr::UNSPECIFIED),
        lock: lock_flag,
        notifier,
        autostart: locations.clone(),
        clipboard_supported: support.supported,
        watch_clipboard: true,
        input_support,
        media_support,
    });

    let server = match Server::start(daemon.handler(), daemon.events()).await {
        Ok(s) => Some(s),
        Err(ServeError::AlreadyRunning) => {
            println!("PlayDrop daemon is already running");
            tracing::info!("another PlayDrop daemon is already running; exiting");
            return Ok(());
        }
        Err(e) => {
            tracing::error!(error = %e, "local IPC is unavailable: the window and other UIs cannot connect");
            None
        }
    };
    if let Err(e) = daemon.start_host().await {
        if let Some(s) = server {
            s.shutdown().await;
        }
        return Err(e);
    }

    if let Some(loc) = locations {
        let _ = tokio::task::spawn_blocking(move || {
            log_sync("D-Bus service file", autostart::sync_dbus_service(&loc));
        })
        .await;
        if first_run {
            daemon.shared.apply_autostart(effective.autostart).await;
        }
    }
    daemon.shared.sync_clipboard_watcher().await;
    daemon.shared.start_media_watcher().await;

    let (net_tx, net_rx) = mpsc::channel(4);
    network::watch(net_tx).await;
    let battery_task = spawn_battery_poll(Arc::downgrade(&daemon.shared));

    let (tray_tx, tray_rx) = mpsc::unbounded_channel();
    let tray = if args.no_tray {
        None
    } else {
        Tray::start(tray_tx, daemon.shared.tray_view())
            .await
            .map(Arc::new)
    };
    let tray_task = tray
        .as_ref()
        .map(|t| spawn_tray_refresh(Arc::clone(t), Arc::downgrade(&daemon.shared)));

    event_loop(&daemon.shared, lock_rx, net_rx, tray_rx).await;

    battery_task.abort();
    daemon.shutdown().await;
    if let Some(s) = server {
        s.shutdown().await;
    }
    if let Some(task) = tray_task {
        task.abort();
    }
    if let Some(t) = tray {
        if let Ok(t) = Arc::try_unwrap(t) {
            t.shutdown().await;
        }
    }
    tracing::info!("playdropd stopped");
    Ok(())
}

/// Reads the settings, applies `--write-settings`, records the first run, and returns
/// `(data_dir, persisted, effective, first_run)`.
fn load_settings(args: &Args) -> anyhow::Result<(PathBuf, Settings, Settings, bool)> {
    let data_dir = args
        .data_dir
        .clone()
        .or_else(settings::default_data_dir)
        .context("no data folder could be found; pass --data-dir")?;
    let loaded = settings::load(&data_dir)?;
    let first_run = !loaded.existed;
    let mut persisted = loaded.settings;
    if args.write_settings {
        persisted.apply_args(args);
    }
    if args.write_settings || first_run {
        settings::save(&data_dir, &persisted)?;
    }
    let mut effective = persisted.clone();
    effective.apply_args(args);
    Ok((data_dir, persisted, effective, first_run))
}

/// How often the tray is rebuilt without a change, so "seen 5 min ago" stays true.
const TRAY_TICK: Duration = Duration::from_mins(1);

/// Pushes a fresh view to the tray whenever something it shows changed, and once a
/// minute for the last-seen times.
fn spawn_tray_refresh(tray: Arc<Tray>, shared: Weak<Shared>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let Some(s) = shared.upgrade() else { return };
            let dirty = Arc::clone(&s.tray_dirty);
            let view = s.tray_view();
            drop(s);
            tray.update(view).await;
            let _ = tokio::time::timeout(TRAY_TICK, dirty.notified()).await;
        }
    })
}

/// Reads this computer's battery every [`battery::POLL`] and hands changes to the host. Ends
/// at once on a computer without a battery: nothing is ever sent for it.
fn spawn_battery_poll(shared: Weak<Shared>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut last: Option<(battery::Reading, tokio::time::Instant)> = None;
        loop {
            let reading = tokio::task::spawn_blocking(battery::read)
                .await
                .ok()
                .flatten();
            match reading {
                Some(r) => {
                    let now = tokio::time::Instant::now();
                    if battery::should_push(last, r, now) {
                        let Some(s) = shared.upgrade() else { return };
                        s.set_local_battery(r);
                        last = Some((r, now));
                    }
                }
                None if last.is_none() => {
                    tracing::info!("no battery found; this computer reports none to peers");
                    return;
                }
                None => {}
            }
            tokio::time::sleep(battery::POLL).await;
        }
    })
}

/// Waits for a stop request while forwarding lock, network and tray events.
async fn event_loop(
    shared: &Arc<Shared>,
    mut lock_rx: watch::Receiver<bool>,
    mut net_rx: mpsc::Receiver<()>,
    mut tray_rx: mpsc::UnboundedReceiver<TrayCommand>,
) {
    let mut shutdown_rx = shared.shutdown_tx.subscribe();
    let mut lock_open = true;
    let mut net_open = true;
    loop {
        tokio::select! {
            () = wait_signal() => {
                tracing::info!("signal received; shutting down");
                return;
            }
            r = shutdown_rx.changed() => {
                if r.is_err() || *shutdown_rx.borrow() {
                    return;
                }
            }
            r = lock_rx.changed(), if lock_open => {
                if r.is_err() {
                    lock_open = false;
                    continue;
                }
                let locked = *lock_rx.borrow_and_update();
                shared.lock_changed(locked);
            }
            r = net_rx.recv(), if net_open => {
                match r {
                    Some(()) => shared.network_changed(),
                    None => net_open = false,
                }
            }
            Some(cmd) = tray_rx.recv() => shared.tray_command(cmd),
        }
    }
}

fn log_sync(what: &str, r: anyhow::Result<autostart::Outcome>) {
    match r {
        Ok(o) => tracing::debug!(?o, "{what} synced"),
        Err(e) => tracing::warn!(error = %e, "{what} not synced"),
    }
}

/// Ctrl-C everywhere, SIGTERM on Unix.
async fn wait_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = term.recv() => {}
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler not installed");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// How to build a [`Daemon`].
pub struct Options {
    /// Identity, pins and settings directory.
    pub data_dir: PathBuf,
    /// Settings as on disk.
    pub persisted: Settings,
    /// Settings with this run's flags applied.
    pub effective: Settings,
    /// Listening address (`::` in production, loopback in tests).
    pub bind_ip: IpAddr,
    /// Lock state the core consults (SR-6).
    pub lock: Arc<LockFlag>,
    /// Notifications.
    pub notifier: Arc<Notifier>,
    /// Where autostart files go; `None` leaves them alone.
    pub autostart: Option<Locations>,
    /// The clipboard backend can watch.
    pub clipboard_supported: bool,
    /// Start the clipboard watcher at all (false in tests).
    pub watch_clipboard: bool,
    /// What the input backend said at start (`accepts_input`, the window's toggle).
    pub input_support: InputSupport,
    /// What the media backend said at start.
    pub media_support: MediaSupport,
}

/// The daemon state shared by the event loop and the request handler.
pub struct Daemon {
    shared: Arc<Shared>,
}

/// Run-time settings: on disk, and with this run's flags.
struct SettingsState {
    persisted: Settings,
    effective: Settings,
}

struct Shared {
    data_dir: PathBuf,
    bind_ip: IpAddr,
    lock: Arc<LockFlag>,
    notifier: Arc<Notifier>,
    autostart: Option<Locations>,
    clipboard_supported: bool,
    watch_clipboard: bool,
    settings: Mutex<SettingsState>,
    host: RwLock<Option<Arc<Host>>>,
    drain: Mutex<Option<JoinHandle<()>>>,
    /// Serialises host start, restart and shutdown.
    lifecycle: tokio::sync::Mutex<()>,
    events: broadcast::Sender<Event>,
    clip_tx: mpsc::Sender<LocalClip>,
    watcher: tokio::sync::Mutex<Option<clipboard::Watcher>>,
    clipboard_paused: AtomicBool,
    /// The `clipboard_images` setting, read by the watcher thread at each change.
    clipboard_images: Arc<AtomicBool>,
    /// Peers' battery levels, for the tray.
    battery: Mutex<HashMap<String, u8>>,
    /// This computer's last battery reading, applied again to a restarted host.
    local_battery: Mutex<Option<battery::Reading>>,
    tray_dirty: Arc<Notify>,
    shutdown_tx: watch::Sender<bool>,
    input_support: InputSupport,
    media_support: MediaSupport,
    /// One injector task per controlling peer (Phase 5, SR-25).
    input_runners: Arc<InputRunners>,
    /// The local player watcher, when the platform has one.
    media: tokio::sync::Mutex<Option<media::Watcher>>,
    /// Why start at login is not in effect (the Background portal refused or failed), for
    /// `Diagnose`. Cleared by the next successful request.
    autostart_problem: Mutex<Option<String>>,
    /// Counts "Start at login" requests. A late answer or deferred write belongs to the
    /// request it captured and is dropped when a newer request has started since.
    autostart_attempt: AtomicU64,
}

impl Daemon {
    /// Builds the daemon without starting the host. Must run inside a tokio runtime.
    pub fn new(opts: Options) -> Self {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (clip_tx, clip_rx) = mpsc::channel(CLIP_QUEUE);
        let images = opts.effective.clipboard_images;
        let notifier = opts.notifier;
        let tray_dirty = Arc::new(Notify::new());
        let input_runners = InputRunners::new(
            opts.data_dir.clone(),
            Arc::clone(&notifier),
            events.clone(),
            Arc::clone(&tray_dirty),
        );
        let shared = Arc::new(Shared {
            data_dir: opts.data_dir,
            bind_ip: opts.bind_ip,
            lock: opts.lock,
            notifier,
            autostart: opts.autostart,
            clipboard_supported: opts.clipboard_supported,
            watch_clipboard: opts.watch_clipboard,
            settings: Mutex::new(SettingsState {
                persisted: opts.persisted,
                effective: opts.effective,
            }),
            host: RwLock::new(None),
            drain: Mutex::new(None),
            lifecycle: tokio::sync::Mutex::new(()),
            events,
            clip_tx,
            watcher: tokio::sync::Mutex::new(None),
            clipboard_paused: AtomicBool::new(false),
            clipboard_images: Arc::new(AtomicBool::new(images)),
            battery: Mutex::new(HashMap::new()),
            local_battery: Mutex::new(None),
            tray_dirty,
            shutdown_tx: watch::Sender::new(false),
            input_support: opts.input_support,
            media_support: opts.media_support,
            input_runners,
            media: tokio::sync::Mutex::new(None),
            autostart_problem: Mutex::new(None),
            autostart_attempt: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&shared);
        shared.input_runners.set_ender(Box::new(move |_peer: &str| {
            if let Some(h) = weak.upgrade().and_then(|s| s.host()) {
                // Ends every session, not just the peer's: a backend failure is global.
                h.stop_input_sessions();
            }
        }));
        tokio::spawn(local_clips(Arc::downgrade(&shared), clip_rx));
        Self { shared }
    }

    /// The handler the IPC server calls.
    pub fn handler(&self) -> Arc<dyn RequestHandler> {
        Arc::new(Handler(Arc::clone(&self.shared)))
    }

    /// The broadcast channel every UI client listens on.
    pub fn events(&self) -> broadcast::Sender<Event> {
        self.shared.events.clone()
    }

    /// Starts the host with the effective settings. A held port gets the SR-31 message.
    pub async fn start_host(&self) -> anyhow::Result<()> {
        let _g = self.shared.lifecycle.lock().await;
        let s = self.shared.effective();
        self.shared.start_with(&s).await.map_err(|e| match e {
            CoreError::PortHeld { port } => anyhow::anyhow!(
                "port {port} is already in use by another program (or another PlayDrop daemon)"
            ),
            e => anyhow::Error::new(e).context("the PlayDrop host did not start"),
        })
    }

    /// Becomes true when something asked the daemon to stop.
    #[cfg(test)]
    pub fn shutdown_requested(&self) -> watch::Receiver<bool> {
        self.shared.shutdown_tx.subscribe()
    }

    /// Announces the shutdown, stops the clipboard watcher and the host.
    pub async fn shutdown(&self) {
        let s = &self.shared;
        let _g = s.lifecycle.lock().await;
        let _ = s.events.send(Event::Daemon(DaemonEvent::ShuttingDown));
        if let Some(w) = s.watcher.lock().await.take() {
            w.stop();
        }
        if let Some(m) = s.media.lock().await.take() {
            m.stop();
        }
        s.input_runners.stop_all();
        s.stop_host().await;
    }
}

/// Feeds clips from the watcher to the host. Holds only a weak reference so the daemon
/// can be dropped.
async fn local_clips(shared: Weak<Shared>, mut rx: mpsc::Receiver<LocalClip>) {
    while let Some(clip) = rx.recv().await {
        let Some(s) = shared.upgrade() else { return };
        s.push_local_clip(clip);
    }
}

/// Drains one host's events: side effects first, then the broadcast to UIs.
async fn drain(shared: Weak<Shared>, mut rx: mpsc::Receiver<HostEvent>) {
    while let Some(ev) = rx.recv().await {
        let Some(s) = shared.upgrade() else { return };
        s.on_host_event(&ev);
        let _ = s.events.send(Event::Host(for_clients(ev)));
    }
}

/// Trims an event before it goes to UI clients. The daemon has already applied a clipboard
/// image to the system clipboard; the bytes themselves would exceed the IPC event cap
/// (4 MiB) and no UI renders them, so clients get the notice with an empty payload.
fn for_clients(ev: HostEvent) -> HostEvent {
    match ev {
        HostEvent::ClipboardImage {
            peer,
            mime,
            sensitive,
            ..
        } => HostEvent::ClipboardImage {
            peer,
            mime,
            data: playdrop_core::host::Secret(playdrop_core::host::Base64Bytes::from(Vec::new())),
            sensitive,
        },
        other => other,
    }
}

fn lock_std<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// First 8 characters of a hex Device ID, the only form logs carry (SR-32).
fn short_hex(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

impl Shared {
    fn effective(&self) -> Settings {
        lock_std(&self.settings).effective.clone()
    }

    fn host(&self) -> Option<Arc<Host>> {
        self.host
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn emit(&self, ev: DaemonEvent) {
        let _ = self.events.send(Event::Daemon(ev));
    }

    fn host_config(&self, s: &Settings) -> HostConfig {
        let mut c = HostConfig::new(&self.data_dir, &s.receive_dir, &s.device_name);
        c.port = s.port;
        c.bind_ip = self.bind_ip;
        c.discovery = s.discovery;
        c.lock_state = Box::new((*self.lock).clone());
        c.accepts_input = self.input_support.supported;
        c
    }

    /// Puts the "Start at login" setting into effect: the XDG entry or the Run key, or
    /// inside Flatpak a Background portal request whose answer arrives later
    /// ([`Shared::autostart_answered`]).
    async fn apply_autostart(self: &Arc<Self>, on: bool) {
        let attempt = self.autostart_attempt.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(loc) = self.autostart.clone() else {
            return;
        };
        if loc.flatpak {
            #[cfg(target_os = "linux")]
            {
                let weak = Arc::downgrade(self);
                tokio::spawn(async move {
                    let answer = autostart::background::request(on).await;
                    if let Some(s) = weak.upgrade() {
                        s.autostart_answered(attempt, on, answer).await;
                    }
                });
            }
            return;
        }
        let result = tokio::task::spawn_blocking(move || {
            let r = autostart::sync_autostart(&loc, on);
            match &r {
                Ok(o) => tracing::debug!(?o, "autostart entry synced"),
                Err(e) => tracing::warn!(error = %e, "autostart entry not synced"),
            }
            r.map(|_| ())
        })
        .await;
        if let (true, Ok(Err(e))) = (on, result) {
            // `SetSettings` calls this while holding the lifecycle lock that
            // `turn_autostart_off` takes, so the write waits for it in its own task.
            let shared = Arc::clone(self);
            tokio::spawn(async move {
                shared
                    .turn_autostart_off(
                        attempt,
                        Some(format!("Start at login could not be set up: {e:#}")),
                    )
                    .await;
            });
        }
    }

    /// The Background portal's answer. A refusal turns the setting back off, so the switch
    /// does not claim something the desktop will not do, and says so in a notification
    /// and in `Diagnose` (anti-pattern 18).
    #[cfg(target_os = "linux")]
    async fn autostart_answered(
        &self,
        attempt: u64,
        on: bool,
        answer: anyhow::Result<autostart::background::Answer>,
    ) {
        use autostart::background::Answer;
        let problem = match answer {
            Ok(Answer::Allowed) => {
                tracing::info!(on, "start at login set through the Background portal");
                self.set_autostart_refused(attempt, None).await;
                None
            }
            Ok(Answer::Denied(reason)) if on => {
                tracing::warn!(reason, "start at login was not allowed");
                self.notifier.info(
                    "Start at login was not allowed",
                    "Allow PlayDrop to run in the background in the system settings, then turn Start at login on again.",
                );
                self.turn_autostart_off(attempt, Some(
                    "Start at login was not allowed. Allow PlayDrop to run in the background in the system settings, then turn it on again."
                        .to_owned(),
                ))
                .await;
                Some(format!("start at login was not allowed: {reason}"))
            }
            Ok(Answer::Denied(reason)) => {
                tracing::warn!(reason, "the login entry could not be removed");
                Some(format!(
                    "start at login could not be turned off: {reason}; remove PlayDrop from the startup apps in the system settings"
                ))
            }
            Err(e) => {
                tracing::warn!(error = %e, "the Background portal is unavailable");
                self.notifier
                    .info("Start at login could not be set up", e.to_string());
                if on {
                    self.turn_autostart_off(
                        attempt,
                        Some(format!("Start at login could not be set up: {e}")),
                    )
                    .await;
                }
                Some(format!("start at login could not be set up: {e}"))
            }
        };
        *lock_std(&self.autostart_problem) = problem;
    }

    /// Records "Start at login" as off with the reason it could not be on, on disk and
    /// for every UI, without asking the portal again.
    ///
    /// Does nothing when a newer request has started since `attempt` (its answer is
    /// stale) or when the setting is already off.
    async fn turn_autostart_off(&self, attempt: u64, refused: Option<String>) {
        let _g = self.lifecycle.lock().await;
        let persisted = {
            let mut st = lock_std(&self.settings);
            if self.autostart_attempt.load(Ordering::SeqCst) != attempt || !st.persisted.autostart {
                return;
            }
            st.effective.autostart = false;
            st.persisted.autostart = false;
            st.effective.autostart_refused.clone_from(&refused);
            st.persisted.autostart_refused = refused;
            st.persisted.clone()
        };
        let dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || settings::save(&dir, &persisted)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "settings not saved"),
            Err(e) => tracing::warn!(error = %e, "settings not saved"),
        }
        self.emit(DaemonEvent::SettingsChanged {
            settings: self.ipc_settings(),
        });
    }

    /// Sets or clears the start-at-login refusal reason, saving it and telling every UI
    /// only when it changed.
    #[cfg(target_os = "linux")]
    async fn set_autostart_refused(&self, attempt: u64, refused: Option<String>) {
        let _g = self.lifecycle.lock().await;
        let persisted = {
            let mut st = lock_std(&self.settings);
            if self.autostart_attempt.load(Ordering::SeqCst) != attempt
                || st.effective.autostart_refused == refused
            {
                return;
            }
            st.effective.autostart_refused.clone_from(&refused);
            st.persisted.autostart_refused = refused;
            st.persisted.clone()
        };
        let dir = self.data_dir.clone();
        match tokio::task::spawn_blocking(move || settings::save(&dir, &persisted)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "settings not saved"),
            Err(e) => tracing::warn!(error = %e, "settings not saved"),
        }
        self.emit(DaemonEvent::SettingsChanged {
            settings: self.ipc_settings(),
        });
    }

    /// Lines `Diagnose` adds after the network findings: a start-at-login problem, and
    /// that KDE Connect running alongside is fine.
    fn diagnose_notes(&self, port: u16) -> Vec<String> {
        let mut notes: Vec<String> = lock_std(&self.autostart_problem).iter().cloned().collect();
        notes.extend(probe::coexistence_note(port));
        notes
    }

    /// The settings view a UI reads, with what the platform backends can do.
    fn ipc_settings(&self) -> playdrop_ipc::Settings {
        let mut s = self.effective().to_ipc(self.clipboard_supported);
        s.remote_input_supported = self.input_support.supported;
        s.remote_input_reason.clone_from(&self.input_support.reason);
        s
    }

    /// Starts the media watcher when the platform has one; its states reach whichever
    /// host is running (a restart keeps the watcher).
    async fn start_media_watcher(self: &Arc<Self>) {
        if !self.media_support.supported {
            return;
        }
        let (tx, rx) = mpsc::channel(64);
        match media::Watcher::start(tx).await {
            Ok(w) => {
                *self.media.lock().await = Some(w);
                let host = control::weak_host(&Arc::downgrade(self), Shared::host);
                tokio::spawn(control::media_feed(rx, host));
            }
            Err(e) => tracing::warn!(error = %e, "media watcher did not start"),
        }
    }

    /// Starts a host and its drain. Caller holds `lifecycle`.
    async fn start_with(self: &Arc<Self>, s: &Settings) -> Result<(), CoreError> {
        let (host, rx) = Host::start(self.host_config(s)).await?;
        // A new host knows nothing of this computer yet: presence and battery again.
        host.set_presence(presence_for(self.lock.is_locked()), None, None);
        if let Some(r) = *lock_std(&self.local_battery) {
            host.set_battery(r.level, r.charging, r.low);
        }
        let task = tokio::spawn(drain(Arc::downgrade(self), rx));
        *self.host.write().unwrap_or_else(PoisonError::into_inner) = Some(Arc::new(host));
        *lock_std(&self.drain) = Some(task);
        self.tray_dirty.notify_one();
        Ok(())
    }

    /// Stops the host and lets its drain hand over the last events. Caller holds
    /// `lifecycle`.
    async fn stop_host(&self) {
        let host = self
            .host
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(h) = host {
            h.shutdown().await;
        }
        let task = lock_std(&self.drain).take();
        if let Some(mut t) = task {
            if tokio::time::timeout(DRAIN_GRACE, &mut t).await.is_err() {
                t.abort();
            }
        }
    }

    /// Restarts the host with `new`; on failure goes back to `old`. Caller holds
    /// `lifecycle`.
    async fn restart(self: &Arc<Self>, old: &Settings, new: &Settings) -> Result<(), IpcError> {
        self.stop_host().await;
        match self.start_with(new).await {
            Ok(()) => {
                self.emit(DaemonEvent::HostRestarted);
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, "host did not start with the new settings; restoring the old ones");
                match self.start_with(old).await {
                    Ok(()) => self.emit(DaemonEvent::HostRestarted),
                    Err(e2) => {
                        tracing::error!(error = %e2, "host did not start again with the old settings");
                    }
                }
                Err(internal(format!(
                    "the new settings could not be applied: {e}"
                )))
            }
        }
    }

    fn label_for(&self, peer: &str) -> String {
        self.host()
            .and_then(|h| h.peers().into_iter().find(|p| p.device_id == peer))
            .map_or_else(|| short_hex(peer).to_owned(), |p| p.label)
    }

    /// Daemon-side reactions to a host event. Never blocks: slow work is spawned.
    fn on_host_event(self: &Arc<Self>, ev: &HostEvent) {
        self.notifier.host_event(ev, || match ev {
            HostEvent::Paired { name, .. } => name.clone(),
            HostEvent::TransferComplete { peer, .. }
            | HostEvent::TransferFailed { peer, .. }
            | HostEvent::Ping { peer, .. }
            | HostEvent::Find { peer, .. }
            | HostEvent::RemovedByPeer { peer, .. } => self.label_for(peer),
            _ => String::new(),
        });
        match ev {
            HostEvent::Battery { peer, level, .. } => {
                lock_std(&self.battery).insert(peer.clone(), *level);
                self.tray_dirty.notify_one();
            }
            HostEvent::Unpaired { peer, .. } => {
                lock_std(&self.battery).remove(peer);
                self.tray_dirty.notify_one();
            }
            HostEvent::PeerOnline { .. }
            | HostEvent::PeerOffline { .. }
            | HostEvent::Paired { .. }
            | HostEvent::RemovedByPeer { .. } => self.tray_dirty.notify_one(),
            HostEvent::Input { peer, input } => {
                // The core already checked grant, session, lock and rate (SR-25); the
                // runner keeps the frames in order and owns the injector's life.
                self.input_runners
                    .on_input(peer, self.label_for(peer), input.clone());
            }
            HostEvent::MediaCommand {
                peer,
                player,
                action,
                value,
            } => {
                let me = Arc::clone(self);
                let (peer, player, action, value) =
                    (peer.clone(), player.clone(), action.clone(), *value);
                tokio::spawn(async move {
                    let guard = me.media.lock().await;
                    let Some(w) = guard.as_ref() else {
                        tracing::debug!(
                            peer = short_hex(&peer),
                            "media command with no media backend"
                        );
                        return;
                    };
                    if let Err(e) = w.command(&player, action, value).await {
                        tracing::info!(peer = short_hex(&peer), error = %e, "media command not applied");
                    }
                });
            }
            HostEvent::Clipboard {
                peer,
                text,
                sensitive,
            } => {
                if let Some(t) = text.0.clone() {
                    self.apply_clip(peer.clone(), ClipContent::Text(t), *sensitive);
                }
            }
            HostEvent::ClipboardImage {
                peer,
                mime,
                data,
                sensitive,
            } => {
                if self.clipboard_images.load(Ordering::SeqCst) {
                    let content = ClipContent::Image {
                        mime: mime.clone(),
                        bytes: data.0 .0.clone(),
                    };
                    self.apply_clip(peer.clone(), content, *sensitive);
                } else {
                    tracing::debug!(
                        peer = short_hex(peer),
                        "clipboard image received while images are off; not applied"
                    );
                }
            }
            _ => {}
        }
    }

    /// Writes a received clip to the system clipboard off the event loop (SR-24: the
    /// backend disables history and cloud sync for sensitive clips). The content is never
    /// logged or shown.
    fn apply_clip(self: &Arc<Self>, peer: String, content: ClipContent, sensitive: bool) {
        let me = Arc::clone(self);
        tokio::spawn(async move {
            let r = tokio::task::spawn_blocking(move || match &content {
                ClipContent::Text(text) => clipboard::apply(text, sensitive),
                ClipContent::Image { mime, bytes } => {
                    clipboard::apply_image(bytes, mime, sensitive)
                }
            })
            .await;
            match r {
                Ok(Ok(())) => me.emit(DaemonEvent::ClipboardApplied { peer, sensitive }),
                Ok(Err(e)) => {
                    tracing::warn!(peer = short_hex(&peer), error = %e, "received clip not applied");
                    me.notifier.info(
                        format!(
                            "Clipboard from {} could not be applied",
                            me.label_for(&peer)
                        ),
                        e.to_string(),
                    );
                }
                Err(e) => tracing::warn!(error = %e, "clipboard apply task failed"),
            }
        });
    }

    /// A clip from the watcher: pushed unless sync is off or paused, or it is an image and
    /// images are off.
    fn push_local_clip(&self, clip: LocalClip) {
        if self.clipboard_paused.load(Ordering::SeqCst) || !self.effective().clipboard_sync {
            return;
        }
        let is_image = matches!(clip.content, ClipContent::Image { .. });
        if is_image && !self.clipboard_images.load(Ordering::SeqCst) {
            return;
        }
        let Some(host) = self.host() else { return };
        match push(&host, clip) {
            Ok(p) => {
                if !p.sent_to.is_empty() {
                    self.emit(DaemonEvent::ClipboardPushed {
                        sent_to: p.sent_to.iter().map(DeviceId::to_hex).collect(),
                    });
                }
            }
            Err(e) => tracing::debug!(error = %e, "local clip not pushed"),
        }
    }

    /// Records this computer's battery and hands it to the host (protocol.md 5.8).
    fn set_local_battery(&self, r: battery::Reading) {
        tracing::debug!(
            level = r.level,
            charging = r.charging,
            low = r.low,
            "battery reading"
        );
        *lock_std(&self.local_battery) = Some(r);
        if let Some(h) = self.host() {
            h.set_battery(r.level, r.charging, r.low);
        }
    }

    /// Starts or stops the watcher to match the settings.
    async fn sync_clipboard_watcher(&self) {
        let want =
            self.watch_clipboard && self.clipboard_supported && self.effective().clipboard_sync;
        let mut w = self.watcher.lock().await;
        if want && w.is_none() {
            let images = Arc::clone(&self.clipboard_images);
            match clipboard::Watcher::start(self.clip_tx.clone(), images).await {
                Ok(x) => *w = Some(x),
                Err(e) => tracing::warn!(error = %e, "clipboard watcher did not start"),
            }
        } else if !want {
            if let Some(x) = w.take() {
                x.stop();
            }
        }
        drop(w);
        self.tray_dirty.notify_one();
    }

    fn tray_view(&self) -> TrayView {
        let battery = lock_std(&self.battery).clone();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let devices = self
            .host()
            .map(|h| h.peers())
            .unwrap_or_default()
            .into_iter()
            .map(|p| TrayDevice {
                battery: battery.get(&p.device_id).copied(),
                seen_secs_ago: (!p.online)
                    .then_some(p.last_seen)
                    .flatten()
                    .map(|t| now.saturating_sub(t)),
                label: p.label,
                online: p.online,
            })
            .collect();
        TrayView {
            devices,
            clipboard_paused: self.clipboard_paused.load(Ordering::SeqCst),
            clipboard_sync: self.clipboard_supported && self.effective().clipboard_sync,
            input_from: self.input_runners.active_labels(),
        }
    }

    fn lock_changed(&self, locked: bool) {
        tracing::debug!(locked, "desktop lock state");
        if let Some(h) = self.host() {
            if locked {
                h.close_add_device_window();
                // SR-25: no input into a locked desktop. The core drops frames on its own;
                // ending the sessions releases every held key and button now (T31).
                let ended = h.stop_input_sessions();
                if !ended.is_empty() {
                    tracing::info!(
                        count = ended.len(),
                        "desktop locked; remote input sessions ended"
                    );
                }
            }
            h.set_presence(presence_for(locked), None, None);
        }
        self.emit(DaemonEvent::LockChanged { locked });
    }

    fn network_changed(&self) {
        if let Some(h) = self.host() {
            h.network_changed();
        }
    }

    fn tray_command(self: &Arc<Self>, cmd: TrayCommand) {
        match cmd {
            TrayCommand::AddDevice => {
                if let Some(h) = self.host() {
                    match h.open_add_device_window() {
                        Ok(()) => {}
                        Err(CoreError::Locked) => {
                            self.notifier.info("Unlock the desktop to add a device", "");
                            return;
                        }
                        Err(e) => tracing::warn!(error = %e, "add-device window not opened"),
                    }
                }
                self.open_window(Some("add-device"));
            }
            TrayCommand::OpenWindow => self.open_window(None),
            TrayCommand::SendClipboard => {
                let me = Arc::clone(self);
                tokio::spawn(async move { me.send_clipboard_now().await });
            }
            TrayCommand::SetClipboardPaused(p) => {
                self.clipboard_paused.store(p, Ordering::SeqCst);
                self.tray_dirty.notify_one();
            }
            TrayCommand::StopInput => {
                if let Some(h) = self.host() {
                    h.stop_input_sessions();
                }
            }
            TrayCommand::Quit => {
                self.shutdown_tx.send_replace(true);
            }
        }
    }

    fn open_window(&self, page: Option<&str>) {
        if let Err(e) = tray::launch_window(page) {
            tracing::warn!(error = %e, "window not started");
            self.notifier
                .info("The PlayDrop window could not be started", e.to_string());
        }
    }

    /// Tray "Send clipboard". SR-24 asks that a manual send of a password-manager clip
    /// asks first; a tray has no dialog, so the clip goes out marked sensitive (the phone
    /// keeps it out of history) and a notification says so. *Choice*, documented for the
    /// window: the window's own send button is where a real prompt belongs.
    async fn send_clipboard_now(&self) {
        let images = self.clipboard_images.load(Ordering::SeqCst);
        let read = tokio::task::spawn_blocking(move || clipboard::read_current(images)).await;
        let clip = match read {
            Ok(Ok(Some(c))) => c,
            Ok(Ok(None)) => {
                self.notifier.info("The clipboard is empty", "");
                return;
            }
            Ok(Err(e)) => {
                self.notifier
                    .info("Could not read the clipboard", e.to_string());
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "clipboard read task failed");
                return;
            }
        };
        let Some(host) = self.host() else { return };
        let sensitive = clip.sensitive;
        match push(&host, clip) {
            Ok(p) if !p.sent_to.is_empty() => {
                self.emit(DaemonEvent::ClipboardPushed {
                    sent_to: p.sent_to.iter().map(DeviceId::to_hex).collect(),
                });
                if sensitive {
                    self.notifier.info(
                        "Clip from a password manager was sent as sensitive",
                        "The receiving device keeps it out of its clipboard history.",
                    );
                }
            }
            Ok(p) => match clip_skip(&p) {
                Some(ClipSkip::Echo) => self.notifier.info("Clipboard already sent", ""),
                Some(skip) => self.notifier.info("Clipboard not sent", skip_notice(skip)),
                None => {}
            },
            Err(e) => self.notifier.info("Clipboard not sent", e.to_string()),
        }
    }

    fn status(&self, host: &Host) -> Status {
        Status {
            device_id: host.device_id().to_hex(),
            short_id: host.short_id(),
            addr: host.local_addr(),
            daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
            ipc_version: IPC_VERSION,
            settings: self.ipc_settings(),
            add_device_window_open: host.add_device_window_open(),
            locked: self.lock.is_locked(),
            input_from: self.input_runners.active_labels(),
        }
    }

    async fn set_settings(
        self: &Arc<Self>,
        patch: &playdrop_ipc::SettingsPatch,
    ) -> Result<IpcOk, IpcError> {
        let _g = self.lifecycle.lock().await;
        let (old, mut new, mut persisted) = {
            let st = lock_std(&self.settings);
            (
                st.effective.clone(),
                st.effective.clone(),
                st.persisted.clone(),
            )
        };
        let changes = new
            .apply_patch(patch)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        persisted
            .apply_patch(patch)
            .map_err(|e| err(ErrorCode::BadRequest, e.to_string()))?;
        if changes.restart_host {
            self.restart(&old, &new).await?;
        }
        let saved = {
            let dir = self.data_dir.clone();
            let p = persisted.clone();
            tokio::task::spawn_blocking(move || settings::save(&dir, &p)).await
        };
        {
            let mut st = lock_std(&self.settings);
            st.effective = new.clone();
            st.persisted = persisted;
        }
        self.notifier.set_enabled(new.notifications);
        self.notifier.set_receive_dir(new.receive_dir.clone());
        if changes.autostart {
            self.apply_autostart(new.autostart).await;
        }
        self.clipboard_images
            .store(new.clipboard_images, Ordering::SeqCst);
        if changes.clipboard_sync {
            self.sync_clipboard_watcher().await;
        }
        let mut view = new.to_ipc(self.clipboard_supported);
        view.remote_input_supported = self.input_support.supported;
        view.remote_input_reason
            .clone_from(&self.input_support.reason);
        if changes.any() {
            self.emit(DaemonEvent::SettingsChanged {
                settings: view.clone(),
            });
        }
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                return Err(internal(format!(
                    "settings applied for now but not saved: {e:#}"
                )))
            }
            Err(e) => return Err(internal(format!("settings not saved: {e}"))),
        }
        Ok(IpcOk::Settings {
            settings: view,
            restarted: changes.restart_host,
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one match arm per request reads best"
    )]
    async fn handle(self: &Arc<Self>, req: Request) -> Result<IpcOk, IpcError> {
        // Settings work without a host; everything else needs one.
        match &req {
            Request::GetSettings => {
                return Ok(IpcOk::Settings {
                    settings: self.ipc_settings(),
                    restarted: false,
                })
            }
            Request::SetSettings(patch) => return self.set_settings(patch).await,
            Request::Shutdown => {
                self.shutdown_tx.send_replace(true);
                return Ok(IpcOk::Done);
            }
            _ => {}
        }
        let host = self
            .host()
            .ok_or_else(|| err(ErrorCode::InvalidState, "PlayDrop is starting; try again"))?;
        let done = |r: Result<(), CoreError>| r.map(|()| IpcOk::Done).map_err(map_core_error);
        match req {
            Request::Status => Ok(IpcOk::Status(self.status(&host))),
            Request::Peers => Ok(IpcOk::Peers(host.peers())),
            Request::Transfers => Ok(IpcOk::Transfers(host.transfers())),
            Request::Diagnose => {
                let core = host.diagnose();
                let port = probe::probe(core.port, &core.interfaces).await;
                let mut summary = probe::summary(&core, &port);
                summary.extend(self.diagnose_notes(core.port));
                Ok(IpcOk::Diagnostics(DiagnosticsReport {
                    core,
                    port,
                    summary,
                }))
            }
            Request::Connect { peer } => {
                let id = resolve(&host, &peer)?;
                done(host.connect(&id).await)
            }
            Request::SetGrants { peer, grants } => {
                let id = resolve(&host, &peer)?;
                done(host.set_grants(&id, grants).await)
            }
            Request::SetLabel { peer, label } => {
                let id = resolve(&host, &peer)?;
                let r = host.set_label(&id, label).await;
                self.tray_dirty.notify_one();
                done(r)
            }
            Request::Unpair { peer } => {
                let id = resolve(&host, &peer)?;
                done(host.unpair(&id).await)
            }
            Request::OpenAddDeviceWindow => done(host.open_add_device_window()),
            Request::CloseAddDeviceWindow => {
                host.close_add_device_window();
                Ok(IpcOk::Done)
            }
            Request::AddDeviceList => Ok(IpcOk::AddDeviceList(host.add_device_list())),
            Request::PairWith { addr } => done(host.pair_with(addr).await),
            Request::PairingRespond { accept } => {
                host.pairing_respond(accept);
                Ok(IpcOk::Done)
            }
            Request::ShowQr => host.show_qr().map(IpcOk::QrUri).map_err(map_core_error),
            Request::ScanQr { uri } => done(host.scan_qr(&uri).await),
            Request::SendFiles { peer, paths } => {
                let id = resolve(&host, &peer)?;
                let handle = host.send(&id, paths).await.map_err(map_core_error)?;
                Ok(IpcOk::TransferStarted {
                    id: handle.id().to_owned(),
                })
            }
            Request::QueueOutbox { peer, paths } => {
                let id = resolve(&host, &peer)?;
                let sources = paths.into_iter().map(SendSource::Path).collect();
                let item = host
                    .queue_outbox(&id, sources, None)
                    .await
                    .map_err(map_core_error)?;
                Ok(IpcOk::OutboxQueued { id: item })
            }
            Request::Outbox => Ok(IpcOk::Outbox(host.outbox())),
            Request::RemoveOutbox { id } => done(host.remove_outbox(&id).await),
            Request::PullOutbox { peer } => {
                let id = resolve_online(&host, &peer)?;
                let ids = host.pull_outbox(&id).await.map_err(map_core_error)?;
                Ok(IpcOk::OutboxPulled { ids })
            }
            Request::RespondOffer { id, accept } => done(host.respond_offer(&id, accept).await),
            Request::CancelTransfer { id } => done(host.cancel_transfer(&id).await),
            Request::Ping { peer, message } => {
                let id = resolve_online(&host, &peer)?;
                done(host.ping(&id, message.as_deref()))
            }
            Request::PushClipboard { text, sensitive } => {
                let p = host
                    .push_clipboard(&text, sensitive)
                    .map_err(map_core_error)?;
                let sent_to: Vec<String> = p.sent_to.iter().map(DeviceId::to_hex).collect();
                if !sent_to.is_empty() {
                    self.emit(DaemonEvent::ClipboardPushed {
                        sent_to: sent_to.clone(),
                    });
                }
                Ok(clip_answer(sent_to, clip_skip(&p)))
            }
            Request::SendClipboard {
                peer,
                text,
                sensitive,
            } => {
                let id = resolve_online(&host, &peer)?;
                let sent = host
                    .send_clipboard(&id, &text, sensitive)
                    .map_err(map_core_error)?;
                let sent_to = if sent { vec![id.to_hex()] } else { Vec::new() };
                if sent {
                    self.emit(DaemonEvent::ClipboardPushed {
                        sent_to: sent_to.clone(),
                    });
                }
                // `send_clipboard` has one "false" case: the echo rule. Every other
                // refusal (no grant, offline, too large) is an error above.
                Ok(clip_answer(sent_to, (!sent).then_some(ClipSkip::Echo)))
            }
            Request::OpenReceived { path } => {
                let dir = self.effective().receive_dir;
                crate::open::open_received(&path, &dir)
                    .map(|_| IpcOk::Done)
                    .map_err(map_open_error)
            }
            Request::ShowInFolder { path } => {
                let dir = self.effective().receive_dir;
                crate::open::show_in_folder(&path, &dir)
                    .await
                    .map(|()| IpcOk::Done)
                    .map_err(map_open_error)
            }
            Request::NetworkChanged => {
                host.network_changed();
                Ok(IpcOk::Done)
            }
            Request::StopInput => {
                let ended = host.stop_input_sessions();
                tracing::info!(count = ended.len(), "remote input stopped from the UI");
                Ok(IpcOk::Done)
            }
            Request::MediaCommand {
                peer,
                player,
                action,
                value,
            } => {
                let id = resolve_online(&host, &peer)?;
                done(host.send_media_command(&id, &player, action, value))
            }
            _ => Err(err(
                ErrorCode::BadRequest,
                "this daemon does not support that request",
            )),
        }
    }
}

/// Why a push sent nothing, from what the host counted. `None` when it sent to someone.
///
/// The host cannot say more than this: a peer that is offline and one with the grant off
/// both mean "nobody to send to", and it has no separate outcome for sensitive clips.
fn clip_skip(p: &playdrop_core::host::ClipboardPush) -> Option<ClipSkip> {
    if !p.sent_to.is_empty() {
        None
    } else if p.skipped_echo {
        Some(ClipSkip::Echo)
    } else if p.skipped_unsupported > 0 {
        Some(ClipSkip::Unsupported)
    } else {
        Some(ClipSkip::NoPeers)
    }
}

/// The IPC answer for a push; `skipped_echo` stays for older clients.
fn clip_answer(sent_to: Vec<String>, skipped: Option<ClipSkip>) -> IpcOk {
    IpcOk::ClipboardPushed {
        sent_to,
        skipped_echo: skipped == Some(ClipSkip::Echo),
        skipped,
    }
}

/// The notification body for a skipped push: the shared reason as a sentence.
fn skip_notice(skip: ClipSkip) -> String {
    let reason = skip.reason();
    let mut chars = reason.chars();
    chars.next().map_or_else(String::new, |first| {
        format!("{}{}.", first.to_uppercase(), chars.as_str())
    })
}

/// Pushes a local clip to every eligible peer: text as text (a blob over 32 KiB), an image
/// as an image blob (protocol.md 5.7, 5.15).
fn push(host: &Host, clip: LocalClip) -> Result<playdrop_core::host::ClipboardPush, CoreError> {
    match clip.content {
        ClipContent::Text(text) => host.push_clipboard(&text, clip.sensitive),
        ClipContent::Image { mime, bytes } => {
            host.push_clipboard_image(bytes, &mime, clip.sensitive)
        }
    }
}

/// The presence a lock state maps to (protocol.md 5.10).
fn presence_for(locked: bool) -> PresenceState {
    if locked {
        PresenceState::Locked
    } else {
        PresenceState::Active
    }
}

/// The [`RequestHandler`] the IPC server calls.
struct Handler(Arc<Shared>);

impl RequestHandler for Handler {
    fn handle(&self, request: Request) -> BoxFuture<'_, Response> {
        Box::pin(async move { self.0.handle(request).await.into() })
    }
}

fn err(code: ErrorCode, message: impl Into<String>) -> IpcError {
    IpcError {
        code,
        message: message.into(),
    }
}

fn internal(message: impl Into<String>) -> IpcError {
    err(ErrorCode::Internal, message)
}

/// Maps a core error to the stable IPC code (`docs/ipc.md`).
pub fn map_core_error(e: CoreError) -> IpcError {
    match e {
        CoreError::Locked => err(ErrorCode::Forbidden, LOCKED_MESSAGE),
        CoreError::NotPaired => err(ErrorCode::NotFound, "that device is not paired"),
        CoreError::NotFound => err(ErrorCode::NotFound, "not found"),
        CoreError::TooLarge(m) => err(ErrorCode::TooLarge, format!("too large: {m}")),
        // A missing grant is a policy refusal (SR-14), not an internal fault.
        CoreError::Forbidden(m) => err(ErrorCode::Forbidden, m),
        CoreError::BadRequest(m) => err(ErrorCode::BadRequest, m),
        CoreError::PairRejected(r) => err(ErrorCode::InvalidState, format!("pairing refused: {r}")),
        e => internal(e.to_string()),
    }
}

fn map_open_error(e: OpenError) -> IpcError {
    match e {
        OpenError::NotFound(m) => err(ErrorCode::NotFound, m),
        OpenError::Outside => err(ErrorCode::Forbidden, OpenError::Outside.to_string()),
        OpenError::Launch(m) => internal(m),
    }
}

/// Resolves a peer query; tells "nothing matches" apart from "several match", which
/// `Host::resolve_peer` folds into one `None`.
fn resolve(host: &Host, query: &str) -> Result<DeviceId, IpcError> {
    if let Some(id) = host.resolve_peer(query) {
        return Ok(id);
    }
    if count_matches(&host.peers(), query) > 1 {
        Err(err(
            ErrorCode::Ambiguous,
            format!("more than one paired device matches {query:?}"),
        ))
    } else {
        Err(err(
            ErrorCode::NotFound,
            format!("no paired device matches {query:?}"),
        ))
    }
}

fn resolve_online(host: &Host, query: &str) -> Result<DeviceId, IpcError> {
    let id = resolve(host, query)?;
    if host.is_online(&id) {
        Ok(id)
    } else {
        Err(err(ErrorCode::Offline, "that device is not connected"))
    }
}

/// How many peers a query matches, by the same rule as `Host::resolve_peer`: Short ID
/// prefix (hyphens optional), hex Device ID prefix, or exact label ignoring ASCII case.
fn count_matches(peers: &[PeerSummary], query: &str) -> usize {
    let q = query.trim().to_ascii_lowercase();
    let q_short: String = q.chars().filter(|c| *c != '-').collect();
    peers
        .iter()
        .filter(|p| {
            let s: String = p.short_id.chars().filter(|c| *c != '-').collect();
            (!q_short.is_empty() && s.starts_with(&q_short))
                || p.label.eq_ignore_ascii_case(query.trim())
                || p.device_id.starts_with(&q)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use playdrop_core::pin_store::Grants;
    use playdrop_ipc::SettingsPatch;

    use super::*;

    #[test]
    fn clip_skip_maps_every_core_outcome() {
        use playdrop_core::host::ClipboardPush;
        let id = DeviceId::from_spki_hash([7; 32]);
        let sent = ClipboardPush {
            sent_to: vec![id],
            // Counters on a sent push are irrelevant.
            skipped_unsupported: 1,
            ..ClipboardPush::default()
        };
        assert_eq!(clip_skip(&sent), None);
        let echo = ClipboardPush {
            skipped_echo: true,
            ..ClipboardPush::default()
        };
        assert_eq!(clip_skip(&echo), Some(ClipSkip::Echo));
        let unsupported = ClipboardPush {
            skipped_unsupported: 2,
            skipped_offline: 1,
            ..ClipboardPush::default()
        };
        assert_eq!(clip_skip(&unsupported), Some(ClipSkip::Unsupported));
        for (offline, no_grant) in [(0, 0), (1, 0), (0, 1), (2, 3)] {
            let none = ClipboardPush {
                skipped_offline: offline,
                skipped_no_grant: no_grant,
                ..ClipboardPush::default()
            };
            assert_eq!(clip_skip(&none), Some(ClipSkip::NoPeers));
        }
    }

    #[test]
    fn notice_is_the_shared_reason_as_a_sentence() {
        assert_eq!(
            skip_notice(ClipSkip::NoPeers),
            "No connected device has clipboard sharing turned on."
        );
        assert_eq!(
            skip_notice(ClipSkip::Echo),
            "It matches the last clip sent or received."
        );
    }

    #[test]
    fn clip_answer_derives_the_old_echo_flag() {
        for (skip, echo) in [
            (None, false),
            (Some(ClipSkip::Echo), true),
            (Some(ClipSkip::NoPeers), false),
            (Some(ClipSkip::Unsupported), false),
        ] {
            let IpcOk::ClipboardPushed {
                skipped_echo,
                skipped,
                ..
            } = clip_answer(Vec::new(), skip)
            else {
                panic!("wrong answer");
            };
            assert_eq!((skipped_echo, skipped), (echo, skip));
        }
    }

    fn test_daemon(tmp: &std::path::Path) -> Daemon {
        let s = Settings {
            receive_dir: tmp.join("in"),
            device_name: "smoke".into(),
            port: 0,
            clipboard_sync: true,
            clipboard_images: true,
            autostart: true,
            autostart_refused: None,
            notifications: false,
            discovery: false,
        };
        Daemon::new(Options {
            data_dir: tmp.join("data"),
            persisted: s.clone(),
            effective: s.clone(),
            bind_ip: IpAddr::V4(Ipv4Addr::LOCALHOST),
            lock: Arc::new(LockFlag::new()),
            notifier: Notifier::new(false, s.receive_dir),
            autostart: Some(Locations {
                config_home: tmp.join("config"),
                data_home: tmp.join("share"),
                exe: PathBuf::from("/usr/bin/playdropd"),
                flatpak: false,
            }),
            clipboard_supported: false,
            watch_clipboard: false,
            input_support: InputSupport {
                supported: false,
                backend: "none",
                reason: Some("test".into()),
            },
            media_support: MediaSupport {
                supported: false,
                backend: "none",
                reason: Some("test".into()),
            },
        })
    }

    async fn call(h: &Arc<dyn RequestHandler>, r: Request) -> Result<IpcOk, IpcError> {
        h.handle(r).await.into()
    }

    #[tokio::test]
    async fn stale_autostart_answers_are_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let d = test_daemon(tmp.path());
        let sh = Arc::clone(&d.shared);
        let mut events = d.events().subscribe();
        let reason = || Some("Start at login was not allowed.".to_owned());

        // The user asks twice; an answer for the first ask lands late and is ignored.
        sh.autostart_attempt.store(2, Ordering::SeqCst);
        sh.turn_autostart_off(1, reason()).await;
        assert!(sh.effective().autostart && sh.effective().autostart_refused.is_none());
        assert!(events.try_recv().is_err(), "a stale answer emits nothing");
        let on_disk = settings::load(&tmp.path().join("data")).unwrap();
        assert!(!on_disk.existed, "a stale answer saves nothing");

        // The current ask refuses: off, with the reason, saved and announced.
        sh.turn_autostart_off(2, reason()).await;
        let e = sh.effective();
        assert!(!e.autostart && e.autostart_refused == reason());
        assert!(events.try_recv().is_ok());
        let saved = settings::load(&tmp.path().join("data")).unwrap().settings;
        assert!(!saved.autostart && saved.autostart_refused == reason());

        // Already off: another refusal for the same ask changes nothing.
        sh.turn_autostart_off(2, Some("other".into())).await;
        assert_eq!(sh.effective().autostart_refused, reason());
        assert!(events.try_recv().is_err());

        // A late "allowed" or clear from an older ask cannot erase the reason.
        #[cfg(target_os = "linux")]
        {
            sh.set_autostart_refused(1, None).await;
            assert_eq!(sh.effective().autostart_refused, reason());
            assert!(events.try_recv().is_err());
            sh.set_autostart_refused(2, None).await;
            assert_eq!(sh.effective().autostart_refused, None);
            assert!(events.try_recv().is_ok());
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "one end-to-end walk through the handler"
    )]
    async fn smoke_status_peers_settings_and_shutdown() {
        let tmp = tempfile::tempdir().unwrap();
        let d = test_daemon(tmp.path());
        let h = d.handler();
        let mut events = d.events().subscribe();

        // Before the host runs, only settings answer.
        let e = call(&h, Request::Status).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidState);

        d.start_host().await.unwrap();
        let Ok(IpcOk::Status(st)) = call(&h, Request::Status).await else {
            panic!("status");
        };
        assert_eq!(st.ipc_version, IPC_VERSION);
        assert_eq!(st.settings.device_name, "smoke");
        assert!(!st.locked && !st.add_device_window_open);
        let first_addr = st.addr;

        let Ok(IpcOk::Peers(p)) = call(&h, Request::Peers).await else {
            panic!("peers");
        };
        assert_eq!(p.len(), 0);

        let Ok(IpcOk::Settings {
            settings,
            restarted,
        }) = call(&h, Request::GetSettings).await
        else {
            panic!("get settings");
        };
        assert!(!restarted && !settings.notifications);

        // A toggle does not restart; it is saved and announced.
        let Ok(IpcOk::Settings {
            settings,
            restarted,
        }) = call(
            &h,
            Request::SetSettings(SettingsPatch {
                autostart: Some(false),
                ..SettingsPatch::default()
            }),
        )
        .await
        else {
            panic!("set settings");
        };
        assert!(!restarted && !settings.autostart);
        assert!(
            !settings::load(&tmp.path().join("data"))
                .unwrap()
                .settings
                .autostart
        );

        // A new name restarts the host and keeps the identity.
        let Ok(IpcOk::Settings {
            settings,
            restarted,
        }) = call(
            &h,
            Request::SetSettings(SettingsPatch {
                device_name: Some("renamed".into()),
                ..SettingsPatch::default()
            }),
        )
        .await
        else {
            panic!("rename");
        };
        assert!(restarted);
        assert_eq!(settings.device_name, "renamed");
        let Ok(IpcOk::Status(st2)) = call(&h, Request::Status).await else {
            panic!("status after restart");
        };
        assert_eq!(st2.device_id, st.device_id);
        let _ = first_addr;

        let mut saw_restart = false;
        let mut saw_settings = false;
        while let Ok(ev) = events.try_recv() {
            match ev {
                Event::Daemon(DaemonEvent::HostRestarted) => saw_restart = true,
                Event::Daemon(DaemonEvent::SettingsChanged { .. }) => saw_settings = true,
                _ => {}
            }
        }
        assert!(saw_restart && saw_settings);

        // Bad patch.
        let e = call(
            &h,
            Request::SetSettings(SettingsPatch {
                receive_dir: Some(PathBuf::from("relative")),
                ..SettingsPatch::default()
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::BadRequest);

        // Unknown peer, path outside the receive folder.
        let e = call(
            &h,
            Request::Unpair {
                peer: "zzzz".into(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let e = call(
            &h,
            Request::OpenReceived {
                path: tmp.path().join("data").join("settings.json"),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::Forbidden);

        // Outbox: empty list, unknown item, unknown peer.
        assert_eq!(
            call(&h, Request::Outbox).await.unwrap(),
            IpcOk::Outbox(Vec::new())
        );
        let e = call(&h, Request::RemoveOutbox { id: "nope".into() })
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);
        let e = call(
            &h,
            Request::QueueOutbox {
                peer: "zzzz".into(),
                paths: vec![tmp.path().join("data")],
            },
        )
        .await
        .unwrap_err();
        assert_eq!(e.code, ErrorCode::NotFound);

        // SR-6: locked refuses the add-device window with the documented message.
        d.shared.lock.set_locked(true);
        let e = call(&h, Request::OpenAddDeviceWindow).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Forbidden);
        assert_eq!(e.message, LOCKED_MESSAGE);
        d.shared.lock.set_locked(false);

        let mut stop = d.shutdown_requested();
        assert!(matches!(call(&h, Request::Shutdown).await, Ok(IpcOk::Done)));
        assert!(*stop.borrow_and_update());
        d.shutdown().await;
        let e = call(&h, Request::Peers).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidState);
    }

    #[test]
    fn core_errors_map_to_stable_codes() {
        let cases = [
            (CoreError::Locked, ErrorCode::Forbidden),
            (CoreError::NotPaired, ErrorCode::NotFound),
            (CoreError::NotFound, ErrorCode::NotFound),
            (CoreError::TooLarge("33 KiB".into()), ErrorCode::TooLarge),
            (
                CoreError::Forbidden("clipboardOut grant is off"),
                ErrorCode::Forbidden,
            ),
            (CoreError::PairRejected("busy"), ErrorCode::InvalidState),
            (CoreError::RootMismatch, ErrorCode::Internal),
            (CoreError::PortHeld { port: 1 }, ErrorCode::Internal),
        ];
        for (e, code) in cases {
            assert_eq!(map_core_error(e).code, code);
        }
        assert_eq!(
            map_core_error(CoreError::Locked).message,
            "desktop is locked"
        );
        assert_eq!(
            map_open_error(OpenError::Outside).code,
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn ambiguous_queries_are_counted() {
        let p = |id: &str, short: &str, label: &str| PeerSummary {
            device_id: id.into(),
            short_id: short.into(),
            label: label.into(),
            online: false,
            grants: Grants::default(),
            last_known: vec![],
            paired_at: 0,
            last_seen: None,
            removed_by_peer: false,
            capabilities: vec![],
        };
        let peers = vec![
            p(&"ab".repeat(32), "k3qd-7f2m-xa9p-41bc", "Phone"),
            p(&"cd".repeat(32), "k3qx-0000-0000-0000", "Tablet"),
        ];
        assert_eq!(count_matches(&peers, "k3q"), 2);
        assert_eq!(count_matches(&peers, "k3qd"), 1);
        assert_eq!(count_matches(&peers, "phone"), 1);
        assert_eq!(count_matches(&peers, "nothing"), 0);
    }
}
