//! Blocking filesystem helpers for the receive side: beneath-only directory walks that never
//! follow symlinks, exclusive file creation with " (n)" collision names, owner-only
//! permissions and atomic small-file writes (SR-18, SR-23).
//!
//! These use `std::fs` only. A planted symlink is refused by checking every component with
//! `symlink_metadata` and by creating files with `create_new` (which fails on any existing
//! name, symlinks included). A local attacker racing between check and use is out of reach
//! of `std`; `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` through `cap-std` or `rustix`
//! closes that gap.
//!
//! TODO(SR-18): walk and create through `rustix::fs::openat2` with `RESOLVE_BENEATH |
//! RESOLVE_NO_SYMLINKS` on Linux (rustix is now a dependency) to close the check-then-use
//! window.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Most " (n)" suffixes tried before giving up.
const MAX_COLLISIONS: u32 = 10_000;

/// Creates the receive root owner-only (0700 on Unix, SR-23) if it does not exist. An
/// existing root is used as is; it may itself be a symlink the user chose.
pub(crate) fn ensure_root(root: &Path) -> io::Result<()> {
    match std::fs::metadata(root) {
        Ok(m) if m.is_dir() => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "receive root is not a directory",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => private_dir_builder(true).create(root),
        Err(e) => Err(e),
    }
}

fn private_dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(recursive);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b
}

/// Walks `components` beneath `root`, creating missing directories, and refuses any
/// component that is a symlink or not a directory (SR-18). Returns the final directory.
pub(crate) fn safe_dir(root: &Path, components: &[String], private: bool) -> Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for c in components {
        dir.push(c);
        loop {
            match std::fs::symlink_metadata(&dir) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::PathInvalid(format!(
                        "{} is a symlink; not followed",
                        dir.display()
                    )))
                }
                Ok(m) if m.is_dir() => break,
                Ok(_) => {
                    return Err(Error::PathInvalid(format!(
                        "{} exists and is not a folder",
                        dir.display()
                    )))
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    let mut b = if private {
                        private_dir_builder(false)
                    } else {
                        std::fs::DirBuilder::new()
                    };
                    match b.recursive(false).create(&dir) {
                        // Re-check on the next loop turn: a racing creator may have won.
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    Ok(dir)
}

/// Opens options for a new owner-only, never-executable file (0600 on Unix, SR-23).
pub(crate) fn private_file_options() -> OpenOptions {
    let mut o = OpenOptions::new();
    o.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Splits `name` into stem and extension for collision names: `a.tar.gz` gives
/// (`a.tar`, `.gz`); `.bashrc` and `noext` have no extension.
fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(0) | None => (name, ""),
        Some(i) => name.split_at(i),
    }
}

/// Name for collision number `n`: `photo.jpg` becomes `photo (n).jpg`, shortening the stem
/// on a character boundary if needed to stay within 255 bytes.
pub(crate) fn collision_name(name: &str, n: u32) -> String {
    if n == 0 {
        return name.to_owned();
    }
    let (stem, ext) = split_ext(name);
    let suffix = format!(" ({n})");
    let budget = 255usize.saturating_sub(suffix.len() + ext.len());
    let mut cut = stem.len().min(budget);
    while !stem.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}{suffix}{ext}", &stem[..cut])
}

/// Exclusively creates `dir/name`, or `name (1)`, `name (2)`, ... on collision; never
/// overwrites and never follows a symlink at the final name (SR-18).
pub(crate) fn create_unique(dir: &Path, name: &str) -> Result<(PathBuf, File)> {
    for n in 0..MAX_COLLISIONS {
        let candidate = dir.join(collision_name(name, n));
        match private_file_options().create_new(true).open(&candidate) {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(map_io(e)),
        }
    }
    Err(Error::PathInvalid(format!(
        "{name}: too many existing files with this name"
    )))
}

/// Opens an existing regular file for writing without following a symlink: the path must
/// not be a symlink, and the opened handle must be the same file (SR-18).
pub(crate) fn open_existing_regular(path: &Path) -> Result<File> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.file_type().is_file() {
        return Err(Error::PathInvalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    let f = private_file_options().open(path).map_err(map_io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let after = f.metadata()?;
        if after.dev() != before.dev() || after.ino() != before.ino() {
            return Err(Error::PathInvalid(format!(
                "{} changed while opening",
                path.display()
            )));
        }
    }
    Ok(f)
}

/// Opens `path` for writing, creating it owner-only if missing, refusing symlinks.
pub(crate) fn open_or_create(path: &Path) -> Result<File> {
    match private_file_options().create_new(true).open(path) {
        Ok(f) => Ok(f),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => open_existing_regular(path),
        Err(e) => Err(map_io(e)),
    }
}

/// Writes `bytes` to `path` atomically: temp file, `sync_all`, rename, then a directory
/// sync on Unix so the rename itself is durable (protocol.md 7.6 durability).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut o = private_file_options();
        o.create(true).truncate(true);
        if std::fs::symlink_metadata(&tmp).is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&tmp)?;
        }
        let mut f = o.open(&tmp).map_err(map_io)?;
        f.write_all(bytes).map_err(map_io)?;
        f.sync_all().map_err(map_io)?;
    }
    std::fs::rename(&tmp, path).map_err(map_io)?;
    if let Some(parent) = path.parent() {
        sync_dir(parent)?;
    }
    Ok(())
}

/// Flushes a directory's entries (Unix). A no-op elsewhere, where directories cannot be
/// opened as files through `std`.
pub(crate) fn sync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)?.sync_all().map_err(map_io)?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Removes a file, treating "already gone" as success.
pub(crate) fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Maps a full disk to [`Error::NoSpace`] (SR-20 "ENOSPC fails cleanly").
pub(crate) fn map_io(e: io::Error) -> Error {
    if e.kind() == io::ErrorKind::StorageFull {
        Error::NoSpace
    } else {
        Error::Io(e)
    }
}

/// Makes the data of many staged files durable at once (protocol.md 7.6). On Linux one
/// `syncfs` on the filesystem holding `dir` replaces an `fdatasync` per file, which keeps
/// thousands of small bundle members from costing thousands of flushes.
pub(crate) fn sync_files(dir: &Path, files: &[PathBuf]) -> Result<()> {
    #[cfg(target_os = "linux")]
    if files.len() > 4 {
        let d = File::open(dir)?;
        return rustix::fs::syncfs(&d).map_err(|e| map_io(io::Error::from(e)));
    }
    let _ = dir;
    for f in files {
        File::open(f)?.sync_data().map_err(map_io)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_names() {
        assert_eq!(collision_name("photo.jpg", 0), "photo.jpg");
        assert_eq!(collision_name("photo.jpg", 2), "photo (2).jpg");
        assert_eq!(collision_name("a.tar.gz", 1), "a.tar (1).gz");
        assert_eq!(collision_name(".bashrc", 1), ".bashrc (1)");
        assert_eq!(collision_name("noext", 3), "noext (3)");
        let long = format!("{}.txt", "\u{e9}".repeat(125)); // 250 + 4 bytes
        let named = collision_name(&long, 12);
        assert!(named.len() <= 255);
        assert!(named.ends_with(" (12).txt"));
    }

    #[test]
    fn create_unique_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"keep").unwrap();
        let (p, _) = create_unique(dir.path(), "a.txt").unwrap();
        assert_eq!(p.file_name().unwrap(), "a (1).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"keep");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("bashrc");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sub")).unwrap();
        assert!(matches!(
            safe_dir(dir.path(), &["sub".into()], false),
            Err(Error::PathInvalid(_))
        ));
        std::os::unix::fs::symlink(&target, dir.path().join("x")).unwrap();
        let (p, _) = create_unique(dir.path(), "x").unwrap();
        assert_eq!(p.file_name().unwrap(), "x (1)");
        assert!(open_existing_regular(&dir.path().join("x")).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
    }
}
