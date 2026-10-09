//! "Open" and "Show in folder" for received files (`docs/ipc.md` section 5, SR-23).
//!
//! Both refuse any path that does not resolve (after following symlinks) to somewhere
//! under the receive folder, so a UI or a notification action can never be used to launch
//! arbitrary files. "Open" applies the SR-23 rule: an executable, a script, a `.desktop`
//! launcher, a `.lnk` shortcut or an installer opens its containing folder instead, so a
//! received file is never run by a single click.
//!
//! "Show in folder" only reveals the item in the file manager, which never runs it, so the
//! SR-23 rule does not change what it does.

use std::path::{Path, PathBuf};

/// Extensions that open their folder instead of themselves (SR-23). Compared without
/// regard to case.
pub const RISKY_EXTENSIONS: &[&str] = &[
    "exe", "msi", "bat", "cmd", "ps1", "sh", "desktop", "lnk", "com", "scr", "vbs", "js", "jar",
    "appimage", "run",
];

/// Why a path was not opened.
#[derive(Debug, thiserror::Error)]
pub enum OpenError {
    /// The path does not exist (or the receive folder does not).
    #[error("{0}")]
    NotFound(String),
    /// The path resolves outside the receive folder.
    #[error("only files in the receive folder can be opened")]
    Outside,
    /// The opener could not be started.
    #[error("could not open: {0}")]
    Launch(String),
}

/// What [`open_received`] actually opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// The file itself, with the default application.
    File(PathBuf),
    /// Its folder: the path was a folder, or the file matched the SR-23 rule.
    Folder(PathBuf),
}

/// Resolves `path` and checks it lies under `receive_dir` (the receive folder itself
/// counts). Returns the canonical path.
pub fn contained(path: &Path, receive_dir: &Path) -> Result<PathBuf, OpenError> {
    let p = std::fs::canonicalize(path).map_err(|e| OpenError::NotFound(e.to_string()))?;
    // No receive folder yet means nothing can be inside it.
    let root = std::fs::canonicalize(receive_dir).map_err(|_| OpenError::Outside)?;
    if p.starts_with(&root) {
        Ok(p)
    } else {
        Err(OpenError::Outside)
    }
}

/// The SR-23 rule: true for a regular file that is executable (any execute bit on Unix) or
/// has one of [`RISKY_EXTENSIONS`].
pub fn is_risky(path: &Path, meta: &std::fs::Metadata) -> bool {
    if !meta.is_file() {
        return false;
    }
    if has_risky_extension(path) {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if meta.permissions().mode() & 0o111 != 0 {
            return true;
        }
    }
    false
}

/// True if the file name ends in one of [`RISKY_EXTENSIONS`].
pub fn has_risky_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RISKY_EXTENSIONS.iter().any(|r| e.eq_ignore_ascii_case(r)))
}

/// Decides what "Open" should open for a path already checked by [`contained`].
pub fn open_target(canonical: &Path) -> Result<Opened, OpenError> {
    let meta = std::fs::metadata(canonical).map_err(|e| OpenError::NotFound(e.to_string()))?;
    if meta.is_dir() {
        return Ok(Opened::Folder(canonical.to_path_buf()));
    }
    if is_risky(canonical, &meta) {
        let parent = canonical
            .parent()
            .map_or_else(|| canonical.to_path_buf(), Path::to_path_buf);
        return Ok(Opened::Folder(parent));
    }
    Ok(Opened::File(canonical.to_path_buf()))
}

/// Opens a received file with the default application, or its folder under the SR-23 rule.
pub fn open_received(path: &Path, receive_dir: &Path) -> Result<Opened, OpenError> {
    let canonical = contained(path, receive_dir)?;
    let target = open_target(&canonical)?;
    let p = match &target {
        Opened::File(p) | Opened::Folder(p) => p.clone(),
    };
    tracing::debug!(path = %p.display(), "opening received item");
    launch(&p)?;
    Ok(target)
}

/// Reveals a received file in the file manager.
pub async fn show_in_folder(path: &Path, receive_dir: &Path) -> Result<(), OpenError> {
    let canonical = contained(path, receive_dir)?;
    tracing::debug!(path = %canonical.display(), "revealing received item");
    platform::reveal(&canonical).await
}

/// Hands a path to the desktop's default opener without waiting for it.
fn launch(path: &Path) -> Result<(), OpenError> {
    open::that_detached(path).map_err(|e| OpenError::Launch(e.to_string()))
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::Path;

    use super::OpenError;

    #[zbus::proxy(
        interface = "org.freedesktop.FileManager1",
        default_service = "org.freedesktop.FileManager1",
        default_path = "/org/freedesktop/FileManager1",
        gen_blocking = false
    )]
    trait FileManager {
        fn show_items(&self, uris: &[&str], startup_id: &str) -> zbus::Result<()>;
    }

    /// `org.freedesktop.FileManager1.ShowItems` selects the file in its folder; without a
    /// file manager on the bus, open the folder instead.
    pub(super) async fn reveal(path: &Path) -> Result<(), OpenError> {
        match show_items(path).await {
            Ok(()) => Ok(()),
            Err(e) => {
                tracing::debug!(error = %e, "FileManager1.ShowItems unavailable; opening the folder");
                let folder = if path.is_dir() {
                    path
                } else {
                    path.parent().unwrap_or(path)
                };
                super::launch(folder)
            }
        }
    }

    async fn show_items(path: &Path) -> zbus::Result<()> {
        let conn = zbus::Connection::session().await?;
        let fm = FileManagerProxy::new(&conn).await?;
        let uri = super::file_uri(path);
        fm.show_items(&[uri.as_str()], "").await
    }
}

#[cfg(windows)]
mod platform {
    use std::os::windows::process::CommandExt as _;
    use std::path::Path;

    use super::OpenError;

    /// `explorer /select,"<path>"`. The argument is passed raw because Explorer parses its
    /// command line itself and does not accept the quoting std would add.
    #[allow(clippy::unused_async)] // same signature as the Linux version, which awaits
    pub(super) async fn reveal(path: &Path) -> Result<(), OpenError> {
        let arg = format!("/select,\"{}\"", path.display());
        std::process::Command::new("explorer")
            .raw_arg(arg)
            .spawn()
            .map(drop)
            .map_err(|e| OpenError::Launch(e.to_string()))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use std::path::Path;

    use super::OpenError;

    #[allow(clippy::unused_async)] // same signature as the Linux version, which awaits
    pub(super) async fn reveal(path: &Path) -> Result<(), OpenError> {
        super::launch(path.parent().unwrap_or(path))
    }
}

/// A `file://` URI for an absolute path, percent-encoding every byte outside the RFC 3986
/// unreserved set and `/`.
#[cfg(any(target_os = "linux", test))]
pub fn file_uri(path: &Path) -> String {
    use std::fmt::Write as _;
    #[cfg(unix)]
    let bytes: Vec<u8> = {
        use std::os::unix::ffi::OsStrExt as _;
        path.as_os_str().as_bytes().to_vec()
    };
    #[cfg(not(unix))]
    let bytes: Vec<u8> = path.to_string_lossy().into_owned().into_bytes();
    let mut out = String::from("file://");
    for b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn risky_extensions_ignore_case() {
        for name in [
            "setup.EXE",
            "a.msi",
            "x.Desktop",
            "run.sh",
            "tool.AppImage",
            "l.lnk",
        ] {
            assert!(has_risky_extension(Path::new(name)), "{name}");
        }
        for name in ["photo.jpg", "notes.txt", "archive.tar.gz", "noext", "sh"] {
            assert!(!has_risky_extension(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn sr23_opens_the_folder_for_risky_files() {
        let tmp = tempfile::tempdir().unwrap();
        let recv = tmp.path().join("in");
        std::fs::create_dir_all(recv.join("sub")).unwrap();
        let photo = recv.join("sub/photo.jpg");
        let installer = recv.join("sub/setup.exe");
        std::fs::write(&photo, b"jpg").unwrap();
        std::fs::write(&installer, b"MZ").unwrap();

        let p = contained(&photo, &recv).unwrap();
        assert_eq!(open_target(&p).unwrap(), Opened::File(p.clone()));
        let i = contained(&installer, &recv).unwrap();
        assert_eq!(
            open_target(&i).unwrap(),
            Opened::Folder(i.parent().unwrap().to_path_buf())
        );
        let d = contained(&recv.join("sub"), &recv).unwrap();
        assert_eq!(open_target(&d).unwrap(), Opened::Folder(d.clone()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let script = recv.join("sub/tool");
            std::fs::write(&script, b"#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let s = contained(&script, &recv).unwrap();
            assert!(matches!(open_target(&s).unwrap(), Opened::Folder(_)));
        }
    }

    #[test]
    fn paths_outside_the_receive_folder_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let recv = tmp.path().join("in");
        std::fs::create_dir_all(&recv).unwrap();
        let outside = tmp.path().join("secret.txt");
        std::fs::write(&outside, b"x").unwrap();

        assert!(matches!(
            contained(&outside, &recv),
            Err(OpenError::Outside)
        ));
        assert!(matches!(
            contained(&recv.join("../secret.txt"), &recv),
            Err(OpenError::Outside)
        ));
        assert!(matches!(
            contained(&recv.join("missing.txt"), &recv),
            Err(OpenError::NotFound(_))
        ));
        // A prefix that is not a path component does not count as inside.
        let sibling = tmp.path().join("in-other");
        std::fs::create_dir_all(&sibling).unwrap();
        std::fs::write(sibling.join("f"), b"x").unwrap();
        assert!(matches!(
            contained(&sibling.join("f"), &recv),
            Err(OpenError::Outside)
        ));

        #[cfg(unix)]
        {
            let link = recv.join("link.txt");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(matches!(contained(&link, &recv), Err(OpenError::Outside)));
        }
    }

    #[test]
    fn file_uris_are_percent_encoded() {
        assert_eq!(
            file_uri(Path::new("/home/u/Downloads/My file #1.txt")),
            "file:///home/u/Downloads/My%20file%20%231.txt"
        );
        assert_eq!(file_uri(Path::new("/a/é")), "file:///a/%C3%A9");
    }
}
