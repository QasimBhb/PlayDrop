//! Start at login and D-Bus activation.
//!
//! Linux, outside Flatpak (packaging owns the Flatpak case, detected by `/.flatpak-info`):
//! - `~/.config/autostart/org.playdrop.Daemon.desktop`, the XDG autostart entry, written
//!   when `autostart` is on and removed when it is off. The daemon syncs it on the first
//!   run and whenever the setting changes, not on every start, so a user who deletes the
//!   entry with a desktop tool is not overruled at the next login.
//! - `~/.local/share/dbus-1/services/org.playdrop.PlayDrop.Daemon1.service`, so a UI call starts
//!   the daemon (`docs/ipc.md` section 2). *Choice:* this file is kept on every start
//!   regardless of `autostart`, because activation is how the window reaches a daemon that
//!   is not running, which matters most exactly when autostart is off. Keeping it current
//!   on each start also follows the binary if it moves.
//!
//! Windows: the `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value `PlayDrop`.
//!
//! TODO(Phase 6): inside Flatpak, ask the Background portal (`RequestBackground`) for
//! autostart instead of relying on packaging alone.
//!
//! Every write is idempotent: a file whose content is already right is not touched.

use std::path::{Path, PathBuf};

/// The XDG autostart file name.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const AUTOSTART_FILE: &str = "org.playdrop.Daemon.desktop";
/// The D-Bus session service file name.
pub const DBUS_SERVICE_FILE: &str = "org.playdrop.PlayDrop.Daemon1.service";

/// Where the per-user files live. Production uses [`Locations::user`]; tests point it at a
/// temp dir so they never touch the real home.
#[derive(Debug, Clone)]
pub struct Locations {
    /// `$XDG_CONFIG_HOME` (`~/.config`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub config_home: PathBuf,
    /// `$XDG_DATA_HOME` (`~/.local/share`).
    pub data_home: PathBuf,
    /// The daemon binary.
    pub exe: PathBuf,
    /// Running inside Flatpak: packaging owns these files.
    pub flatpak: bool,
}

impl Locations {
    /// The current user's directories and this binary.
    pub fn user() -> anyhow::Result<Self> {
        let base =
            directories::BaseDirs::new().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
        Ok(Self {
            config_home: base.config_dir().to_path_buf(),
            data_home: base.data_dir().to_path_buf(),
            exe: std::env::current_exe()?,
            flatpak: Path::new("/.flatpak-info").exists(),
        })
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn autostart_path(&self) -> PathBuf {
        self.config_home.join("autostart").join(AUTOSTART_FILE)
    }

    fn dbus_service_path(&self) -> PathBuf {
        self.data_home
            .join("dbus-1")
            .join("services")
            .join(DBUS_SERVICE_FILE)
    }
}

/// What a sync did, for logs and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The file was written.
    Written,
    /// The file was removed.
    Removed,
    /// The file already matched.
    Unchanged,
    /// Nothing to do on this platform or in Flatpak.
    Skipped,
}

/// Writes or removes the login autostart entry.
pub fn sync_autostart(loc: &Locations, enabled: bool) -> anyhow::Result<Outcome> {
    if loc.flatpak {
        return Ok(Outcome::Skipped);
    }
    platform::sync_autostart(loc, enabled)
}

/// Keeps the D-Bus activation file pointing at this binary (Linux only).
pub fn sync_dbus_service(loc: &Locations) -> anyhow::Result<Outcome> {
    if loc.flatpak || !cfg!(target_os = "linux") {
        return Ok(Outcome::Skipped);
    }
    let body = format!(
        "[D-BUS Service]\nName={}\nExec={}\n",
        playdrop_ipc::DBUS_NAME,
        exec_quote(&loc.exe.to_string_lossy())
    );
    write_if_changed(&loc.dbus_service_path(), &body)
}

/// The XDG autostart entry. `Exec` is the bare `playdropd` when that name on `PATH` is this
/// binary, otherwise the absolute path.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn desktop_entry(exe: &Path) -> String {
    let exec = if on_path_as("playdropd", exe) {
        "playdropd".to_owned()
    } else {
        exec_quote(&exe.to_string_lossy())
    };
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=PlayDrop\n\
         Comment=Phone and computer companion on the local network\n\
         Exec={exec}\n\
         Icon=org.playdrop.PlayDrop\n\
         Terminal=false\n\
         NoDisplay=true\n\
         Hidden=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

/// Quotes an `Exec` argument per the Desktop Entry spec when it needs it: wraps it in
/// double quotes and escapes `"`, `` ` ``, `$` and `\`. D-Bus service files parse `Exec`
/// with the same shell-like rules.
pub fn exec_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.' | '+' | ',' | ':')
        });
    if plain {
        return arg.to_owned();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn on_path_as(name: &str, exe: &Path) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    let me = std::fs::canonicalize(exe).ok();
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .is_some_and(|p| std::fs::canonicalize(p).ok() == me)
}

/// Writes `body` to `path` unless it already holds exactly that; creates parents.
fn write_if_changed(path: &Path, body: &str) -> anyhow::Result<Outcome> {
    if std::fs::read_to_string(path).ok().as_deref() == Some(body) {
        return Ok(Outcome::Unchanged);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(Outcome::Written)
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn remove_if_present(path: &Path) -> anyhow::Result<Outcome> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Outcome::Removed),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Outcome::Unchanged),
        Err(e) => Err(e.into()),
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use super::{desktop_entry, remove_if_present, write_if_changed, Locations, Outcome};

    pub(super) fn sync_autostart(loc: &Locations, enabled: bool) -> anyhow::Result<Outcome> {
        let path = loc.autostart_path();
        if enabled {
            write_if_changed(&path, &desktop_entry(&loc.exe))
        } else {
            remove_if_present(&path)
        }
    }
}

#[cfg(windows)]
mod platform {
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ,
    };

    use super::{Locations, Outcome};

    const RUN_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Run");
    const VALUE: PCWSTR = w!("PlayDrop");

    pub(super) fn sync_autostart(loc: &Locations, enabled: bool) -> anyhow::Result<Outcome> {
        let want = format!("\"{}\"", loc.exe.display());
        let current = read();
        if enabled {
            if current.as_deref() == Some(want.as_str()) {
                return Ok(Outcome::Unchanged);
            }
            let wide: Vec<u16> = want.encode_utf16().chain(std::iter::once(0)).collect();
            let bytes = u32::try_from(wide.len() * 2)?;
            // SAFETY: `wide` is a NUL-terminated UTF-16 buffer of `bytes` bytes that outlives
            // the call; key and value names are static wide strings.
            #[allow(unsafe_code)]
            let rc = unsafe {
                RegSetKeyValueW(
                    HKEY_CURRENT_USER,
                    RUN_KEY,
                    VALUE,
                    REG_SZ.0,
                    Some(wide.as_ptr().cast()),
                    bytes,
                )
            };
            if rc != ERROR_SUCCESS {
                anyhow::bail!("could not write the Run key: {rc:?}");
            }
            Ok(Outcome::Written)
        } else {
            if current.is_none() {
                return Ok(Outcome::Unchanged);
            }
            // SAFETY: static wide strings for key and value names.
            #[allow(unsafe_code)]
            let rc = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, VALUE) };
            if rc != ERROR_SUCCESS && rc != ERROR_FILE_NOT_FOUND {
                anyhow::bail!("could not remove the Run key value: {rc:?}");
            }
            Ok(Outcome::Removed)
        }
    }

    fn read() -> Option<String> {
        let mut buf = [0u16; 1024];
        let mut len = u32::try_from(std::mem::size_of_val(&buf)).ok()?;
        // SAFETY: `buf` is writable for `len` bytes and `len` is a valid in/out pointer;
        // RRF_RT_REG_SZ guarantees a NUL-terminated string on success.
        #[allow(unsafe_code)]
        let rc = unsafe {
            RegGetValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                VALUE,
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&raw mut len),
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let chars = (len as usize / 2).saturating_sub(1).min(buf.len());
        Some(String::from_utf16_lossy(&buf[..chars]))
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
mod platform {
    use super::{Locations, Outcome};

    pub(super) fn sync_autostart(_loc: &Locations, _enabled: bool) -> anyhow::Result<Outcome> {
        Ok(Outcome::Skipped)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn loc(tmp: &Path, flatpak: bool) -> Locations {
        Locations {
            config_home: tmp.join("config"),
            data_home: tmp.join("data"),
            exe: PathBuf::from("/opt/Play Drop/playdropd"),
            flatpak,
        }
    }

    #[test]
    fn autostart_entry_is_written_once_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let l = loc(tmp.path(), false);
        assert_eq!(sync_autostart(&l, true).unwrap(), Outcome::Written);
        assert_eq!(sync_autostart(&l, true).unwrap(), Outcome::Unchanged);
        let body = std::fs::read_to_string(l.autostart_path()).unwrap();
        assert!(
            body.contains("Exec=\"/opt/Play Drop/playdropd\"\n"),
            "{body}"
        );
        assert!(body.contains("X-GNOME-Autostart-enabled=true"));
        assert!(body.contains("Hidden=false"));
        assert_eq!(sync_autostart(&l, false).unwrap(), Outcome::Removed);
        assert_eq!(sync_autostart(&l, false).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn dbus_service_points_at_this_binary() {
        let tmp = tempfile::tempdir().unwrap();
        let l = loc(tmp.path(), false);
        assert_eq!(sync_dbus_service(&l).unwrap(), Outcome::Written);
        assert_eq!(sync_dbus_service(&l).unwrap(), Outcome::Unchanged);
        let body = std::fs::read_to_string(l.dbus_service_path()).unwrap();
        assert_eq!(
            body,
            "[D-BUS Service]\nName=org.playdrop.PlayDrop.Daemon1\nExec=\"/opt/Play Drop/playdropd\"\n"
        );
    }

    #[test]
    fn flatpak_is_left_to_packaging() {
        let tmp = tempfile::tempdir().unwrap();
        let l = loc(tmp.path(), true);
        assert_eq!(sync_autostart(&l, true).unwrap(), Outcome::Skipped);
        assert_eq!(sync_dbus_service(&l).unwrap(), Outcome::Skipped);
        assert!(!l.config_home.exists() && !l.data_home.exists());
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(exec_quote("/usr/bin/playdropd"), "/usr/bin/playdropd");
        assert_eq!(exec_quote("/a b/$x\"y"), "\"/a b/\\$x\\\"y\"");
    }
}
