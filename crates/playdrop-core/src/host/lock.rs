//! Desktop lock state for SR-6: "Add device" must not open while the screen is locked, so
//! someone at a locked machine cannot pair a new device (threat model SR-6, protocol.md
//! 6.1).
//!
//! The core has no portable way to ask; the host process supplies it. On Linux the daemon
//! reads logind's `LockedHint` on its session, on Windows it tracks `WTS_SESSION_LOCK` and
//! `WTS_SESSION_UNLOCK`; a phone host reports its keyguard state. The CLI has no lock
//! signal and uses [`AlwaysUnlocked`].

/// Answers "is the desktop locked right now?". Called on every attempt to open the
/// "Add device" window, so it must be cheap (read a cached flag, do not block on IPC).
pub trait LockState: Send + Sync + 'static {
    /// True while the user's session is locked.
    fn is_locked(&self) -> bool;
}

/// A [`LockState`] for hosts without a lock signal (the CLI, tests): never locked.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysUnlocked;

impl LockState for AlwaysUnlocked {
    fn is_locked(&self) -> bool {
        false
    }
}

/// A [`LockState`] backed by a shared flag the host process flips from its own lock
/// listener (logind signal, WTS notification).
#[derive(Debug, Clone, Default)]
pub struct LockFlag(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl LockFlag {
    /// A new flag, unlocked.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the current lock state.
    pub fn set_locked(&self, locked: bool) {
        self.0.store(locked, std::sync::atomic::Ordering::SeqCst);
    }
}

impl LockState for LockFlag {
    fn is_locked(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    use super::LockState;
    use crate::host::{Host, HostConfig};
    use crate::Error;

    /// A lock source the test flips by hand.
    struct Stub(Arc<AtomicBool>);

    impl LockState for Stub {
        fn is_locked(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn pairing_is_refused_while_locked() {
        let tmp = tempfile::tempdir().unwrap();
        let locked = Arc::new(AtomicBool::new(true));
        let mut cfg = HostConfig::new(tmp.path().join("d"), tmp.path().join("r"), "lock");
        cfg.port = 0;
        cfg.bind_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        cfg.discovery = false;
        cfg.lock_state = Box::new(Stub(Arc::clone(&locked)));
        let (host, _events) = Host::start(cfg).await.unwrap();

        assert!(matches!(host.open_add_device_window(), Err(Error::Locked)));
        assert!(!host.add_device_window_open());
        assert!(matches!(host.show_qr(), Err(Error::Locked)));
        let nowhere = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 9);
        assert!(matches!(host.pair_with(nowhere).await, Err(Error::Locked)));
        assert!(matches!(
            host.scan_qr("playdrop://pair?bad").await,
            Err(Error::Locked)
        ));

        locked.store(false, Ordering::SeqCst);
        host.open_add_device_window().unwrap();
        assert!(host.add_device_window_open());
        host.close_add_device_window();

        locked.store(true, Ordering::SeqCst);
        assert!(matches!(host.open_add_device_window(), Err(Error::Locked)));
        host.shutdown().await;
    }
}
