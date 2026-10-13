//! Desktops where remote input cannot be applied, with the reason shown to the user
//! (anti-pattern 18: say why). Used whole on targets with no backend, and for the reasons
//! and the error helper on Linux and Windows.

use std::path::Path;

use playdrop_core::session::types::Input;

use super::InputSupport;

/// Backend name reported when nothing works.
pub(super) const BACKEND: &str = "none";

/// Targets with no injection backend at all.
#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
pub(super) const NO_BACKEND: &str = "remote input is not supported on this platform";

pub(super) fn unsupported(reason: impl Into<String>) -> InputSupport {
    InputSupport {
        supported: false,
        backend: BACKEND,
        reason: Some(reason.into()),
    }
}

/// The error every entry point returns when the desktop is unsupported.
pub(super) fn error(support: &InputSupport) -> anyhow::Error {
    anyhow::anyhow!(
        "{}",
        support
            .reason
            .as_deref()
            .unwrap_or("remote input is not available")
    )
}

/// The no-op backend: `support()` says why, `start` fails with the same sentence.
#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
pub(super) struct Injector;

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
impl Injector {
    pub(super) async fn start(_data_dir: &Path, _peer_label: &str) -> anyhow::Result<Self> {
        Err(error(&support().await))
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the real backends"
    )]
    pub(super) async fn apply(&mut self, _input: &Input) -> anyhow::Result<()> {
        Err(anyhow::anyhow!(NO_BACKEND))
    }

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the real backends"
    )]
    pub(super) async fn release_all(&mut self) {}

    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "same signature as the real backends"
    )]
    pub(super) async fn close(self) {}
}

#[cfg_attr(any(target_os = "linux", windows), allow(dead_code))]
#[allow(
    clippy::unused_async,
    clippy::unused_async_trait_impl,
    reason = "same signature as the real backends"
)]
pub(super) async fn support() -> InputSupport {
    unsupported(NO_BACKEND)
}
