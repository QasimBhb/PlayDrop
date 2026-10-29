//! Free space of the receive volume on Windows (SR-20), from `GetDiskFreeSpaceExW`.
//!
//! The core reads free space through [`FreeSpace`] and uses `statvfs` on Unix. It forbids
//! `unsafe`, so on Windows its built-in fallback cannot call the system and reports a fixed
//! huge volume; the daemon plugs this adapter into `HostConfig::free_space` instead, so the
//! reserve of max(2 GiB, 5%) is enforced at accept and on every write.
//!
//! Not compiled or tested on the Linux build machine; Windows CI runs the test below.

use std::io;
use std::path::Path;

use playdrop_core::transfer::{FreeSpace, SpaceInfo};
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

/// [`FreeSpace`] backed by `GetDiskFreeSpaceExW`.
#[derive(Debug, Clone, Copy, Default)]
pub struct WindowsVolumeSpace;

impl FreeSpace for WindowsVolumeSpace {
    fn space(&self, path: &Path) -> io::Result<SpaceInfo> {
        // The receive root may not exist yet; ask about its nearest existing ancestor, as
        // the core's Unix version does.
        let mut probe = path;
        while !probe.exists() {
            match probe.parent() {
                Some(p) if !p.as_os_str().is_empty() => probe = p,
                _ => break,
            }
        }
        disk_space(probe)
    }
}

/// `available` is what this user may still write (quotas included), `total` the volume
/// size.
#[allow(unsafe_code)]
fn disk_space(path: &Path) -> io::Result<SpaceInfo> {
    let wide = HSTRING::from(path);
    let mut available = 0u64;
    let mut total = 0u64;
    // SAFETY: `wide` is a NUL-terminated wide string alive for the call; the two out
    // pointers are valid u64s and the third count is not requested.
    unsafe { GetDiskFreeSpaceExW(&wide, Some(&raw mut available), Some(&raw mut total), None) }
        .map_err(|e| io::Error::other(format!("{}: {}", path.display(), e.message())))?;
    Ok(SpaceInfo { available, total })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_has_real_numbers() {
        let tmp = std::env::temp_dir();
        let s = WindowsVolumeSpace.space(&tmp).unwrap();
        assert!(s.total > 0);
        assert!(s.available <= s.total, "{s:?}");
        assert!(s.total < 1 << 50, "not the old 1 PiB placeholder: {s:?}");

        // A folder that does not exist yet reports its volume.
        let missing = tmp.join("playdrop-free-space-missing").join("deeper");
        assert_eq!(WindowsVolumeSpace.space(&missing).unwrap().total, s.total);
    }
}
