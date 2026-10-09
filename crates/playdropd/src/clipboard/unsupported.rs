//! Sessions where the daemon cannot watch or write the clipboard, with the reason shown to the
//! user (anti-pattern 18: say why). On Linux and Windows only the reasons and the error helper
//! are used; on any other target this file is the whole backend.

use super::ClipboardSupport;

#[cfg(target_os = "linux")]
/// Wayland compositors without a data-control protocol, GNOME Mutter among them.
pub(super) const NO_DATA_CONTROL: &str = "this desktop does not expose clipboard changes to \
     background apps; use the Send clipboard button in the window";

#[cfg(target_os = "linux")]
/// X11 sessions (Pop!_OS and Ubuntu default to Wayland; X11 is out of scope for v1).
pub(super) const X11: &str = "X11 sessions are not supported for clipboard sync in this version";

/// Backend name reported when nothing works.
#[cfg(not(windows))]
pub(super) const BACKEND: &str = "none";

#[cfg(not(windows))]
pub(super) fn unsupported(reason: impl Into<String>) -> ClipboardSupport {
    ClipboardSupport {
        supported: false,
        backend: BACKEND,
        reason: Some(reason.into()),
    }
}

/// The error every entry point returns when the session is unsupported.
pub(super) fn error(support: &ClipboardSupport) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        support
            .reason
            .as_deref()
            .unwrap_or("clipboard sync is not available")
    )
}

#[cfg(not(any(target_os = "linux", windows)))]
mod other {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::super::{ClipboardSupport, LocalClip};

    const REASON: &str = "clipboard sync is not supported on this platform";

    pub(in super::super) fn probe() -> ClipboardSupport {
        super::unsupported(REASON)
    }

    pub(in super::super) struct WatchHandle;

    impl WatchHandle {
        pub(in super::super) fn stop(&self) {}
    }

    pub(in super::super) async fn spawn_watcher(
        _raw: mpsc::Sender<LocalClip>,
        _images: Arc<AtomicBool>,
    ) -> anyhow::Result<WatchHandle> {
        anyhow::bail!(REASON)
    }

    pub(in super::super) fn read_current(_images: bool) -> anyhow::Result<Option<LocalClip>> {
        anyhow::bail!(REASON)
    }

    pub(in super::super) fn apply(_text: &str, _sensitive: bool) -> anyhow::Result<()> {
        anyhow::bail!(REASON)
    }

    pub(in super::super) fn apply_image(
        _bytes: &[u8],
        _mime: &str,
        _sensitive: bool,
    ) -> anyhow::Result<()> {
        anyhow::bail!(REASON)
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
pub(super) use other::{apply, apply_image, probe, read_current, spawn_watcher, WatchHandle};

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_always_has_a_reason() {
        let s = unsupported(NO_DATA_CONTROL);
        assert!(!s.supported);
        assert_eq!(s.backend, "none");
        assert_eq!(error(&s).to_string(), NO_DATA_CONTROL);
    }
}
