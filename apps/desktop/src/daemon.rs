//! Starting `playdropd` from the not-connected screen, and saying why it did not come up.
//!
//! Looks for the daemon next to this binary first (installed layouts, the Flatpak's
//! `/app/bin`, a `cargo build` target directory), then on `PATH`. The child is detached:
//! the daemon outlives the window. A watcher thread reaps it if it exits while the window is
//! open and keeps its exit status, so the screen can tell "it started and stopped at once"
//! from "it is still coming up" ([`Launch::exited`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex, PoisonError};

/// File name of the daemon binary on this platform.
const DAEMON_BIN: &str = if cfg!(windows) {
    "playdropd.exe"
} else {
    "playdropd"
};

/// The daemon's program name, which names its log file on Windows.
const DAEMON_PROGRAM: &str = "playdropd";

/// Where to run the daemon from: beside this executable if present, else by name on `PATH`.
fn daemon_path() -> PathBuf {
    std::env::current_exe()
        .ok()
        .map(|exe| exe.with_file_name(DAEMON_BIN))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(DAEMON_BIN))
}

/// Where the daemon writes its log: `%LOCALAPPDATA%\PlayDrop\logs\playdropd.log` on
/// Windows, `None` on Linux (it logs to the terminal or the journal there).
pub fn daemon_log() -> Option<PathBuf> {
    playdrop_platform::startup::log_path(DAEMON_PROGRAM)
}

/// Why "Start" could not run the daemon at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnError {
    /// No daemon binary next to the window or on `PATH`.
    Missing,
    /// The binary is there but did not start.
    Failed {
        /// What was run.
        path: PathBuf,
        /// The system's reason.
        error: String,
    },
}

impl fmt::Display for SpawnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => write!(
                f,
                "{DAEMON_BIN} was not found next to this program or on PATH. Reinstall PlayDrop."
            ),
            Self::Failed { path, error } => {
                write!(f, "Could not start {}: {error}", path.display())
            }
        }
    }
}

/// A daemon started by this window.
#[derive(Debug)]
pub struct Launch {
    exit: Arc<Mutex<Option<ExitStatus>>>,
}

impl Launch {
    /// The daemon's exit status once it has exited, `None` while it runs.
    pub fn exited(&self) -> Option<ExitStatus> {
        *self.exit.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Spawns the daemon.
pub fn spawn() -> Result<Launch, SpawnError> {
    spawn_path(&daemon_path())
}

fn spawn_path(path: &Path) -> Result<Launch, SpawnError> {
    tracing::info!("starting daemon: {}", path.display());
    // Its output is not read here: on Windows the daemon writes its own log file, on Linux
    // the user runs it in a terminal to see it (the message says so).
    let mut child = Command::new(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                SpawnError::Missing
            } else {
                SpawnError::Failed {
                    path: path.to_owned(),
                    error: e.to_string(),
                }
            }
        })?;
    let exit = Arc::new(Mutex::new(None));
    let slot = Arc::clone(&exit);
    // Reap the child if it exits while the window is open, so it does not linger as a
    // zombie, and keep the status for the screen; the thread ends with it.
    std::thread::Builder::new()
        .name("playdropd-reaper".to_owned())
        .spawn(move || match child.wait() {
            Ok(status) => {
                tracing::info!("daemon exited: {status}");
                *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(status);
            }
            Err(e) => tracing::warn!("waiting for daemon: {e}"),
        })
        .map_err(|e| SpawnError::Failed {
            path: path.to_owned(),
            error: format!("could not watch the new process: {e}"),
        })?;
    Ok(Launch { exit })
}

/// Where to look for the reason, as the end of a sentence.
fn see_log(log: Option<&Path>) -> String {
    match log {
        Some(path) => format!("The reason is in its log: {}", path.display()),
        None => "Run playdropd in a terminal to see why.".to_owned(),
    }
}

/// The daemon this window started exited before it answered. `code` is its exit code (`None`
/// when a signal ended it).
pub fn exit_reason(code: Option<i32>, log: Option<&Path>) -> String {
    let how = match code {
        Some(c) => format!("The daemon started but stopped at once (exit code {c})."),
        None => "The daemon started but was stopped at once.".to_owned(),
    };
    format!("{how} {}", see_log(log))
}

/// The daemon this window started is still running but never answered.
pub fn no_answer_reason(log: Option<&Path>) -> String {
    format!(
        "The daemon was started but did not answer within 10 seconds. {}",
        see_log(log)
    )
}

/// Shown with "not running" when nothing is known about why: on Windows, where the reason
/// for a daemon that stopped on its own would be. Empty on Linux, as before.
pub fn not_running_hint(log: Option<&Path>) -> String {
    log.map_or_else(String::new, |path| {
        format!(
            "If it stopped by itself, the reason is in its log: {}",
            path.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_daemon_is_named_as_missing() {
        let err = spawn_path(Path::new("/nonexistent/pd-test/playdropd")).unwrap_err();
        assert_eq!(err, SpawnError::Missing);
        assert!(err.to_string().contains("was not found"), "{err}");
        assert!(err.to_string().contains("Reinstall PlayDrop"), "{err}");
    }

    #[test]
    fn a_failed_start_carries_the_error() {
        let err = SpawnError::Failed {
            path: PathBuf::from("/opt/playdropd"),
            error: "Permission denied".to_owned(),
        };
        assert_eq!(
            err.to_string(),
            "Could not start /opt/playdropd: Permission denied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_exit_right_after_start_is_seen_with_its_code() {
        let launch = spawn_path(Path::new("/bin/false")).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let status = loop {
            if let Some(s) = launch.exited() {
                break s;
            }
            assert!(std::time::Instant::now() < deadline, "child never exited");
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(1));
    }

    #[test]
    fn reasons_point_at_the_log_or_the_terminal() {
        let log = Path::new(r"C:\Users\a\AppData\Local\PlayDrop\logs\playdropd.log");
        assert_eq!(
            exit_reason(Some(3), Some(log)),
            format!(
                "The daemon started but stopped at once (exit code 3). The reason is in its log: {}",
                log.display()
            )
        );
        assert_eq!(
            exit_reason(None, None),
            "The daemon started but was stopped at once. Run playdropd in a terminal to see why."
        );
        // Linux keeps the wording it always had.
        assert_eq!(
            no_answer_reason(None),
            "The daemon was started but did not answer within 10 seconds. Run playdropd in a terminal to see why."
        );
        assert!(no_answer_reason(Some(log)).ends_with(&log.display().to_string()));
        assert_eq!(not_running_hint(None), "");
        assert!(not_running_hint(Some(log)).contains(&log.display().to_string()));
    }

    #[test]
    fn the_daemon_log_is_only_named_where_one_is_written() {
        if cfg!(not(windows)) {
            assert!(daemon_log().is_none());
        }
    }
}
