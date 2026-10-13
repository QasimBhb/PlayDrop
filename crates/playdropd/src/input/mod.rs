//! Remote input injection (Phase 5 tasks a and b; protocol.md 5.12, SR-25, T29 to T31).
//!
//! The daemon talks to this module only through the API below. The core has already
//! enforced the grant, the input session, the lock state and the 60 keys per second cap
//! before a frame reaches [`Injector::apply`]; this module turns the protocol vocabulary
//! into OS input and never applies anything on its own.
//!
//! Backends, picked by target and, on Linux, by a one-time runtime probe:
//!
//! - `linux.rs`: the `org.freedesktop.portal.RemoteDesktop` portal over `zbus`
//!   (COSMIC, GNOME, KDE). One portal session per [`Injector`], device types keyboard and
//!   pointer, `persist_mode` "until revoked" with the restore token kept under the data
//!   dir (0600) so the consent dialog appears once, not on every input session. Input goes
//!   through the portal's `NotifyPointerMotion`, `NotifyPointerButton`, `NotifyPointerAxis`
//!   and `NotifyKeyboardKeysym` calls; keys are X11 keysyms (`xkeysym`) so the compositor
//!   maps them through the user's layout. `ConnectToEIS` plus `reis` is the faster path
//!   for a later phase; the portal calls are enough for a trackpad coalesced to 8 ms.
//! - `windows.rs`: `SendInput` with `MOUSEEVENTF_MOVE`, `MOUSEEVENTF_WHEEL`, virtual-key
//!   codes for named keys and `KEYEVENTF_UNICODE` for text. The daemon never runs
//!   elevated or with `uiAccess`, so UAC and elevated windows are out of reach by
//!   construction (SR-25, T30).
//! - `unsupported.rs`: everything else, always with a sentence saying why (anti-pattern
//!   18): X11 sessions, a desktop without the `RemoteDesktop` portal, a portal whose
//!   `AvailableDeviceTypes` lacks keyboard or pointer.
//!
//! `keys.rs` holds the platform-free vocabulary mapping (W3C UI Events `key` names and
//! modifiers to keysyms and virtual keys) with its tests; it compiles on every target.
//! `plan.rs` expands a frame into press, release, move and scroll steps and keeps the
//! held state, once for both backends; `sendinput.rs` turns those steps into `SendInput`
//! records and is compiled on Windows and in tests everywhere.
//!
//! Rules every backend follows:
//!
//! - **Release on end (T31).** [`Injector::release_all`] releases every key, modifier and
//!   button this injector pressed and is called on `session`/`stop`, on drop of the
//!   connection, on lock, and from the kill switch. [`Injector::close`] releases first.
//! - **Modifiers** are pressed before the key and released after it, in order; held
//!   state is tracked per injector so `release_all` knows what to undo.
//! - **Presenter actions** map to keys: `next` is Right, `previous` is Left, `blackout`
//!   is `b` (the usual slideshow key), `pointerOn` and `pointerOff` are ignored here
//!   (no OS-level laser pointer; a later phase may draw one).
//! - **SR-32.** Input events are never logged: debug lines carry kinds and counts only.

#[cfg(target_os = "linux")]
mod linux;
mod plan;
#[cfg(any(windows, test))]
mod sendinput;
mod unsupported;
#[cfg(windows)]
mod windows;

pub mod keys;

#[cfg(target_os = "linux")]
use self::linux as backend;
#[cfg(not(any(target_os = "linux", windows)))]
use self::unsupported as backend;
#[cfg(windows)]
use self::windows as backend;

use std::path::Path;

use playdrop_core::session::types::Input;

/// Whether remote input works on this desktop, and why not when it does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputSupport {
    /// `true` when [`Injector::start`] can work.
    pub supported: bool,
    /// Backend name for diagnostics: `remote-desktop-portal`, `send-input` or `none`.
    pub backend: &'static str,
    /// A sentence for the user when `supported` is `false`.
    pub reason: Option<String>,
}

/// Probes the platform once and caches the answer. Never blocks for long: on Linux it asks
/// the portal for its version and `AvailableDeviceTypes` with a short timeout and treats a
/// missing portal as unsupported with the reason naming it.
pub async fn support() -> InputSupport {
    backend::support().await
}

/// One remote input session's injector: created on `session`/`start` from a peer and
/// closed on `stop`. Holds the portal session (Linux) or nothing (Windows) plus the set
/// of keys and buttons currently pressed through it.
pub struct Injector {
    inner: backend::Injector,
}

impl std::fmt::Debug for Injector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Injector").finish_non_exhaustive()
    }
}

impl Injector {
    /// Opens an injector for input from the peer labelled `peer_label`. On Linux this
    /// creates and starts a portal session; the first time, the desktop shows its consent
    /// dialog and this call waits for the answer (the daemon tells the user to look at the
    /// screen if it takes more than a few seconds). `data_dir` holds the portal restore
    /// token between runs. Fails with the user-facing reason when the platform refuses.
    pub async fn start(data_dir: &Path, peer_label: &str) -> anyhow::Result<Self> {
        Ok(Self {
            inner: backend::Injector::start(data_dir, peer_label).await?,
        })
    }

    /// Applies one frame. `session` frames are not for this method (the daemon opens and
    /// closes injectors itself) and are ignored.
    pub async fn apply(&mut self, input: &Input) -> anyhow::Result<()> {
        self.inner.apply(input).await
    }

    /// Releases every key, modifier and button this injector pressed (T31).
    pub async fn release_all(&mut self) {
        self.inner.release_all().await;
    }

    /// Releases everything and ends the platform session.
    pub async fn close(mut self) {
        self.inner.release_all().await;
        self.inner.close().await;
    }
}
