//! Finding and starting the other `PlayDrop` programs: the window and, when D-Bus activation
//! is not installed, the daemon.
//!
//! The applet is a host binary only (applets run beside the panel, not in a sandbox), so it
//! looks next to its own executable first, then on `PATH`, like the daemon's tray does.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The window binary.
pub const WINDOW_BIN: &str = "playdrop-desktop";
/// The daemon binary.
pub const DAEMON_BIN: &str = "playdropd";

/// `name` next to `exe`, else the first match in `path` (a `PATH`-style list).
pub fn find_in(name: &str, exe: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let sibling = exe
        .and_then(Path::parent)
        .map(|d| d.join(name))
        .filter(|p| p.is_file());
    sibling.or_else(|| {
        // A relative entry (an empty one means the current folder) would resolve against
        // wherever the panel started the applet: ignore it.
        std::env::split_paths(path?)
            .filter(|d| d.is_absolute())
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// `name` next to this executable or on `PATH`. Looked up each time it is needed, so an
/// install made while the applet runs is found.
pub fn find(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let path = std::env::var_os("PATH");
    find_in(name, exe.as_deref(), path.as_deref())
}

/// Starts `program` detached (double fork, so it outlives the applet and leaves no zombie),
/// passing the Wayland activation token when there is one so the window gets focus.
pub async fn spawn(program: PathBuf, token: Option<String>) -> bool {
    let mut cmd = std::process::Command::new(program);
    if let Some(token) = token {
        cmd.env("XDG_ACTIVATION_TOKEN", &token);
        cmd.env("DESKTOP_STARTUP_ID", &token);
    }
    cosmic::process::spawn(cmd).await.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_the_sibling_then_path() {
        let dir = std::env::temp_dir().join(format!("pd-applet-launch-{}", std::process::id()));
        let bin = dir.join("bin");
        let other = dir.join("other");
        std::fs::create_dir_all(&bin).expect("mkdir");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(other.join("tool"), b"").expect("write");
        let exe = bin.join("playdrop-cosmic-applet");
        let path = std::env::join_paths([&other]).expect("path");

        assert_eq!(
            find_in("tool", Some(&exe), Some(&path)),
            Some(other.join("tool"))
        );
        std::fs::write(bin.join("tool"), b"").expect("write");
        assert_eq!(
            find_in("tool", Some(&exe), Some(&path)),
            Some(bin.join("tool"))
        );
        assert_eq!(find_in("missing", Some(&exe), Some(&path)), None);
        assert_eq!(find_in("missing", None, None), None);
        // A directory with the name is not a program.
        std::fs::create_dir_all(other.join("dir-tool")).expect("mkdir");
        assert_eq!(find_in("dir-tool", None, Some(&path)), None);
        // Relative entries are ignored even when they would match from the current folder.
        let rel = std::env::join_paths([std::path::Path::new("."), std::path::Path::new("")])
            .expect("path");
        let cwd_tool = std::env::current_dir().expect("cwd").join("Cargo.toml");
        assert!(cwd_tool.is_file(), "tests run from the crate folder");
        assert_eq!(find_in("Cargo.toml", None, Some(&rel)), None);
        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
