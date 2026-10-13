//! Local media players for the `media` frames (Phase 5 task d; protocol.md 5.9).
//!
//! The daemon talks to this module only through the API below:
//!
//! - [`Watcher::start`] watches every player and sends a [`MediaState`] on each change
//!   through the channel; the daemon hands it to `Host::push_media_state`, which already
//!   coalesces to 2 frames per second per player (anti-pattern 10: never relay the raw
//!   signal flood). A player that goes away is reported once more with `playing: false`
//!   and an empty `can` list, which UIs treat as "gone".
//! - [`Watcher::command`] applies a peer's `media` command to one player.
//!
//! Backends, by target:
//!
//! - `linux.rs`: MPRIS2 over the session bus with `zbus`: `org.mpris.MediaPlayer2.*`
//!   names from `ListNames` plus `NameOwnerChanged`, the `Player` interface properties
//!   (`PlaybackStatus`, `Metadata`, `Position`, `Volume`, `CanPlay`, `CanPause`,
//!   `CanGoNext`, `CanGoPrevious`, `CanSeek`, `CanControl`) with `PropertiesChanged` and
//!   `Seeked`; commands through `Play`, `Pause`, `PlayPause`, `Stop`, `Next`, `Previous`,
//!   `SetPosition` (with the track ID from `Metadata`) and the `Volume` property. The
//!   player ID is the bus name without the `org.mpris.MediaPlayer2.` prefix; `name` is
//!   the `Identity` property.
//! - `windows.rs`: `GlobalSystemMediaTransportControlsSessionManager` (`WinRT`), type-checked
//!   only on this machine.
//! - `unsupported.rs`: everything else, always with a sentence saying why (anti-pattern 18).
//!
//! Album art is not sent in v1 (`art` is `None`; it needs a blob per track).
//! Media state goes only to peers with the `media` grant; the core enforces that.

#[cfg(any(windows, test))]
mod gsmtc_model;
#[cfg(any(target_os = "linux", windows))]
mod latest;
#[cfg(target_os = "linux")]
mod linux;
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as backend;
#[cfg(not(any(target_os = "linux", windows)))]
use self::unsupported as backend;
#[cfg(windows)]
use self::windows as backend;

use playdrop_core::host::MediaState;
use playdrop_core::session::types::MediaAction;
use tokio::sync::mpsc;

/// Whether local media players can be watched and controlled on this desktop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSupport {
    /// `true` when [`Watcher::start`] can work.
    pub supported: bool,
    /// Backend name for diagnostics: `mpris`, `gsmtc` or `none`.
    pub backend: &'static str,
    /// A sentence for the user when `supported` is `false`.
    pub reason: Option<String>,
}

/// Probes the platform once and caches the answer.
pub async fn support() -> MediaSupport {
    backend::support().await
}

/// Watches every local player and applies commands to them.
pub struct Watcher {
    inner: backend::Watcher,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching. The current state of every player is sent first, then changes.
    /// A full channel drops the oldest pending state for that player rather than blocking
    /// the bus task.
    pub async fn start(tx: mpsc::Sender<MediaState>) -> anyhow::Result<Self> {
        Ok(Self {
            inner: backend::Watcher::start(tx).await?,
        })
    }

    /// Applies a command to `player` (the ID from its [`MediaState`]). `value` is the seek
    /// position in ms or the volume 0 to 100. Fails with a user-facing sentence when the
    /// player is gone or cannot do it.
    pub async fn command(
        &self,
        player: &str,
        action: MediaAction,
        value: Option<u64>,
    ) -> anyhow::Result<()> {
        self.inner.command(player, action, value).await
    }

    /// Stops watching.
    pub fn stop(self) {
        self.inner.stop();
    }
}
