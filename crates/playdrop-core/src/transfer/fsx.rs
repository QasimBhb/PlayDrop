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
//! A staged file reaches its final name through a rename that never replaces anything
//! (SR-18): `renameat2(RENAME_NOREPLACE)` on Linux and Android, `renameatx_np(RENAME_EXCL)`
//! on Apple platforms, both between the two folder handles. It fails on any existing name,
//! symlinks included, and the next " (n)" name is tried. Where that rename is not
//! available (Windows, or a filesystem that refuses the flag) the final name is first
//! created exclusively with `create_new` relative to the folder handle, and the staged
//! file is renamed onto that empty placeholder.
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

/// Windows share mode for every staged, ledger and placeholder file:
/// `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` (1 | 2 | 4, `winnt.h`).
///
/// On Windows a file cannot be deleted or renamed, and a rename cannot replace it, while
/// any handle on it was opened without `FILE_SHARE_DELETE`. Opening everything with it
/// means a handle that lives a little too long (a sink still holding a delivered file, a
/// `.part` writer not yet dropped) cannot block cleanup or placement. `std` and `cap-std`
/// default to this today; it is spelled out so the guarantee does not rest on a default.
#[cfg(windows)]
const SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;

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
    #[cfg_attr(not(unix), allow(unused_mut, reason = "mode is set on Unix only"))]
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
#[cfg_attr(
    not(unix),
    allow(clippy::unnecessary_wraps, reason = "a no-op off Unix, same signature")
)]
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
    share_all(&mut o);
    o
}

/// Lets other handles read, write, delete and rename the file while this one is open
/// (Windows, see [`SHARE_ALL`]). A no-op elsewhere, where that is always allowed.
fn share_all(o: &mut OpenOptions) {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        o.share_mode(SHARE_ALL);
    }
    #[cfg(not(windows))]
    let _ = o;
}

/// Opens an existing staged file read-only, without blocking its later deletion or
/// rename on Windows (see [`SHARE_ALL`]).
pub(crate) fn open_shared_read(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true);
    share_all(&mut o);
    o.open(path)
}

/// Opens an existing file so its data can be flushed. Windows' `FlushFileBuffers` needs
/// write access: syncing a read-only handle fails with "Access is denied".
fn open_for_sync(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.write(true);
    share_all(&mut o);
    o.open(path)
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
    #[cfg(windows)]
    {
        use cap_std::fs::OpenOptionsExt;
        o.share_mode(SHARE_ALL);
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

/// True where this build has a rename that never replaces its target (see
/// [`rename_noreplace`]). Elsewhere [`place_unique`] reserves names with placeholders.
/// On Android, `renameat2` is in bionic's app seccomp allowlist from API 26 and rustix
/// issues the raw syscall.
pub(crate) const RENAME_NOREPLACE: bool = cfg!(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple"
));

/// Renames `from` in `from_dir` to `to` in `to_dir` only if `to` does not exist, between
/// the two handles: the kernel checks and renames in one step, so nothing at `to`, a
/// symlink included, is ever replaced (SR-18).
///
/// Errors: [`io::ErrorKind::AlreadyExists`] when `to` exists;
/// [`io::ErrorKind::Unsupported`] where there is no such rename (Windows, a kernel without
/// `renameat2`, or a filesystem that refuses the flag); `EPERM` comes back as
/// [`io::ErrorKind::PermissionDenied`].
pub(crate) fn rename_noreplace(
    from_dir: &Dir,
    from: &str,
    to_dir: &Dir,
    to: &str,
) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android", target_vendor = "apple"))]
    {
        use rustix::fs::{renameat_with, RenameFlags};
        use rustix::io::Errno;
        match renameat_with(from_dir, from, to_dir, to, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(Errno::EXIST) => Err(io::ErrorKind::AlreadyExists.into()),
            // ENOSYS: no renameat2 (Linux before 3.15). EINVAL, ENOTSUP: a filesystem
            // without the flag (some FUSE and network filesystems on Linux, non-APFS
            // volumes on Apple platforms). EPERM stays a permission error here: it may also
            // be a sandbox hiding the call, which `place_unique` tells apart.
            Err(Errno::NOSYS | Errno::INVAL | Errno::NOTSUP) => {
                Err(io::ErrorKind::Unsupported.into())
            }
            Err(e) => Err(e.into()),
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_vendor = "apple")))]
    {
        // Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING` takes paths, and
        // `cap-std` itself turns folder handles back into paths there; a handle-relative
        // rename (`SetFileInformationByHandle` with `FILE_RENAME_INFO.RootDirectory`) has
        // not been tried on a real Windows machine. The placeholder path stays.
        let _ = (from_dir, from, to_dir, to);
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// What [`place_unique`] tells its caller about a candidate name, so the caller can
/// record it before anything happens to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// The name was free a moment ago and the staged file is about to be renamed to it
    /// without replacing anything. A record of it is an intent: after a kill, a staged
    /// file still there was not renamed; one that is gone went to the last name recorded.
    Rename,
    /// There is no rename that never replaces here (Windows, or a filesystem that refuses
    /// the flag): the name, free a moment ago, is about to be reserved with an empty
    /// placeholder and the staged file renamed onto it. A record of it must say so, so
    /// that recovery renames onto an empty file at that name instead of placing again.
    Reserve,
    /// The placeholder now exists and the rename onto it comes next. Callers record
    /// nothing here; it exists so tests can stop between the two.
    Reserved,
}

/// Moves `staged_name` from the `staging` folder to `name`, or the first free " (n)" form
/// of it, in `dir`, never replacing anything and never following a symlink at the final
/// name (SR-18). Returns the name used. The caller syncs `dir`.
///
/// A name that already exists (checked without following a symlink) is skipped before the
/// caller hears of it, so a record only ever names a name believed free; the kernel's
/// no-replace check (or the exclusive create) still guards a name taken in between, and
/// the next name is then noted before the next attempt.
///
/// `note` is told each candidate with its [`Step`], and its error stops the move.
///
/// `noreplace` is the caller's memory of whether the no-replace rename is worth trying in
/// this folder: false skips it (tests set it to reach the placeholder path), and this
/// function sets it false once the rename proves unavailable, so later members go
/// straight to placeholders. It is never tried where [`RENAME_NOREPLACE`] is false.
/// `Unsupported` (ENOSYS, EINVAL, ENOTSUP) proves it at once. `EPERM` is ambiguous (a
/// sandbox that hides `renameat2`, or a real permission refusal): the same name is then
/// reserved and the plain rename onto that placeholder is tried, and only if that
/// succeeds is the flag judged unavailable; if it fails, its error is returned.
pub(crate) fn place_unique(
    staging: &Dir,
    staged_name: &str,
    dir: &Dir,
    name: &str,
    noreplace: &mut bool,
    mut note: impl FnMut(&str, Step) -> Result<()>,
) -> Result<String> {
    check_component(name)?;
    if !RENAME_NOREPLACE {
        *noreplace = false;
    }
    for n in 0..MAX_COLLISIONS {
        let candidate = collision_name(name, n);
        if is_taken(dir, &candidate)? {
            continue;
        }
        let mut refused = false;
        if *noreplace {
            note(&candidate, Step::Rename)?;
            match rename_noreplace(staging, staged_name, dir, &candidate) {
                Ok(()) => return Ok(candidate),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                // The same name is reserved below, under a record that says so.
                Err(e) if e.kind() == io::ErrorKind::Unsupported => *noreplace = false,
                Err(e) if e.kind() == io::ErrorKind::PermissionDenied => refused = true,
                Err(e) => return Err(map_io(e)),
            }
        }
        note(&candidate, Step::Reserve)?;
        if !reserve(dir, &candidate)? {
            continue;
        }
        note(&candidate, Step::Reserved)?;
        staging
            .rename(staged_name, dir, &candidate)
            .map_err(map_io)?;
        if refused {
            // The plain rename was allowed where the flagged one was not: a sandbox
            // hides `renameat2`, not a permission problem.
            *noreplace = false;
        }
        return Ok(candidate);
    }
    Err(Error::PathInvalid(format!(
        "{name}: too many existing files with this name"
    )))
}

/// True when `name` exists in `dir`, a symlink included (not followed).
fn is_taken(dir: &Dir, name: &str) -> Result<bool> {
    match dir.symlink_metadata(name) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(map_io(e)),
    }
}

/// Creates `name` in `dir` exclusively as an empty placeholder. False when the name is
/// taken (symlinks included).
fn reserve(dir: &Dir, name: &str) -> Result<bool> {
    match dir.open_with(name, &private_create_new()) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(map_io(e)),
    }
}

/// [`place_unique`] for a caller that keeps no record of the name.
pub(crate) fn place_into(
    staging: &Dir,
    staged_name: &str,
    dir: &Dir,
    name: &str,
) -> Result<String> {
    place_unique(staging, staged_name, dir, name, &mut true, |_, _| Ok(()))
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
#[cfg_attr(
    not(unix),
    allow(clippy::unnecessary_wraps, reason = "a no-op off Unix, same signature")
)]
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

/// Makes the data of many staged files durable at once (protocol.md 7.6). On Linux and
/// Android one `syncfs` on the filesystem holding `dir` replaces an `fdatasync` per file,
/// which keeps thousands of small bundle members from costing thousands of flushes
/// (Phase 9 benchmark, `tests/phase9_receiver.rs`, checkpoint time per 4 to 64 KiB
/// member: 0.53 ms with a flush each against 0.05 ms with `syncfs` on ext4, 0.8 to 1.4 ms
/// against 0.25 to 0.5 ms on the test phone's f2fs). Elsewhere, and where `syncfs` is
/// refused, each file is reopened writable and flushed, since Windows refuses to flush a
/// read-only handle.
///
/// `syncfs` is used only where [`Flush::for_dir`] found a local block filesystem: on a
/// stacked or remote one (Android's sdcardfs on API 26 to 29, FUSE, NFS) it can return
/// success without flushing the data underneath, and a checkpoint would then call bytes
/// durable that are not. Kernels before 5.8 do not report writeback errors through
/// `syncfs`; that is accepted, as the bytes are verified again on resume.
///
/// Android: `syncfs` is in bionic's app seccomp allowlist from API 26 and rustix issues
/// the raw syscall. It flushes the whole data partition, other apps' dirty pages
/// included; at one call per checkpoint (64 MiB or 1 s) that is still far cheaper than a
/// flush per member.
pub(crate) fn sync_files(dir: &Path, files: &[PathBuf], flush: Flush) -> Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    if files.len() > 4 && flush == Flush::Syncfs {
        let d = File::open(dir)?;
        match rustix::fs::syncfs(&d) {
            Ok(()) => return Ok(()),
            // An old kernel or a sandbox without it: flush file by file below.
            Err(rustix::io::Errno::NOSYS | rustix::io::Errno::PERM) => {}
            Err(e) => return Err(map_io(io::Error::from(e))),
        }
    }
    let _ = (dir, flush);
    for f in files {
        open_for_sync(f)?.sync_data().map_err(map_io)?;
    }
    Ok(())
}

/// How [`sync_files`] makes many files durable on one filesystem. Decided once per
/// staging folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(any(target_os = "linux", target_os = "android")),
    allow(dead_code, reason = "syncfs is used on Linux and Android only")
)]
pub(crate) enum Flush {
    /// One `syncfs` flushes every file on it (a local block filesystem, Linux and Android).
    Syncfs,
    /// One `fdatasync` per file: every other filesystem and platform.
    PerFile,
}

impl Flush {
    /// Looks at the filesystem holding `dir` (`fstatfs`). Anything not known to flush its
    /// own data on `syncfs`, or not identifiable, gets [`Flush::PerFile`].
    pub(crate) fn for_dir(dir: &Path) -> Self {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let magic = File::open(dir)
                .ok()
                .and_then(|d| rustix::fs::fstatfs(&d).ok())
                // Filesystem magic numbers are 32 bits; the field is wider on 64-bit
                // targets and may be signed (and is already `u32` on 32-bit Arm).
                .map(|st| {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::unnecessary_cast
                    )]
                    let m = st.f_type as u32;
                    m
                });
            magic.map_or(Flush::PerFile, Self::for_fs_type)
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        {
            let _ = dir;
            Flush::PerFile
        }
    }

    /// The decision for a filesystem magic number (`statfs.f_type`, `linux/magic.h`).
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "android")),
        allow(dead_code, reason = "syncfs is used on Linux and Android only")
    )]
    fn for_fs_type(magic: u32) -> Self {
        const EXT2_3_4: u32 = 0xEF53;
        const F2FS: u32 = 0xF2F5_2010;
        const XFS: u32 = 0x5846_5342;
        const BTRFS: u32 = 0x9123_683E;
        match magic {
            EXT2_3_4 | F2FS | XFS | BTRFS => Flush::Syncfs,
            // sdcardfs (0x5DCA2DF5), FUSE (0x65735546), NFS (0x6969), tmpfs and the rest.
            _ => Flush::PerFile,
        }
    }
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

    /// Flushing staged files reopens them writable: Windows refuses to flush a read-only
    /// handle ("Access is denied"). Both the per-file path and, on Linux, the `syncfs`
    /// path work.
    #[test]
    fn sync_files_flushes_few_and_many() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..6)
            .map(|i| {
                let p = dir.path().join(format!("{i}.part"));
                drop(open_or_create(&p).unwrap());
                std::fs::write(&p, b"data").unwrap();
                p
            })
            .collect();
        let flush = Flush::for_dir(dir.path());
        sync_files(dir.path(), &files[..2], flush).unwrap();
        sync_files(dir.path(), &files, flush).unwrap();
        sync_files(dir.path(), &files, Flush::PerFile).unwrap();
        // A reader still holding a file does not stop its deletion (FILE_SHARE_DELETE).
        let reader = open_shared_read(&files[0]).unwrap();
        remove_if_exists(&files[0]).unwrap();
        assert!(!files[0].exists());
        drop(reader);
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

    /// A staging folder with `0.part` and a receive root holding `a.txt` ("keep").
    fn place_fixture() -> (tempfile::TempDir, Dir, Dir) {
        let dir = tempfile::tempdir().unwrap();
        let d = open_root(dir.path()).unwrap();
        let staging = safe_dir(&d, &[".staging".into()], true).unwrap();
        std::fs::write(dir.path().join(".staging").join("0.part"), b"new").unwrap();
        std::fs::write(dir.path().join("a.txt"), b"keep").unwrap();
        (dir, d, staging)
    }

    /// A name that exists is skipped before it is noted, so a record only names a name
    /// believed free; where the no-replace rename exists it refuses a taken name. Nothing
    /// is overwritten (SR-18).
    #[test]
    fn place_unique_notes_only_free_names_and_never_overwrites() {
        let (dir, d, staging) = place_fixture();
        if RENAME_NOREPLACE {
            let e = rename_noreplace(&staging, "0.part", &d, "a.txt").unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        }
        let mut noted = Vec::new();
        let name = place_unique(&staging, "0.part", &d, "a.txt", &mut true, |c, s| {
            noted.push((c.to_owned(), s));
            Ok(())
        })
        .unwrap();
        assert_eq!(name, "a (1).txt");
        let want = if RENAME_NOREPLACE {
            vec![("a (1).txt".to_owned(), Step::Rename)]
        } else {
            vec![
                ("a (1).txt".to_owned(), Step::Reserve),
                ("a (1).txt".to_owned(), Step::Reserved),
            ]
        };
        assert_eq!(noted, want);
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"keep");
        assert_eq!(std::fs::read(dir.path().join("a (1).txt")).unwrap(), b"new");

        // A failing note stops the move: nothing renamed, no placeholder left.
        std::fs::write(dir.path().join(".staging").join("1.part"), b"x").unwrap();
        let r = place_unique(&staging, "1.part", &d, "b.txt", &mut true, |_, _| {
            Err(Error::Io(io::Error::other("no record")))
        });
        assert!(r.is_err());
        assert!(!dir.path().join("b.txt").exists());
        assert!(dir.path().join(".staging").join("1.part").exists());
    }

    /// A name taken between the check and the attempt (another process) is refused by
    /// the kernel or the exclusive create, and the next name is noted before the next
    /// attempt, on both paths.
    #[test]
    fn place_unique_retries_a_name_taken_after_it_was_noted() {
        for try_noreplace in [true, false] {
            let (dir, d, staging) = place_fixture();
            let root = dir.path().to_path_buf();
            let mut noted = Vec::new();
            let mut noreplace = try_noreplace;
            let name = place_unique(&staging, "0.part", &d, "b.txt", &mut noreplace, |c, s| {
                if c == "b.txt" && s != Step::Reserved {
                    std::fs::write(root.join("b.txt"), b"theirs").unwrap();
                }
                noted.push((c.to_owned(), s));
                Ok(())
            })
            .unwrap();
            assert_eq!(name, "b (1).txt", "{try_noreplace}");
            let first = if try_noreplace && RENAME_NOREPLACE {
                Step::Rename
            } else {
                Step::Reserve
            };
            assert_eq!(noted[0], ("b.txt".to_owned(), first));
            assert_eq!(noted[1], ("b (1).txt".to_owned(), first));
            assert_eq!(std::fs::read(root.join("b.txt")).unwrap(), b"theirs");
            assert_eq!(std::fs::read(root.join("b (1).txt")).unwrap(), b"new");
        }
    }

    /// `syncfs` only for local block filesystems; sdcardfs, FUSE, NFS and anything
    /// unknown flush file by file.
    #[test]
    fn syncfs_only_on_known_local_filesystems() {
        for magic in [0xEF53, 0xF2F5_2010, 0x5846_5342, 0x9123_683E] {
            assert_eq!(Flush::for_fs_type(magic), Flush::Syncfs, "{magic:#x}");
        }
        for magic in [
            0x5DCA_2DF5,
            0x6573_5546,
            0x6969,
            0x0102_1994,
            0,
            0x1234_5678,
        ] {
            assert_eq!(Flush::for_fs_type(magic), Flush::PerFile, "{magic:#x}");
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        assert_eq!(Flush::for_dir(Path::new(".")), Flush::PerFile);
        // A folder that cannot be looked at is not trusted either.
        assert_eq!(
            Flush::for_dir(Path::new("/no/such/playdrop/folder")),
            Flush::PerFile
        );
    }

    #[cfg(unix)]
    #[test]
    fn rename_noreplace_refuses_a_symlink_at_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("bashrc");
        std::fs::write(&target, b"original").unwrap();
        let d = open_root(dir.path()).unwrap();
        let staging = safe_dir(&d, &[".staging".into()], true).unwrap();
        std::fs::write(dir.path().join(".staging").join("0.part"), b"evil").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("x")).unwrap();
        let e = rename_noreplace(&staging, "0.part", &d, "x").unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"original");
        assert!(std::fs::symlink_metadata(dir.path().join("x"))
            .unwrap()
            .file_type()
            .is_symlink());
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
