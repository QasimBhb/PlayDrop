//! Persisted daemon settings: `settings.json` in the data dir, mode 0600, written atomically
//! (temp file, fsync, rename) so a crash mid-write never leaves a half file behind.
//!
//! Two copies live at run time. The *persisted* settings are what is on disk. The
//! *effective* settings are the persisted ones with this run's command line flags laid over
//! them (`cli.rs`: flags apply to this run only unless `--write-settings` is given). A
//! change from a UI (`Request::SetSettings`) is applied to both and saved, so it wins over
//! a flag for the rest of the run and sticks afterwards.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};

use crate::cli::Args;

/// File name inside the data dir.
pub const SETTINGS_FILE: &str = "settings.json";

/// Name used when the hostname cannot be read or sanitises to nothing.
const FALLBACK_NAME: &str = "PlayDrop desktop";

/// Daemon settings as stored on disk. Unknown fields are ignored and missing ones take
/// their default, so older and newer daemons can share one file.
// Each bool is an independent user toggle, not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Folder received files land in (SR-23).
    pub receive_dir: PathBuf,
    /// Advertised device name (SR-8), sanitised by the core.
    pub device_name: String,
    /// TCP and UDP port (protocol.md section 1).
    pub port: u16,
    /// Watch the local clipboard and push changes to peers.
    pub clipboard_sync: bool,
    /// Sync clipboard images (PNG, at most 16 MiB) as well as text, both ways.
    pub clipboard_images: bool,
    /// Start the daemon at login.
    pub autostart: bool,
    /// Show desktop notifications.
    pub notifications: bool,
    /// mDNS and the beacon; off means last-known dial only.
    pub discovery: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            receive_dir: default_receive_dir(),
            device_name: default_device_name(),
            port: playdrop_core::constants::TCP_PORT,
            clipboard_sync: true,
            clipboard_images: true,
            autostart: true,
            notifications: true,
            discovery: true,
        }
    }
}

/// What applying a [`playdrop_ipc::SettingsPatch`] changed, so the daemon knows which side
/// effects to run.
// One flag per independent side effect.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Changes {
    /// `receive_dir` or `device_name` changed: the host must restart.
    pub restart_host: bool,
    /// `clipboard_sync` changed.
    pub clipboard_sync: bool,
    /// `clipboard_images` changed.
    pub clipboard_images: bool,
    /// `autostart` changed.
    pub autostart: bool,
    /// `notifications` changed.
    pub notifications: bool,
}

impl Changes {
    /// True if anything changed at all.
    pub fn any(self) -> bool {
        self.restart_host
            || self.clipboard_sync
            || self.clipboard_images
            || self.autostart
            || self.notifications
    }
}

/// A patch the daemon refuses, worded for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    /// The receive folder must be an absolute path.
    #[error("the receive folder must be an absolute path")]
    RelativeReceiveDir,
    /// The device name is empty after removing control and direction characters (SR-8).
    #[error("the device name is empty")]
    EmptyName,
}

impl Settings {
    /// The view a UI reads. `clipboard_watch_supported` comes from the clipboard backend.
    pub fn to_ipc(&self, clipboard_watch_supported: bool) -> playdrop_ipc::Settings {
        playdrop_ipc::Settings {
            receive_dir: self.receive_dir.clone(),
            device_name: self.device_name.clone(),
            clipboard_sync: self.clipboard_sync,
            clipboard_images: self.clipboard_images,
            clipboard_watch_supported,
            // Filled in by the daemon from the input backend probe (`input::support()`).
            remote_input_supported: false,
            remote_input_reason: None,
            autostart: self.autostart,
            notifications: self.notifications,
        }
    }

    /// Applies a UI patch. Fields left `None` keep their value; the device name is
    /// sanitised the way the core will sanitise it, so "changed" means changed on the wire.
    /// Nothing is applied when the patch is refused.
    pub fn apply_patch(
        &mut self,
        patch: &playdrop_ipc::SettingsPatch,
    ) -> Result<Changes, PatchError> {
        let name = match &patch.device_name {
            Some(raw) => {
                let clean = playdrop_core::session::sanitize_name(raw);
                if clean.is_empty() {
                    return Err(PatchError::EmptyName);
                }
                Some(clean)
            }
            None => None,
        };
        if let Some(dir) = &patch.receive_dir {
            if !dir.is_absolute() {
                return Err(PatchError::RelativeReceiveDir);
            }
        }
        let mut c = Changes::default();
        if let Some(dir) = &patch.receive_dir {
            if *dir != self.receive_dir {
                self.receive_dir.clone_from(dir);
                c.restart_host = true;
            }
        }
        if let Some(name) = name {
            if name != self.device_name {
                self.device_name = name;
                c.restart_host = true;
            }
        }
        if let Some(v) = patch.clipboard_sync {
            c.clipboard_sync = v != self.clipboard_sync;
            self.clipboard_sync = v;
        }
        if let Some(v) = patch.clipboard_images {
            c.clipboard_images = v != self.clipboard_images;
            self.clipboard_images = v;
        }
        if let Some(v) = patch.autostart {
            c.autostart = v != self.autostart;
            self.autostart = v;
        }
        if let Some(v) = patch.notifications {
            c.notifications = v != self.notifications;
            self.notifications = v;
        }
        Ok(c)
    }

    /// Lays this run's command line flags over the settings. The `--no-*` flags only turn
    /// things off; a flag that is absent leaves the setting alone.
    pub fn apply_args(&mut self, args: &Args) {
        if let Some(dir) = &args.receive_dir {
            self.receive_dir.clone_from(dir);
        }
        if let Some(name) = &args.name {
            let clean = playdrop_core::session::sanitize_name(name);
            if !clean.is_empty() {
                self.device_name = clean;
            }
        }
        if let Some(port) = args.port {
            self.port = port;
        }
        if args.no_clipboard {
            self.clipboard_sync = false;
        }
        if args.no_notifications {
            self.notifications = false;
        }
        if args.no_discovery {
            self.discovery = false;
        }
    }
}

/// Settings read from disk.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// The settings (defaults if the file was missing or unreadable).
    pub settings: Settings,
    /// False when no settings file existed: this is the first run.
    pub existed: bool,
}

/// Reads `settings.json` from `data_dir`. A missing file gives the defaults and
/// `existed: false`. A file that does not parse is logged and replaced by the defaults in
/// memory only; it is left on disk for the user to inspect until the next save.
pub fn load(data_dir: &Path) -> anyhow::Result<Loaded> {
    let path = data_dir.join(SETTINGS_FILE);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Loaded {
                settings: Settings::default(),
                existed: false,
            })
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let settings = match serde_json::from_slice::<Settings>(&bytes) {
        Ok(mut s) => {
            s.device_name = playdrop_core::session::sanitize_name(&s.device_name);
            if s.device_name.is_empty() {
                s.device_name = default_device_name();
            }
            s
        }
        Err(e) => {
            tracing::warn!(error = %e, file = %path.display(), "settings file does not parse; using defaults");
            Settings::default()
        }
    };
    Ok(Loaded {
        settings,
        existed: true,
    })
}

/// Writes `settings.json` atomically with mode 0600, creating the data dir (0700) if needed.
pub fn save(data_dir: &Path, settings: &Settings) -> anyhow::Result<()> {
    create_private_dir(data_dir)?;
    let path = data_dir.join(SETTINGS_FILE);
    let tmp = data_dir.join(format!("{SETTINGS_FILE}.tmp-{}", std::process::id()));
    let mut json = serde_json::to_vec_pretty(settings).context("encoding settings")?;
    json.push(b'\n');
    let write = || -> std::io::Result<()> {
        let mut f = private_file(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &path)?;
        sync_dir(data_dir);
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("writing {}", path.display()));
    }
    Ok(())
}

#[cfg(unix)]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
}

// TODO(SR-28): on Windows the data dir sits under %LOCALAPPDATA%, which only the user can
// read by default; an explicit DACL is Phase 2 follow-up work shared with the core.
#[cfg(not(unix))]
fn private_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::DirBuilderExt as _;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))
}

/// Makes the rename durable. Best effort: some filesystems refuse to open a directory.
#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

/// The daemon's data dir when `--data-dir` is not given: the core's
/// `identity::default_dir()`, which is `~/.local/share/PlayDrop` on Linux and
/// `%LOCALAPPDATA%\PlayDrop\data` on Windows (the `directories` crate's layout).
pub fn default_data_dir() -> Option<PathBuf> {
    playdrop_core::identity::default_dir()
}

/// `~/Downloads/PlayDrop` (the XDG or Known Folder download dir), falling back to
/// `~/Downloads/PlayDrop` under the home dir, then to `./PlayDrop-received`.
pub fn default_receive_dir() -> PathBuf {
    if let Some(u) = directories::UserDirs::new() {
        if let Some(d) = u.download_dir() {
            return d.join("PlayDrop");
        }
        return u.home_dir().join("Downloads").join("PlayDrop");
    }
    PathBuf::from("PlayDrop-received")
}

/// The hostname, sanitised as the core will sanitise it (SR-8).
pub fn default_device_name() -> String {
    let clean = playdrop_core::session::sanitize_name(&hostname().unwrap_or_default());
    if clean.is_empty() {
        FALLBACK_NAME.to_owned()
    } else {
        clean
    }
}

#[cfg(unix)]
fn hostname() -> Option<String> {
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok())
}

#[cfg(windows)]
fn hostname() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(not(any(unix, windows)))]
fn hostname() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;
    use playdrop_ipc::SettingsPatch;

    /// An absolute receive folder on every platform: a bare `/srv/in` has no drive on
    /// Windows and counts as relative there.
    fn abs_dir(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join("playdrop-settings-test")
            .join(name)
    }

    fn sample() -> Settings {
        Settings {
            receive_dir: abs_dir("in"),
            device_name: "desk".into(),
            port: 42187,
            clipboard_sync: true,
            clipboard_images: true,
            autostart: true,
            notifications: true,
            discovery: true,
        }
    }

    #[test]
    fn round_trips_through_disk_with_private_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let first = load(&dir).unwrap();
        assert!(!first.existed);

        let mut s = sample();
        s.port = 42190;
        s.notifications = false;
        save(&dir, &s).unwrap();
        let back = load(&dir).unwrap();
        assert!(back.existed);
        assert_eq!(back.settings, s);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(dir.join(SETTINGS_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dmode = std::fs::metadata(&dir).unwrap().permissions().mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn missing_fields_take_defaults_and_garbage_falls_back() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(SETTINGS_FILE),
            br#"{"port": 42999, "future": 1}"#,
        )
        .unwrap();
        let s = load(tmp.path()).unwrap().settings;
        assert_eq!(s.port, 42999);
        assert!(s.clipboard_sync && s.autostart && s.notifications && s.discovery);
        assert!(
            s.clipboard_images,
            "a file from before images defaults them on"
        );

        std::fs::write(tmp.path().join(SETTINGS_FILE), b"{not json").unwrap();
        let l = load(tmp.path()).unwrap();
        assert!(l.existed);
        assert_eq!(l.settings.port, playdrop_core::constants::TCP_PORT);
    }

    #[test]
    fn patch_reports_what_changed() {
        let mut s = sample();
        let c = s.apply_patch(&SettingsPatch::default()).unwrap();
        assert!(!c.any());

        let c = s
            .apply_patch(&SettingsPatch {
                notifications: Some(false),
                clipboard_sync: Some(true),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert!(c.notifications && !c.clipboard_sync && !c.restart_host);
        assert!(!s.notifications);

        let c = s
            .apply_patch(&SettingsPatch {
                clipboard_images: Some(false),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert!(c.clipboard_images && c.any() && !s.clipboard_images);

        let c = s
            .apply_patch(&SettingsPatch {
                device_name: Some("  desk\u{202E}  ".into()),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert!(!c.restart_host, "same name after sanitising");

        let c = s
            .apply_patch(&SettingsPatch {
                device_name: Some("Work laptop".into()),
                receive_dir: Some(abs_dir("in")),
                ..SettingsPatch::default()
            })
            .unwrap();
        assert!(c.restart_host);
        assert_eq!(s.device_name, "Work laptop");
    }

    #[test]
    fn bad_patches_change_nothing() {
        let mut s = sample();
        let before = s.clone();
        assert_eq!(
            s.apply_patch(&SettingsPatch {
                receive_dir: Some(PathBuf::from("relative/dir")),
                notifications: Some(false),
                ..SettingsPatch::default()
            }),
            Err(PatchError::RelativeReceiveDir)
        );
        assert_eq!(
            s.apply_patch(&SettingsPatch {
                device_name: Some("\u{200E}\n".into()),
                ..SettingsPatch::default()
            }),
            Err(PatchError::EmptyName)
        );
        assert_eq!(s, before);
    }

    #[test]
    fn ipc_view_and_flags() {
        let mut s = sample();
        let v = s.to_ipc(false);
        assert_eq!(v.receive_dir, s.receive_dir);
        assert_eq!(v.device_name, "desk");
        assert!(!v.clipboard_watch_supported);

        let args = Args::parse_from([
            "playdropd",
            "--port",
            "42300",
            "--name",
            "flagged",
            "--no-clipboard",
            "--no-discovery",
        ]);
        s.apply_args(&args);
        assert_eq!(s.port, 42300);
        assert_eq!(s.device_name, "flagged");
        assert!(!s.clipboard_sync && !s.discovery && s.notifications);
    }
}
