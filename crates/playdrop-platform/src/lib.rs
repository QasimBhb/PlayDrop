//! Desktop platform hooks shared by `playdropd` and `playdrop host`.
//!
//! The core forbids `unsafe`, so what needs a system call it cannot make arrives through
//! `HostConfig` hooks. Both desktop hosts plug in the same ones here, so the CLI host gets
//! the daemon's protections too:
//!
//! - `at_rest` (Windows): DPAPI sealing of keys and pins, the owner-only data dir DACL
//!   (SR-28).
//! - `free_space` (Windows): free space of the receive volume for the SR-20 reserve.
//! - `listener` (Windows): `SO_EXCLUSIVEADDRUSE` on the listening port (SR-31).
//!
//! On Linux the core does all three itself (`statvfs`, mode 0700 and 0600, no
//! `SO_REUSEPORT`), so [`apply_hooks`] changes nothing there.

use std::io;
use std::path::Path;

use playdrop_core::host::HostConfig;

#[cfg(windows)]
pub mod at_rest;
#[cfg(windows)]
pub mod free_space;
#[cfg(windows)]
pub mod listener;

/// Plugs this platform's free space, secret store and listener hooks into `config`.
#[cfg_attr(not(windows), allow(clippy::needless_pass_by_ref_mut))]
pub fn apply_hooks(config: &mut HostConfig) {
    #[cfg(windows)]
    {
        use std::sync::Arc;
        config.free_space = Arc::new(free_space::WindowsVolumeSpace);
        config.secret_store = Some(Arc::new(at_rest::DpapiStore));
        config.listener_setup = Some(Arc::new(listener::ExclusivePort));
    }
    #[cfg(not(windows))]
    let _ = config;
}

/// Creates the data dir if needed and makes it private before the core reads or writes
/// keys in it (SR-28). Unix: mode 0700 on a new dir; the core refuses looser modes on an
/// existing one. Windows: a protected DACL with only this user and `SYSTEM`, set on every
/// call so an install made before the DACL existed is tightened too; everything the core
/// creates inside inherits it.
pub fn prepare_data_dir(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        std::fs::create_dir_all(dir)?;
        at_rest::protect_dir(dir)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(any(unix, windows)))]
    std::fs::create_dir_all(dir)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn new_data_dir_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = std::env::temp_dir().join(format!("pd-platform-{}", std::process::id()));
        let dir = tmp.join("a").join("data");
        prepare_data_dir(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        prepare_data_dir(&dir).unwrap(); // an existing dir is fine
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn hooks_leave_linux_defaults() {
        let tmp = std::env::temp_dir();
        let mut c = HostConfig::new(&tmp, &tmp, "t");
        apply_hooks(&mut c);
        assert!(c.secret_store.is_none());
        assert!(c.listener_setup.is_none());
    }
}
