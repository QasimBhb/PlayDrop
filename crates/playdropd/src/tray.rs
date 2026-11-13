//! The status icon: `ksni` (`StatusNotifierItem`) on Linux, `tray-icon` on Windows.
//!
//! Menu: the paired devices (online or offline, battery when known), "Add device", "Send
//! clipboard", "Open window", "Pause clipboard sync" and "Quit". The tray never talks to
//! the host itself: each click becomes a [`TrayCommand`] the daemon handles, and the daemon
//! pushes a fresh [`TrayView`] when something the menu shows changes.
//!
//! A desktop without a `StatusNotifierWatcher` (stock GNOME without the `AppIndicator`
//! extension) gets no tray; that is logged at info and the daemon carries on
//! (anti-pattern 18: say why, do not fail).

use std::path::Path;

use tokio::sync::mpsc;

mod menu;

/// A tray click, handled by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    /// Open the "Add device" window and show the `PlayDrop` window.
    AddDevice,
    /// Read the clipboard now and push it (manual send, SR-24).
    SendClipboard,
    /// Start the `PlayDrop` window.
    OpenWindow,
    /// Pause (true) or resume automatic clipboard sync for this run.
    SetClipboardPaused(bool),
    /// End every remote input session now (the SR-25 kill switch).
    StopInput,
    /// Quit the daemon.
    Quit,
}

/// One device line of the menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayDevice {
    /// Local label.
    pub label: String,
    /// Session up.
    pub online: bool,
    /// Last reported battery percent.
    pub battery: Option<u8>,
    /// Seconds since the device last sent anything, when it is offline and was ever seen
    /// (SR-8). Computed when the view is built; the tray refreshes it every minute.
    pub seen_secs_ago: Option<u64>,
}

/// Everything the menu shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayView {
    /// Paired devices.
    pub devices: Vec<TrayDevice>,
    /// Automatic clipboard sync is paused.
    pub clipboard_paused: bool,
    /// Automatic clipboard sync is running at all (setting on and platform supported).
    pub clipboard_sync: bool,
    /// Labels of the peers controlling this desktop right now (remote input, SR-25).
    pub input_from: Vec<String>,
}

/// The menu text of a device line.
pub fn device_line(d: &TrayDevice) -> String {
    let state = match (d.online, d.seen_secs_ago) {
        (true, _) => "online".to_owned(),
        (false, Some(ago)) => format!("offline, seen {}", ago_text(ago)),
        (false, None) => "offline".to_owned(),
    };
    match d.battery {
        Some(b) => format!("{} - {state}, battery {b}%", d.label),
        None => format!("{} - {state}", d.label),
    }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
fn ago_text(secs: u64) -> String {
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        86_400..=172_799 => "1 day ago".to_owned(),
        _ => format!("{} days ago", secs / 86_400),
    }
}

/// A running tray.
pub struct Tray {
    inner: platform::Handle,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tray").finish_non_exhaustive()
    }
}

impl Tray {
    /// Shows the tray, or returns `None` (logged) when the desktop has no tray host.
    pub async fn start(
        commands: mpsc::UnboundedSender<TrayCommand>,
        view: TrayView,
    ) -> Option<Self> {
        platform::start(commands, view)
            .await
            .map(|inner| Self { inner })
    }

    /// Replaces what the menu shows.
    pub async fn update(&self, view: TrayView) {
        platform::update(&self.inner, view).await;
    }

    /// Removes the icon.
    pub async fn shutdown(self) {
        platform::shutdown(self.inner).await;
    }
}

/// True inside a Flatpak sandbox.
pub fn sandboxed() -> bool {
    Path::new("/.flatpak-info").exists()
}

/// Starts the `PlayDrop` window: `playdrop-desktop` next to this binary, then on `PATH`,
/// then, inside Flatpak, through `flatpak run`. The Windows installer puts the window next
/// to the daemon as `PlayDrop.exe`; that name is looked for there only, never on `PATH`,
/// where file names ignore case and it would find the `playdrop.exe` command. `page` is the window tab to open
/// (`--page` names: `devices`, `add-device`, `transfers`, `settings`, `diagnostics`), or
/// `None` for the default. The child is reaped in a background task.
pub fn launch_window(page: Option<&str>) -> anyhow::Result<()> {
    /// The window's file name in a Windows install (`packaging/windows/playdrop.iss`).
    const INSTALLED_WINDOW_EXE: &str = "PlayDrop.exe";
    let name = if cfg!(windows) {
        "playdrop-desktop.exe"
    } else {
        "playdrop-desktop"
    };
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(std::path::Path::to_path_buf));
    let installed = beside
        .as_ref()
        .filter(|_| cfg!(windows))
        .map(|d| d.join(INSTALLED_WINDOW_EXE))
        .filter(|p| p.is_file());
    let sibling = installed.or_else(|| {
        beside
            .as_ref()
            .map(|d| d.join(name))
            .filter(|p| p.is_file())
    });
    let on_path = || {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    };
    let mut cmd = if let Some(p) = sibling.or_else(on_path) {
        tokio::process::Command::new(p)
    } else if sandboxed() {
        let mut c = tokio::process::Command::new("flatpak");
        c.args(["run", "--command=playdrop-desktop", "org.playdrop.PlayDrop"]);
        c
    } else {
        anyhow::bail!("playdrop-desktop was not found next to playdropd or on PATH");
    };
    if let Some(page) = page {
        cmd.arg("--page").arg(page);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start the PlayDrop window: {e}"))?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

/// A 32x32 ARGB icon drawn in code (a filled circle with a down arrow), so the tray works
/// even when no themed icon is installed.
fn icon_argb(size: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let extent = f64::from(size);
    let centre = (extent - 1.0) / 2.0;
    let radius = extent / 2.0 - 1.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (f64::from(x) - centre, f64::from(y) - centre);
            let inside = dx * dx + dy * dy <= radius * radius;
            // Arrow: a vertical bar and a triangle head, in white.
            let bar = dx.abs() <= extent * 0.07 && dy >= -extent * 0.28 && dy <= extent * 0.1;
            let head = dy >= extent * 0.02
                && dy <= extent * 0.28
                && dx.abs() <= (extent * 0.28 - dy) * 0.9;
            let argb: [u8; 4] = match (inside, bar || head) {
                (false, _) => [0, 0, 0, 0],
                (true, true) => [255, 255, 255, 255],
                (true, false) => [255, 0x2b, 0x6c, 0xd8],
            };
            out.extend_from_slice(&argb);
        }
    }
    out
}

#[cfg(target_os = "linux")]
mod platform {
    use ksni::menu::{CheckmarkItem, StandardItem};
    use ksni::{MenuItem, TrayMethods as _};
    use tokio::sync::mpsc;

    use super::menu::{menu_entries, online_summary, MenuEntry, PAUSE_LABEL};
    use super::{icon_argb, TrayCommand, TrayView};

    pub(super) type Handle = ksni::Handle<SniTray>;

    pub(super) struct SniTray {
        view: TrayView,
        commands: mpsc::UnboundedSender<TrayCommand>,
    }

    impl SniTray {
        fn send(&self, c: TrayCommand) {
            let _ = self.commands.send(c);
        }
    }

    impl ksni::Tray for SniTray {
        fn id(&self) -> String {
            "org.playdrop.PlayDrop".to_owned()
        }

        fn title(&self) -> String {
            "PlayDrop".to_owned()
        }

        fn icon_name(&self) -> String {
            "org.playdrop.PlayDrop".to_owned()
        }

        fn icon_pixmap(&self) -> Vec<ksni::Icon> {
            vec![ksni::Icon {
                width: 32,
                height: 32,
                data: icon_argb(32),
            }]
        }

        fn tool_tip(&self) -> ksni::ToolTip {
            ksni::ToolTip {
                title: "PlayDrop".to_owned(),
                description: online_summary(&self.view),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::OpenWindow);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            menu_entries(&self.view)
                .into_iter()
                .map(|e| match e {
                    MenuEntry::Item {
                        label,
                        enabled,
                        command,
                    } => StandardItem {
                        label,
                        enabled,
                        activate: Box::new(move |t: &mut Self| {
                            if let Some(c) = command {
                                t.send(c);
                            }
                        }),
                        ..Default::default()
                    }
                    .into(),
                    MenuEntry::PauseClipboard { enabled, checked } => CheckmarkItem {
                        label: PAUSE_LABEL.to_owned(),
                        enabled,
                        checked,
                        activate: Box::new(|t: &mut Self| {
                            let paused = !t.view.clipboard_paused;
                            t.view.clipboard_paused = paused;
                            t.send(TrayCommand::SetClipboardPaused(paused));
                        }),
                        ..Default::default()
                    }
                    .into(),
                    MenuEntry::Separator => MenuItem::Separator,
                })
                .collect()
        }

        fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
            tracing::info!(
                ?reason,
                "the tray host went away; the icon returns when it does"
            );
            true
        }
    }

    pub(super) async fn start(
        commands: mpsc::UnboundedSender<TrayCommand>,
        view: TrayView,
    ) -> Option<Handle> {
        let tray = SniTray { view, commands };
        // Inside Flatpak the sandbox may only own the names its manifest grants, so the
        // per-process StatusNotifierItem name is skipped there (ksni `disable_dbus_name`).
        match tray.disable_dbus_name(super::sandboxed()).spawn().await {
            Ok(h) => Some(h),
            Err(e) => {
                tracing::info!(error = %e, "no tray: the desktop has no StatusNotifierItem host; use the window or notifications");
                None
            }
        }
    }

    pub(super) async fn update(h: &Handle, view: TrayView) {
        let _ = h.update(move |t| t.view = view).await;
    }

    pub(super) async fn shutdown(h: Handle) {
        h.shutdown().await;
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::{Arc, Mutex, PoisonError};

    use tokio::sync::mpsc;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_APP, WM_QUIT,
    };

    use super::menu::{
        clicked, menu_entries, online_summary, win32_menu_text, MenuEntry, PAUSE_LABEL,
    };
    use super::{icon_argb, TrayCommand, TrayView};

    /// Wakes the tray thread to pick up a new view. The view itself waits in
    /// [`Handle::pending`], so a wake-up lost to a modal loop (an open menu runs one) only
    /// delays it to the next message the thread gets.
    const WM_PLAYDROP_VIEW: u32 = WM_APP + 1;

    /// The tray thread's ID, used to wake it and to end its message loop, and the newest
    /// view it has not shown yet.
    pub(super) struct Handle {
        thread_id: u32,
        pending: Arc<Mutex<Option<TrayView>>>,
    }

    pub(super) async fn start(
        commands: mpsc::UnboundedSender<TrayCommand>,
        view: TrayView,
    ) -> Option<Handle> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let pending = Arc::new(Mutex::new(None));
        let thread_pending = Arc::clone(&pending);
        let spawned = std::thread::Builder::new()
            .name("playdropd-tray".into())
            .spawn(move || run(&commands, &view, &thread_pending, tx));
        if let Err(e) = spawned {
            tracing::info!(error = %e, "no tray thread");
            return None;
        }
        rx.await
            .ok()
            .flatten()
            .map(|thread_id| Handle { thread_id, pending })
    }

    fn rgba(size: u32) -> Vec<u8> {
        icon_argb(size)
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[1], p[2], p[3], p[0]])
            .collect()
    }

    fn tooltip(view: &TrayView) -> String {
        format!("PlayDrop - {}", online_summary(view))
    }

    /// The native menu for one list of entries, and what each clickable item sends.
    struct Rendered {
        /// What the menu shows now (the pause item kept in step with clicks).
        entries: Vec<MenuEntry>,
        /// The native item ID of each entry, in the same order.
        ids: Vec<MenuId>,
        /// The pause item, read after a click (it flips itself before the event).
        pause: Option<CheckMenuItem>,
    }

    fn render(entries: Vec<MenuEntry>) -> Result<(Menu, Rendered), String> {
        let menu = Menu::new();
        let mut ids = Vec::with_capacity(entries.len());
        let mut pause = None;
        for e in &entries {
            let appended = match e {
                MenuEntry::Item { label, enabled, .. } => {
                    let item = MenuItem::new(win32_menu_text(label), *enabled, None);
                    ids.push(item.id().clone());
                    menu.append(&item)
                }
                MenuEntry::PauseClipboard { enabled, checked } => {
                    let item = CheckMenuItem::new(PAUSE_LABEL, *enabled, *checked, None);
                    ids.push(item.id().clone());
                    let r = menu.append(&item);
                    pause = Some(item);
                    r
                }
                MenuEntry::Separator => {
                    let item = PredefinedMenuItem::separator();
                    ids.push(item.id().clone());
                    menu.append(&item)
                }
            };
            appended.map_err(|e| e.to_string())?;
        }
        Ok((
            menu,
            Rendered {
                entries,
                ids,
                pause,
            },
        ))
    }

    /// Replaces the menu and tooltip when `view` shows something new.
    fn show_view(tray: &TrayIcon, current: &mut Rendered, view: &TrayView) {
        let entries = menu_entries(view);
        if entries == current.entries {
            return;
        }
        match render(entries) {
            Ok((menu, rendered)) => {
                tray.set_menu(Some(Box::new(menu)));
                *current = rendered;
                if let Err(e) = tray.set_tooltip(Some(tooltip(view))) {
                    tracing::debug!(error = %e, "tray tooltip not updated");
                }
            }
            Err(e) => tracing::debug!(error = %e, "tray menu not rebuilt"),
        }
    }

    /// The command a menu click sends, if the item is one of ours.
    fn command_for(current: &mut Rendered, id: &MenuId) -> Option<TrayCommand> {
        let pause_checked = current.pause.as_ref().map(CheckMenuItem::is_checked);
        clicked(&mut current.entries, &current.ids, id, pause_checked)
    }

    #[allow(unsafe_code)]
    fn run(
        commands: &mpsc::UnboundedSender<TrayCommand>,
        view: &TrayView,
        pending: &Mutex<Option<TrayView>>,
        ready: tokio::sync::oneshot::Sender<Option<u32>>,
    ) {
        let built = render(menu_entries(view)).and_then(|(menu, rendered)| {
            let icon = Icon::from_rgba(rgba(32), 32, 32).map_err(|e| e.to_string())?;
            TrayIconBuilder::new()
                .with_menu(Box::new(menu))
                .with_tooltip(tooltip(view))
                .with_icon(icon)
                .build()
                .map(|t| (t, rendered))
                .map_err(|e| e.to_string())
        });
        let (tray, mut current) = match built {
            Ok(t) => t,
            Err(e) => {
                tracing::info!(error = %e, "no tray icon");
                let _ = ready.send(None);
                return;
            }
        };
        // Menu events are read on this thread, after each dispatched message, because the
        // menu items are not `Send` and `pause.is_checked()` must run where they live.
        let menu_events = MenuEvent::receiver();
        // SAFETY: GetCurrentThreadId has no preconditions.
        let _ = ready.send(Some(unsafe { GetCurrentThreadId() }));
        let mut msg = MSG::default();
        loop {
            // SAFETY: standard message pump on the thread that owns the tray window; `msg`
            // is a valid, writable MSG for every call.
            // GetMessageW returns 0 on WM_QUIT and -1 on error; both end the loop.
            let more = unsafe { GetMessageW(&raw mut msg, None, 0, 0).0 > 0 };
            if !more {
                break;
            }
            // SAFETY: `msg` was filled by GetMessageW on this thread just above.
            unsafe {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
            while let Ok(e) = menu_events.try_recv() {
                if let Some(c) = command_for(&mut current, &e.id) {
                    let _ = commands.send(c);
                }
            }
            // Checked after every message, not only WM_PLAYDROP_VIEW: see that constant.
            let next = pending
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(view) = next {
                show_view(&tray, &mut current, &view);
            }
        }
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn update(h: &Handle, view: TrayView) {
        *h.pending.lock().unwrap_or_else(PoisonError::into_inner) = Some(view);
        // SAFETY: posting a message with no pointers to a thread ID we got from that
        // thread; a stale ID only makes the call fail.
        #[allow(unsafe_code)]
        let _ = unsafe { PostThreadMessageW(h.thread_id, WM_PLAYDROP_VIEW, WPARAM(0), LPARAM(0)) };
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn shutdown(h: Handle) {
        // SAFETY: posting WM_QUIT to a thread ID we got from that thread; a stale ID only
        // makes the call fail.
        #[allow(unsafe_code)]
        let _ = unsafe { PostThreadMessageW(h.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use tokio::sync::mpsc;

    use super::{TrayCommand, TrayView};

    pub(super) struct Handle;

    #[allow(clippy::unused_async)]
    pub(super) async fn start(
        _c: mpsc::UnboundedSender<TrayCommand>,
        _v: TrayView,
    ) -> Option<Handle> {
        tracing::info!("no tray on this platform");
        None
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn update(_h: &Handle, _view: TrayView) {}

    #[allow(clippy::unused_async)]
    pub(super) async fn shutdown(_h: Handle) {}
}

#[cfg(test)]
mod tests {
    use super::{device_line, TrayDevice};

    #[test]
    fn device_lines_show_last_seen_when_offline() {
        let mut d = TrayDevice {
            label: "Phone".into(),
            online: false,
            battery: None,
            seen_secs_ago: Some(300),
        };
        assert_eq!(device_line(&d), "Phone - offline, seen 5 min ago");
        d.seen_secs_ago = None;
        assert_eq!(device_line(&d), "Phone - offline");
        d.online = true;
        d.battery = Some(80);
        d.seen_secs_ago = Some(0);
        assert_eq!(device_line(&d), "Phone - online, battery 80%");
    }
}
