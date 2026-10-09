//! Desktop lock state for SR-6: the "Add device" window must not open while the screen is
//! locked. The core asks a [`LockFlag`] on every attempt; this module keeps the flag current
//! and tells the daemon about changes through a `watch` channel.
//!
//! Linux reads logind's `LockedHint` on our session over the system bus and follows its
//! `PropertiesChanged` signal. Our session is found with `GetSessionByPID` (works when the
//! daemon runs inside the login session), then `GetSession("auto")`, then the user's
//! display session (`/org/freedesktop/login1/user/self` `Display`), which is what a daemon
//! started by D-Bus activation or a systemd user unit needs.
//!
//! *Choice* on unknown state. SR-25 says unknown counts as locked, but SR-25 is a Phase 5
//! input-injection rule; for pairing (SR-6) the question is whether a lock screen exists:
//! - logind present but `LockedHint` unreadable: locked (warn). Pairing is refused until a
//!   readable unlocked state arrives, so a broken integration fails closed.
//! - logind absent (no system bus or nobody owns `org.freedesktop.login1`): a system
//!   without lock screen integration, so unlocked (info). Refusing would make pairing
//!   impossible on such systems while protecting nothing.
//!
//! Windows registers a hidden message-only window for `WTS_SESSION_LOCK` and
//! `WTS_SESSION_UNLOCK`. The daemon starts at login, so the initial state is unlocked.

use std::sync::Arc;

use playdrop_core::host::LockFlag;
use tokio::sync::watch;

/// Starts tracking the lock state. Returns the flag to hand to the core and a receiver that
/// changes whenever the state does. Never fails because the platform has no lock signal;
/// the error case is reserved for a setup failure the caller should report.
pub async fn start() -> anyhow::Result<(Arc<LockFlag>, watch::Receiver<bool>)> {
    let flag = Arc::new(LockFlag::new());
    let (tx, rx) = watch::channel(false);
    platform::start(Arc::clone(&flag), tx).await?;
    Ok((flag, rx))
}

/// Records a new state in both the flag and the channel.
fn publish(flag: &LockFlag, tx: &watch::Sender<bool>, locked: bool) {
    flag.set_locked(locked);
    tx.send_if_modified(|cur| {
        let changed = *cur != locked;
        *cur = locked;
        changed
    });
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Arc;

    use futures_util::StreamExt as _;
    use playdrop_core::host::LockFlag;
    use tokio::sync::watch;
    use zbus::zvariant::OwnedObjectPath;

    const LOGIND: &str = "org.freedesktop.login1";

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1",
        gen_blocking = false
    )]
    trait Manager {
        #[zbus(name = "GetSessionByPID")]
        fn get_session_by_pid(&self, pid: u32) -> zbus::Result<OwnedObjectPath>;
        fn get_session(&self, id: &str) -> zbus::Result<OwnedObjectPath>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.login1.User",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1/user/self",
        gen_blocking = false
    )]
    trait User {
        #[zbus(property)]
        fn display(&self) -> zbus::Result<(String, OwnedObjectPath)>;
    }

    #[zbus::proxy(
        interface = "org.freedesktop.login1.Session",
        default_service = "org.freedesktop.login1",
        gen_blocking = false
    )]
    trait Session {
        #[zbus(property)]
        fn locked_hint(&self) -> zbus::Result<bool>;
    }

    pub(super) async fn start(flag: Arc<LockFlag>, tx: watch::Sender<bool>) -> anyhow::Result<()> {
        let conn = match zbus::Connection::system().await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(error = %e, "no system bus; treating the desktop as unlocked (no lock screen integration)");
                return Ok(());
            }
        };
        let present = match zbus::fdo::DBusProxy::new(&conn).await {
            Ok(p) => p
                .name_has_owner(LOGIND.try_into().map_err(anyhow::Error::from)?)
                .await
                .unwrap_or(false),
            Err(_) => false,
        };
        if !present {
            tracing::info!("logind is not running; treating the desktop as unlocked (no lock screen integration)");
            return Ok(());
        }
        let session = match find_session(&conn).await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "logind is present but our session was not found; pairing stays refused (SR-6)");
                super::publish(&flag, &tx, true);
                return Ok(());
            }
        };
        let proxy = SessionProxy::builder(&conn).path(session)?.build().await?;
        match proxy.locked_hint().await {
            Ok(l) => super::publish(&flag, &tx, l),
            Err(e) => {
                tracing::warn!(error = %e, "LockedHint unreadable; pairing stays refused until it reads unlocked (SR-6)");
                super::publish(&flag, &tx, true);
            }
        }
        let mut changes = proxy.receive_locked_hint_changed().await;
        tokio::spawn(async move {
            while let Some(change) = changes.next().await {
                let locked = match change.get().await {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "LockedHint unreadable; treating as locked (SR-6)");
                        true
                    }
                };
                tracing::debug!(locked, "desktop lock state changed");
                super::publish(&flag, &tx, locked);
            }
            tracing::info!("logind lock signal ended; keeping the last known state");
        });
        Ok(())
    }

    async fn find_session(conn: &zbus::Connection) -> anyhow::Result<OwnedObjectPath> {
        let mgr = ManagerProxy::new(conn).await?;
        match mgr.get_session_by_pid(std::process::id()).await {
            Ok(p) => return Ok(p),
            Err(e) => tracing::debug!(error = %e, "GetSessionByPID found nothing"),
        }
        match mgr.get_session("auto").await {
            Ok(p) => return Ok(p),
            Err(e) => tracing::debug!(error = %e, "GetSession(auto) found nothing"),
        }
        let user = UserProxy::new(conn).await?;
        let (id, path) = user.display().await?;
        if id.is_empty() {
            anyhow::bail!("the user has no display session");
        }
        Ok(path)
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::{Arc, OnceLock};

    use playdrop_core::host::LockFlag;
    use tokio::sync::watch;
    use windows::core::w;
    use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::RemoteDesktop::{
        WTSRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, RegisterClassW,
        TranslateMessage, HWND_MESSAGE, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_WTSSESSION_CHANGE,
        WNDCLASSW, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
    };

    /// Where the window procedure publishes. Set once before the window exists.
    static TARGET: OnceLock<(Arc<LockFlag>, watch::Sender<bool>)> = OnceLock::new();

    #[allow(
        clippy::unused_async,
        reason = "same signature as the logind watcher on Linux"
    )]
    pub(super) async fn start(flag: Arc<LockFlag>, tx: watch::Sender<bool>) -> anyhow::Result<()> {
        if TARGET.set((flag, tx)).is_err() {
            anyhow::bail!("lock tracking already started");
        }
        std::thread::Builder::new()
            .name("playdropd-lock".into())
            .spawn(message_loop)?;
        Ok(())
    }

    extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if msg == WM_WTSSESSION_CHANGE {
            let locked = match u32::try_from(wparam.0).unwrap_or(0) {
                WTS_SESSION_LOCK => Some(true),
                WTS_SESSION_UNLOCK => Some(false),
                _ => None,
            };
            if let (Some(l), Some((flag, tx))) = (locked, TARGET.get()) {
                super::publish(flag, tx, l);
            }
            return LRESULT(0);
        }
        // SAFETY: forwarding the unmodified arguments of a window message to the default
        // procedure, on the thread that owns the window.
        #[allow(unsafe_code)]
        unsafe {
            DefWindowProcW(hwnd, msg, wparam, lparam)
        }
    }

    #[allow(unsafe_code)]
    fn message_loop() {
        let class = w!("PlayDropLockWatcher");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            lpszClassName: class,
            ..Default::default()
        };
        // SAFETY: `wc` is fully initialised and outlives the call; the class name is a
        // static wide string. A null hInstance registers the class for this process image.
        if unsafe { RegisterClassW(&raw const wc) } == 0 {
            tracing::warn!("lock watcher window class not registered; desktop counts as unlocked");
            return;
        }
        // SAFETY: a message-only window (parent HWND_MESSAGE) of the class registered above;
        // all pointer arguments are static strings or None.
        let hwnd = match unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                w!("PlayDrop lock watcher"),
                WINDOW_STYLE(0),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                None,
                None,
            )
        } {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "lock watcher window not created; desktop counts as unlocked");
                return;
            }
        };
        // SAFETY: `hwnd` is the live window created above on this thread.
        if let Err(e) = unsafe { WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION) } {
            tracing::warn!(error = %e, "session notifications not registered; desktop counts as unlocked");
            return;
        }
        let mut msg = MSG::default();
        // SAFETY: standard message pump on the thread that owns the window; `msg` is a
        // valid, writable MSG for every call. GetMessageW returns 0 on WM_QUIT and -1 on
        // error; both end the loop.
        unsafe {
            while GetMessageW(&raw mut msg, None, 0, 0).0 > 0 {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use std::sync::Arc;

    use playdrop_core::host::LockFlag;
    use tokio::sync::watch;

    pub(super) async fn start(
        _flag: Arc<LockFlag>,
        _tx: watch::Sender<bool>,
    ) -> anyhow::Result<()> {
        tracing::info!("no lock signal on this platform; treating the desktop as unlocked");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use playdrop_core::host::LockState as _;

    use super::*;

    #[test]
    fn publish_updates_flag_and_channel_once() {
        let flag = LockFlag::new();
        let (tx, mut rx) = watch::channel(false);
        publish(&flag, &tx, true);
        assert!(flag.is_locked());
        assert!(rx.has_changed().unwrap());
        assert!(*rx.borrow_and_update());
        publish(&flag, &tx, true);
        assert!(!rx.has_changed().unwrap(), "same state is not a change");
        publish(&flag, &tx, false);
        assert!(!flag.is_locked());
        assert!(rx.has_changed().unwrap());
    }
}
