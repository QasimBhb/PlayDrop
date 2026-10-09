//! Blocking filesystem helpers for the receive side: beneath-only directory walks that never
//! follow symlinks, exclusive file creation with " (n)" collision names, owner-only
//! permissions and atomic small-file writes (SR-18, SR-23).
//!
//! Everything a peer's manifest names is reached through directory handles, never through
//! a re-resolved path (SR-18). The receive root is opened once as a `cap_std::fs::Dir`;
//! each manifest folder is then opened one component at a time relative to its parent:
//!
//! - On Linux with `openat2(RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)`, so the kernel refuses
//!   any symlink and any escape at the moment of the open: no check-then-use window.
//! - Elsewhere (and on Linux kernels before 5.6, or under a seccomp filter that hides
//!   `openat2`) through `cap-std`, which keeps every open beneath the root, after refusing a
//!   component that is a symlink. A race there can at worst redirect within the root.
//!
//! Final names are created with `create_new` relative to the folder handle (fails on any
//! existing name, symlinks included) and the staged file is renamed onto that name with
//! `renameat` between the two handles.
//!
//! The staging folder `<root>/.playdrop-partial/<id>` is created the same way, owner-only;
//! files inside it (`.part`, ledgers) are then used by path, since nothing in it is named
//! by a peer and only this user can write there (SR-23).

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use cap_std::fs::Dir;

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

/// Opens the receive root as a directory handle, creating it owner-only first if needed
/// (SR-23). The root itself may be a symlink the user chose; nothing beneath it may be.
pub(crate) fn open_root(root: &Path) -> Result<Dir> {
    ensure_root(root)?;
    Ok(Dir::open_ambient_dir(root, cap_std::ambient_authority())?)
}

/// Refuses a component that is not one plain name. Manifest validation already does this
/// (protocol.md 7.2); this keeps the walk safe on its own.
fn check_component(c: &str) -> Result<()> {
    if c.is_empty() || c == "." || c == ".." || c.contains(['/', '\\', '\0']) {
        return Err(Error::PathInvalid(format!("{c:?} is not a plain name")));
    }
    Ok(())
}

/// Opens the folder `name` directly inside `parent`, never following a symlink (SR-18).
fn open_child_dir(parent: &Dir, name: &str) -> io::Result<Dir> {
    #[cfg(target_os = "linux")]
    {
        use rustix::fs::{openat2, Mode, OFlags, ResolveFlags};
        let opened = openat2(
            parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH | ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        );
        match opened {
            Ok(fd) => return Ok(Dir::from(fd)),
            // No openat2 here (old kernel, seccomp): use the portable path below.
            Err(rustix::io::Errno::NOSYS | rustix::io::Errno::PERM) => {}
            Err(e) => return Err(io::Error::from(e)),
        }
    }
    if parent.symlink_metadata(name)?.file_type().is_symlink() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "symlink"));
    }
    parent.open_dir(name)
}

/// Why opening `name` in `parent` failed, worded for the user.
fn refusal(parent: &Dir, name: &str, e: io::Error) -> Error {
    match parent.symlink_metadata(name) {
        Ok(m) if m.file_type().is_symlink() => {
            Error::PathInvalid(format!("{name} is a symlink; not followed"))
        }
        Ok(m) if !m.is_dir() => Error::PathInvalid(format!("{name} exists and is not a folder")),
        _ => map_io(e),
    }
}

/// Walks `components` beneath `root`, creating missing folders (0700 if `private`, SR-23),
/// and refuses any component that is a symlink or not a folder (SR-18). Returns a handle to
/// the final folder.
pub(crate) fn safe_dir(root: &Dir, components: &[String], private: bool) -> Result<Dir> {
    let mut dir = root.try_clone()?;
    let mut builder = cap_std::fs::DirBuilder::new();
    #[cfg(unix)]
    if private {
        use cap_std::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    #[cfg(not(unix))]
    let _ = private;
    for c in components {
        check_component(c)?;
        match dir.create_dir_with(c, &builder) {
            Ok(()) => {}
            // Existing folder, file or symlink: the open below tells them apart.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(refusal(&dir, c, e)),
        }
        dir = open_child_dir(&dir, c).map_err(|e| refusal(&dir, c, e))?;
    }
    Ok(dir)
}

/// Opens a directory handle on a path this host created itself (a staging folder).
pub(crate) fn open_own_dir(path: &Path) -> Result<Dir> {
    Ok(Dir::open_ambient_dir(path, cap_std::ambient_authority())?)
}

/// Makes a folder's entries durable (Unix `fsync`). A no-op elsewhere. `cap-std` may hold
/// a folder as an `O_PATH` handle, which cannot be synced, so `.` is reopened readable
/// first; that open is relative to the handle and resolves nothing else.
pub(crate) fn sync_dir_handle(dir: &Dir) -> Result<()> {
    #[cfg(unix)]
    {
        use rustix::fs::{openat, Mode, OFlags};
        let sync = || -> rustix::io::Result<()> {
            let fd = openat(
                dir,
                ".",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )?;
            rustix::fs::fsync(&fd)
        };
        sync().map_err(|e| map_io(io::Error::from(e)))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
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

/// Options for a new owner-only, never-executable file relative to a [`Dir`] (0600 on
/// Unix, SR-23), created exclusively.
fn private_create_new() -> cap_std::fs::OpenOptions {
    let mut o = cap_std::fs::OpenOptions::new();
    o.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Exclusively creates `name` in `dir`, or `name (1)`, `name (2)`, ... on collision; never
/// overwrites and never follows a symlink at the final name (SR-18). Returns the name used
/// and the open file.
pub(crate) fn create_unique(dir: &Dir, name: &str) -> Result<(String, File)> {
    check_component(name)?;
    let options = private_create_new();
    for n in 0..MAX_COLLISIONS {
        let candidate = collision_name(name, n);
        match dir.open_with(&candidate, &options) {
            Ok(f) => return Ok((candidate, f.into_std())),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(map_io(e)),
        }
    }
    Err(Error::PathInvalid(format!(
        "{name}: too many existing files with this name"
    )))
}

/// Moves `staged_name` from the `staging` folder to `name` (or its " (n)" form) in `dir`
/// without overwriting anything (SR-18): the final name is created exclusively, then the
/// staged file is renamed onto that placeholder between the two handles. Returns the name
/// used. The caller syncs `dir`.
pub(crate) fn place_into(
    staging: &Dir,
    staged_name: &str,
    dir: &Dir,
    name: &str,
) -> Result<String> {
    let (final_name, placeholder) = create_unique(dir, name)?;
    drop(placeholder);
    staging
        .rename(staged_name, dir, &final_name)
        .map_err(map_io)?;
    Ok(final_name)
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
        let d = open_root(dir.path()).unwrap();
        let (name, _) = create_unique(&d, "a.txt").unwrap();
        assert_eq!(name, "a (1).txt");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"keep");
        assert!(matches!(
            create_unique(&d, "../x"),
            Err(Error::PathInvalid(_))
        ));
        assert!(matches!(
            safe_dir(&d, &["..".into()], false),
            Err(Error::PathInvalid(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn private_modes_and_nested_dirs() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("r");
        let d = open_root(&root).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&root), 0o700);
        let leaf = safe_dir(&d, &["a".into(), "b".into()], true).unwrap();
        assert_eq!(mode(&root.join("a").join("b")), 0o700);
        let (name, _) = create_unique(&leaf, "f").unwrap();
        assert_eq!(mode(&root.join("a").join("b").join(name)), 0o600);
        // Walking again reuses the existing folders.
        safe_dir(&d, &["a".into(), "b".into()], true).unwrap();
        std::fs::write(root.join("file"), b"x").unwrap();
        assert!(matches!(
            safe_dir(&d, &["file".into()], false),
            Err(Error::PathInvalid(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("bashrc");
        std::fs::write(&target, b"original").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("sub")).unwrap();
        let d = open_root(dir.path()).unwrap();
        assert!(matches!(
            safe_dir(&d, &["sub".into()], false),
            Err(Error::PathInvalid(_))
        ));
        // A link deeper in the walk is refused too.
        let inner = safe_dir(&d, &["ok".into()], false).unwrap();
        drop(inner);
        std::os::unix::fs::symlink(outside.path(), dir.path().join("ok").join("deep")).unwrap();
        assert!(matches!(
            safe_dir(&d, &["ok".into(), "deep".into(), "x".into()], false),
            Err(Error::PathInvalid(_))
        ));
        std::os::unix::fs::symlink(&target, dir.path().join("x")).unwrap();
        let (name, _) = create_unique(&d, "x").unwrap();
        assert_eq!(name, "x (1)");
        assert!(open_existing_regular(&dir.path().join("x")).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
    }

    /// SR-18: a symlink planted inside the receive root that points outside is never used to
    /// create a file there, even when the walk would go through it.
    #[cfg(unix)]
    #[test]
    fn planted_symlink_to_outside_is_not_followed_when_creating() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let root = dir.path().join("Downloads");
        let d = open_root(&root).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.join("Photos")).unwrap();
        let staging = safe_dir(&d, &[".staging".into()], true).unwrap();
        std::fs::write(root.join(".staging").join("0.part"), b"payload").unwrap();

        let r = safe_dir(&d, &["Photos".into()], false)
            .and_then(|photos| place_into(&staging, "0.part", &photos, "evil.sh"));
        assert!(matches!(r, Err(Error::PathInvalid(_))), "{r:?}");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
        assert!(
            root.join(".staging").join("0.part").exists(),
            "nothing moved"
        );

        // A link at the final name is not overwritten either.
        std::os::unix::fs::symlink(outside.path().join("t"), root.join("evil.sh")).unwrap();
        let name = place_into(&staging, "0.part", &d, "evil.sh").unwrap();
        assert_eq!(name, "evil (1).sh");
        assert_eq!(std::fs::read(root.join("evil (1).sh")).unwrap(), b"payload");
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
