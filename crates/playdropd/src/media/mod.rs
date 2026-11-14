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
//! On Linux and Windows the computer's own output volume is one more entry, the reserved
//! player `@system` (protocol.md 5.9): never playing, it can do `volume` and `mute`, and it
//! reports the level also while muted. `system_volume_model.rs` holds what both share (the
//! entry, the worker loop, folding a drag into one change); `system_volume.rs` reads the
//! default sink through the `PulseAudio` protocol (`PipeWire` or `PulseAudio`), and
//! `system_volume_windows.rs` the default render endpoint through Core Audio
//! (`IAudioEndpointVolume`, with change and default-device notices), type-checked only on
//! this machine.
//!
//! Album art is not sent in v1 (`art` is `None`; it needs a blob per track).
//! Media state goes only to peers with the `media` grant; the core enforces that.

#[cfg(any(windows, test))]
mod gsmtc_model;
#[cfg(any(target_os = "linux", windows))]
mod latest;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
mod system_volume;
#[cfg(any(target_os = "linux", windows))]
mod system_volume_model;
#[cfg(windows)]
mod system_volume_windows;
mod unsupported;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
use self::linux as backend;
#[cfg(not(any(target_os = "linux", windows)))]
use self::unsupported as backend;
#[cfg(windows)]
use self::windows as backend;

#[cfg(target_os = "linux")]
use self::system_volume as system_backend;
#[cfg(windows)]
use self::system_volume_windows as system_backend;

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

/// Whether this desktop offers the computer's own volume (`@system`). It does not depend
/// on the player backend: the daemon starts a [`Watcher`] when either this or
/// [`MediaSupport::supported`] is true.
pub const fn volume_supported() -> bool {
    cfg!(any(target_os = "linux", windows))
}

/// Watches every local player and the computer's volume, and applies commands to them.
pub struct Watcher {
    /// The player backend, or the sentence saying why players cannot be watched.
    inner: Result<backend::Watcher, String>,
    /// The computer's own volume, as the reserved player `@system`.
    #[cfg(any(target_os = "linux", windows))]
    system: system_volume_model::SystemVolume,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher").finish_non_exhaustive()
    }
}

impl Watcher {
    /// Starts watching. The current state of every player is sent first, then changes.
    /// A full channel drops the oldest pending state for that player rather than blocking
    /// the bus task. Never fails: when the player backend cannot start, the computer's
    /// volume still works and player commands answer with the backend's reason.
    pub async fn start(tx: mpsc::Sender<MediaState>) -> Self {
        #[cfg(any(target_os = "linux", windows))]
        let system = system_backend::start(tx.clone());
        let inner = backend::Watcher::start(tx).await.map_err(|e| {
            tracing::warn!(error = %e, "media players are not watched");
            e.to_string()
        });
        Self {
            inner,
            #[cfg(any(target_os = "linux", windows))]
            system,
        }
    }

    /// Sends the current state of every player and of the computer's volume again, also
    /// when nothing changed: a host started after the watcher knows none of them.
    pub fn republish(&self) {
        #[cfg(any(target_os = "linux", windows))]
        self.system.republish();
        if let Ok(inner) = &self.inner {
            inner.republish();
        }
    }

    /// Applies a command to `player` (the ID from its [`MediaState`]). `value` is the seek
    /// position in ms, the volume 0 to 100, or for `mute` `1` (mute) or `0` (unmute). Fails
    /// with a user-facing sentence when the player is gone or cannot do it.
    pub async fn command(
        &self,
        player: &str,
        action: MediaAction,
        value: Option<u64>,
    ) -> anyhow::Result<()> {
        #[cfg(any(target_os = "linux", windows))]
        if player == system_volume_model::SYSTEM_PLAYER {
            return self.system.command(&action, value);
        }
        match &self.inner {
            Ok(inner) => inner.command(player, action, value).await,
            Err(reason) => Err(anyhow::anyhow!("{reason}")),
        }
    }

    /// Stops watching.
    pub fn stop(self) {
        if let Ok(inner) = self.inner {
            inner.stop();
        }
    }
}
