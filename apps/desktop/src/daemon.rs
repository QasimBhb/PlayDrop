//! Starting `playdropd` from the not-connected screen.
//!
//! Looks for the daemon next to this binary first (installed layouts, the Flatpak's
//! `/app/bin`, a `cargo build` target directory), then on `PATH`. The child is detached:
//! the daemon outlives the window.

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// File name of the daemon binary on this platform.
const DAEMON_BIN: &str = if cfg!(windows) {
    "playdropd.exe"
} else {
    "playdropd"
};

/// Where to run the daemon from: beside this executable if present, else by name on `PATH`.
fn daemon_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(DAEMON_BIN))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(DAEMON_BIN))
}

/// Spawns the daemon. Returns a message for the user on failure.
pub fn spawn() -> Result<(), String> {
    let path = daemon_path();
    tracing::info!("starting daemon: {}", path.display());
    let mut child = Command::new(&path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", path.display()))?;
    // Reap the child if it exits while the window is open, so it does not linger as a
    // zombie; the thread ends with it.
    std::thread::Builder::new()
        .name("playdropd-reaper".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) => tracing::info!("daemon exited: {status}"),
            Err(e) => tracing::warn!("waiting for daemon: {e}"),
        })
        .map_err(|e| format!("Could not watch the daemon process: {e}"))?;
    Ok(())
}
