//! Desktops with no media backend, with the reason shown to the user (anti-pattern 18).

use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use tokio::sync::mpsc;

use super::MediaSupport;

/// Backend name reported when nothing works.
pub(super) const BACKEND: &str = "none";

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
pub(super) const NO_BACKEND: &str = "media control is not supported on this platform";

pub(super) fn unsupported(reason: impl Into<String>) -> MediaSupport {
    MediaSupport {
        supported: false,
        backend: BACKEND,
        reason: Some(reason.into()),
    }
}

/// The error every entry point returns when the desktop is unsupported.
pub(super) fn error(support: &MediaSupport) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        support
            .reason
            .as_deref()
            .unwrap_or("media control is not available")
    )
}

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
pub(super) struct Watcher;

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
impl Watcher {
    pub(super) async fn start(_tx: mpsc::Sender<MediaState>) -> anyhow::Result<Self> {
        Err(error(&support().await))
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the real backends"
    )]
    pub(super) async fn command(
        &self,
        _player: &str,
        _action: MediaAction,
        _value: Option<u64>,
    ) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(NO_BACKEND))
    }

    #[allow(clippy::unused_self, reason = "same signature as the real backends")]
    pub(super) fn stop(self) {}
}

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
#[allow(clippy::unused_async, reason = "same signature as the real backends")]
pub(super) async fn support() -> MediaSupport {
    unsupported(NO_BACKEND)
}
