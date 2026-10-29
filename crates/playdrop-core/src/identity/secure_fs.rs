//! Private on-disk storage for keys, announce keys, pins and grants (SR-28).
//!
//! On Unix the storage directory must be mode 0700 or stricter and every file 0600 or
//! stricter. Looser modes are refused, never repaired silently, so a misconfigured install
//! stops with a clear message instead of running with readable keys. Symlinked files are
//! refused for the same reason. Writes go to a fresh temporary file in the same directory,
//! are flushed, then renamed over the target so a crash never leaves a half-written file.
//!
//! On other platforms the mode checks are skipped. On Windows `playdropd` gives the data
//! dir a protected owner-only DACL before the core starts (new files here inherit it) and
//! seals `identity.key` and `pins.json` with DPAPI through [`super::sealing`]; Android and
//! iOS seal through the same trait with their keystores. This crate has no `unsafe` and no
//! Windows security APIs, so it neither sets nor checks the DACL itself.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// Permission bits that must be clear on private directories and files (group and other).
#[cfg(unix)]
const LOOSE_BITS: u32 = 0o077;

/// Builds the error returned when an existing path has looser permissions than SR-28 allows.
#[cfg(unix)]
fn insecure(path: &Path, mode: u32, wanted: u32) -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refusing to use {}: mode {:o} is looser than {:o} (SR-28); fix it with chmod {:o}",
            path.display(),
            mode & 0o777,
            wanted,
            wanted
        ),
    ))
}

/// Creates `dir` (and missing parents) with mode 0700, or checks an existing one.
///
/// Refuses an existing directory whose mode grants any group or other bit (SR-28).
pub(crate) fn ensure_private_dir(dir: &Path) -> Result<()> {
    match fs::symlink_metadata(dir) {
        Ok(meta) => {
            if !meta.is_dir() {
                return Err(Error::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{} exists and is not a directory", dir.display()),
                )));
            }
            check_dir_mode(dir, &meta)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(dir)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn check_dir_mode(dir: &Path, meta: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    if mode & LOOSE_BITS != 0 {
        return Err(insecure(dir, mode, 0o700));
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Same signature as the Unix variant.
fn check_dir_mode(_dir: &Path, _meta: &fs::Metadata) -> Result<()> {
    Ok(())
}

/// Reads a private file, or returns `Ok(None)` if it does not exist.
///
/// Refuses symlinks, non-regular files and files with group or other bits set (SR-28).
pub(crate) fn read_private(path: &Path) -> Result<Option<Vec<u8>>> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if !meta.file_type().is_file() {
        return Err(Error::Io(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to use {}: not a regular file", path.display()),
        )));
    }
    check_file_mode(path, &meta)?;
    Ok(Some(fs::read(path)?))
}

#[cfg(unix)]
fn check_file_mode(path: &Path, meta: &fs::Metadata) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = meta.permissions().mode();
    if mode & LOOSE_BITS != 0 {
        return Err(insecure(path, mode, 0o600));
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Same signature as the Unix variant.
fn check_file_mode(_path: &Path, _meta: &fs::Metadata) -> Result<()> {
    Ok(())
}

/// Writes `data` to a new mode-0600 temporary file next to `path`, flushes it and returns
/// the temporary path. Pair with [`commit`] to make the write visible atomically.
pub(crate) fn stage_private(path: &Path, data: &[u8]) -> Result<PathBuf> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut suffix = [0u8; 8];
    crate::identity::fill_random(&mut suffix)?;
    let tmp = dir.join(format!(
        ".{name}.tmp-{}",
        data_encoding::HEXLOWER.encode(&suffix)
    ));

    let mut opts = OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| -> io::Result<()> {
        let mut file = opts.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()
    })();
    if let Err(e) = result {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(tmp)
}

/// Renames a staged temporary file over its target and flushes the directory entry.
pub(crate) fn commit(tmp: &Path, path: &Path) -> Result<()> {
    if let Err(e) = fs::rename(tmp, path) {
        let _ = fs::remove_file(tmp);
        return Err(e.into());
    }
    sync_dir(path.parent().unwrap_or_else(|| Path::new(".")))
}

/// Atomically replaces `path` with `data`, mode 0600 (SR-28).
pub(crate) fn write_private_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let tmp = stage_private(path, data)?;
    commit(&tmp, path)
}

#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    fs::File::open(dir)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)] // Same signature as the Unix variant.
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}
