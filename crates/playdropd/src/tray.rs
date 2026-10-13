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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
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
/// then, inside Flatpak, through `flatpak run`. `page` is the window tab to open
/// (`--page` names: `devices`, `add-device`, `transfers`, `settings`, `diagnostics`), or
/// `None` for the default. The child is reaped in a background task.
pub fn launch_window(page: Option<&str>) -> anyhow::Result<()> {
    let name = if cfg!(windows) {
        "playdrop-desktop.exe"
    } else {
        "playdrop-desktop"
    };
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)))
        .filter(|p| p.is_file());
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

    use super::{device_line, icon_argb, TrayCommand, TrayView};

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

    fn action(label: &str, c: TrayCommand) -> MenuItem<SniTray> {
        StandardItem {
            label: label.to_owned(),
            activate: Box::new(move |t: &mut SniTray| t.send(c)),
            ..Default::default()
        }
        .into()
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
            let online = self.view.devices.iter().filter(|d| d.online).count();
            ksni::ToolTip {
                title: "PlayDrop".to_owned(),
                description: format!("{online} of {} devices online", self.view.devices.len()),
                ..Default::default()
            }
        }

        fn activate(&mut self, _x: i32, _y: i32) {
            self.send(TrayCommand::OpenWindow);
        }

        fn menu(&self) -> Vec<MenuItem<Self>> {
            let mut items: Vec<MenuItem<Self>> = Vec::new();
            if self.view.devices.is_empty() {
                items.push(
                    StandardItem {
                        label: "No paired devices".to_owned(),
                        enabled: false,
                        ..Default::default()
                    }
                    .into(),
                );
            }
            for d in &self.view.devices {
                items.push(
                    StandardItem {
                        label: device_line(d),
                        activate: Box::new(|t: &mut Self| t.send(TrayCommand::OpenWindow)),
                        ..Default::default()
                    }
                    .into(),
                );
            }
            items.push(MenuItem::Separator);
            items.push(action("Add device", TrayCommand::AddDevice));
            items.push(action("Send clipboard", TrayCommand::SendClipboard));
            items.push(action("Open window", TrayCommand::OpenWindow));
            items.push(
                CheckmarkItem {
                    label: "Pause clipboard sync".to_owned(),
                    enabled: self.view.clipboard_sync,
                    checked: self.view.clipboard_paused,
                    activate: Box::new(|t: &mut Self| {
                        let paused = !t.view.clipboard_paused;
                        t.view.clipboard_paused = paused;
                        t.send(TrayCommand::SetClipboardPaused(paused));
                    }),
                    ..Default::default()
                }
                .into(),
            );
            if !self.view.input_from.is_empty() {
                items.push(MenuItem::Separator);
                items.push(action(
                    &format!("Stop remote input from {}", self.view.input_from.join(", ")),
                    TrayCommand::StopInput,
                ));
            }
            items.push(MenuItem::Separator);
            items.push(action("Quit", TrayCommand::Quit));
            items
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
    use tokio::sync::mpsc;
    use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
    use tray_icon::{Icon, TrayIconBuilder};
    use windows::Win32::Foundation::{LPARAM, WPARAM};
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, GetMessageW, PostThreadMessageW, TranslateMessage, MSG, WM_QUIT,
    };

    use super::{icon_argb, TrayCommand, TrayView};

    /// The tray thread's ID, used to end its message loop.
    pub(super) struct Handle {
        thread_id: u32,
    }

    /// TODO(Phase 2 follow-up): device lines and battery in the Windows menu; the minimal
    /// menu below has the actions only.
    pub(super) async fn start(
        commands: mpsc::UnboundedSender<TrayCommand>,
        _view: TrayView,
    ) -> Option<Handle> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let spawned = std::thread::Builder::new()
            .name("playdropd-tray".into())
            .spawn(move || run(&commands, tx));
        if let Err(e) = spawned {
            tracing::info!(error = %e, "no tray thread");
            return None;
        }
        rx.await
            .ok()
            .flatten()
            .map(|thread_id| Handle { thread_id })
    }

    fn rgba(size: u32) -> Vec<u8> {
        icon_argb(size)
            .as_chunks::<4>()
            .0
            .iter()
            .flat_map(|p| [p[1], p[2], p[3], p[0]])
            .collect()
    }

    #[allow(unsafe_code)]
    fn run(
        commands: &mpsc::UnboundedSender<TrayCommand>,
        ready: tokio::sync::oneshot::Sender<Option<u32>>,
    ) {
        let menu = Menu::new();
        let add = MenuItem::new("Add device", true, None);
        let clip = MenuItem::new("Send clipboard", true, None);
        let open = MenuItem::new("Open window", true, None);
        let pause = CheckMenuItem::new("Pause clipboard sync", true, false, None);
        // Always present on Windows: the menu is built once and `update` is a no-op there.
        let stop_input = MenuItem::new("Stop remote input", true, None);
        let quit = MenuItem::new("Quit", true, None);
        let built = menu
            .append_items(&[
                &add,
                &clip,
                &open,
                &pause,
                &PredefinedMenuItem::separator(),
                &stop_input,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .map_err(|e| e.to_string())
            .and_then(|()| Icon::from_rgba(rgba(32), 32, 32).map_err(|e| e.to_string()))
            .and_then(|icon| {
                TrayIconBuilder::new()
                    .with_menu(Box::new(menu))
                    .with_tooltip("PlayDrop")
                    .with_icon(icon)
                    .build()
                    .map_err(|e| e.to_string())
            });
        let _tray = match built {
            Ok(t) => t,
            Err(e) => {
                tracing::info!(error = %e, "no tray icon");
                let _ = ready.send(None);
                return;
            }
        };
        let ids = (
            add.id().clone(),
            clip.id().clone(),
            open.id().clone(),
            pause.id().clone(),
            quit.id().clone(),
            stop_input.id().clone(),
        );
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
                let c = if e.id == ids.0 {
                    TrayCommand::AddDevice
                } else if e.id == ids.1 {
                    TrayCommand::SendClipboard
                } else if e.id == ids.2 {
                    TrayCommand::OpenWindow
                } else if e.id == ids.3 {
                    // The check item flips itself before the event arrives.
                    TrayCommand::SetClipboardPaused(pause.is_checked())
                } else if e.id == ids.4 {
                    TrayCommand::Quit
                } else if e.id == ids.5 {
                    TrayCommand::StopInput
                } else {
                    continue;
                };
                let _ = commands.send(c);
            }
        }
    }

    #[allow(clippy::unused_async)]
    pub(super) async fn update(_h: &Handle, _view: TrayView) {}

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
