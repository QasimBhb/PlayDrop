//! `playdropd`: the `PlayDrop` desktop daemon.
//!
//! Hosts `playdrop_core::host::Host`, owns the listening socket (anti-pattern 3: never a
//! window), drains `HostEvent`s and fans them out to UI clients over `playdrop_ipc`, shows
//! the tray and notifications, watches the clipboard where the platform allows it, and
//! installs its own autostart entry. See `docs/ipc.md` and `docs/phases/phase-1-core.md`
//! section 9 task a.
//!
//! Module map (Phase 2 task a fills these in; `clipboard` is task c):
//! - `cli`: command line flags and the settings they override.
//! - `settings`: persisted daemon settings (`settings.json` in the data dir, 0600).
//! - `daemon`: the event loop: `Host` lifecycle, event fan-out, request handling.
//! - `lock`: desktop lock state (logind `LockedHint`, `WTS_SESSION_LOCK`) for SR-6.
//! - `notify`: desktop notifications (freedesktop via `notify-rust`, Windows toasts).
//! - `tray`: status icon (`ksni` on Linux, `tray-icon` on Windows).
//! - `autostart`: XDG autostart `.desktop` and D-Bus service file; Windows Run key.
//! - `probe`: the port self-check behind `Request::Diagnose` (SR-31 wording).
//! - `open`: "Open" and "Show in folder" with the SR-23 executable rule.
//! - `network`: OS network change notifications (`NetworkManager`, `systemd-networkd`).
//! - `clipboard`: watcher and applier per platform, text and PNG images (SR-24).
//! - `battery`: this computer's battery for the `battery` frame (protocol.md 5.8).
//! - `input`: remote input injection (`SendInput`, the `RemoteDesktop` portal) and the key
//!   vocabulary mapping (protocol.md 5.12, SR-25). Phase 5.
//! - `media`: local media players (MPRIS on Linux, GSMTC on Windows) for the `media`
//!   frames (protocol.md 5.9). Phase 5.
//! - `control`: the glue for both: one injector task per controlling peer, the media feed.

mod autostart;
mod battery;
mod cli;
mod clipboard;
mod control;
mod daemon;
mod input;
mod lock;
mod media;
mod network;
mod notify;
mod open;
mod probe;
mod settings;
mod tray;

use std::time::Duration;

use clap::Parser;

/// How long blocking helpers may keep the process alive after the daemon stopped.
///
/// A notification with "Open" / "Show in folder" buttons waits on a blocking thread until
/// it is clicked or dismissed, and a notification centre can keep it forever. Dropping a
/// tokio runtime waits for such threads without limit, so `SIGTERM` logged "stopped" but
/// never exited. Everything that matters (host, IPC, pins) is already shut down by then.
const BLOCKING_GRACE: Duration = Duration::from_secs(2);

fn main() -> anyhow::Result<()> {
    let args = cli::Args::parse();
    cli::init_logging(&args);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = rt.block_on(daemon::run(args));
    rt.shutdown_timeout(BLOCKING_GRACE);
    result
}
