//! Linux process checks shared by the D-Bus server and client (SR-26).
//!
//! Everything here reads procfs through safe std or rustix calls; no unsafe code.

use std::io;
use std::path::{Path, PathBuf};

use crate::FLATPAK_APP_ID;

/// Largest `.flatpak-info` we read. Real files are a few hundred bytes.
const FLATPAK_INFO_MAX: u64 = 64 * 1024;

/// The daemon binary's file name; the client refuses a name owner running anything else.
pub(crate) const DAEMON_EXE_NAME: &str = "playdropd";

/// Where Flatpak puts its instance info inside a sandbox.
const SELF_FLATPAK_INFO: &str = "/.flatpak-info";

/// Whether this process runs inside a Flatpak sandbox. Each Flatpak instance has its own
/// PID namespace and cannot resolve the host PIDs the bus reports, so the `/proc/<pid>`
/// checks are skipped on a sandboxed side (the UID check stays; `docs/ipc.md` section 2).
/// The skip is logged once at info.
pub(crate) fn self_sandboxed() -> bool {
    static SANDBOXED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SANDBOXED.get_or_init(|| {
        let sandboxed = sandboxed_by(Path::new(SELF_FLATPAK_INFO));
        if sandboxed {
            tracing::info!("running inside Flatpak: skipping /proc/<pid> checks, UID check stays");
        }
        sandboxed
    })
}

fn sandboxed_by(probe: &Path) -> bool {
    probe.exists()
}

/// Our own real UID.
pub(crate) fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// The sandbox a caller runs in, as far as `/proc/<pid>/root/.flatpak-info` tells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Sandbox {
    /// No `.flatpak-info`: an ordinary process of the same user.
    None,
    /// A Flatpak app; the ID is empty when the file names none.
    Flatpak(String),
}

impl Sandbox {
    /// Whether SR-26 lets this caller in: unsandboxed, or our own Flatpak app.
    pub(crate) fn allowed(&self) -> bool {
        match self {
            Self::None => true,
            Self::Flatpak(app) => app == FLATPAK_APP_ID,
        }
    }
}

/// Read the sandbox of process `pid`.
///
/// A missing `/proc/<pid>` (the caller exited, or lives in a PID namespace we cannot see) is
/// an error, so the caller is refused. A missing `.flatpak-info` under an existing root means
/// no sandbox. Any other read failure is an error as well: the check fails closed.
pub(crate) fn caller_sandbox(pid: u32) -> io::Result<Sandbox> {
    caller_sandbox_in(Path::new("/proc"), pid)
}

fn caller_sandbox_in(proc_root: &Path, pid: u32) -> io::Result<Sandbox> {
    use std::io::Read as _;

    let process = proc_root.join(pid.to_string());
    // Fails with NotFound when the process is gone; that must not read as "unsandboxed".
    std::fs::metadata(&process)?;
    let info = process.join("root").join(".flatpak-info");
    let file = match std::fs::File::open(&info) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Sandbox::None),
        Err(e) => return Err(e),
    };
    let mut contents = String::new();
    file.take(FLATPAK_INFO_MAX).read_to_string(&mut contents)?;
    Ok(Sandbox::Flatpak(
        flatpak_app_name(&contents).unwrap_or_default().to_owned(),
    ))
}

/// The `name=` key of the `[Application]` group of a `.flatpak-info` keyfile.
pub(crate) fn flatpak_app_name(contents: &str) -> Option<&str> {
    let mut in_application = false;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(group) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_application = group == "Application";
            continue;
        }
        if in_application {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "name" {
                    return Some(value.trim());
                }
            }
        }
    }
    None
}

/// The executable of process `pid`, from `readlink /proc/<pid>/exe`.
pub(crate) fn process_exe(pid: u32) -> io::Result<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe"))
}

/// Whether an `exe` link points at a `playdropd` binary. A binary replaced by a package
/// upgrade while the daemon runs reads as `<path> (deleted)`; that still names our daemon.
pub(crate) fn is_daemon_exe(exe: &Path) -> bool {
    let text = exe.to_string_lossy();
    let path = text.strip_suffix(" (deleted)").unwrap_or(&text);
    Path::new(path)
        .file_name()
        .is_some_and(|name| name == DAEMON_EXE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OURS: &str = "[Application]\nname=org.playdrop.PlayDrop\nruntime=runtime/org.freedesktop.Platform/x86_64/24.08\n\n[Instance]\ninstance-id=123\n";
    const OTHER: &str =
        "[Application]\nname=com.example.Evil\n[Instance]\nname=org.playdrop.PlayDrop\n";

    #[test]
    fn parses_application_name() {
        assert_eq!(flatpak_app_name(OURS), Some("org.playdrop.PlayDrop"));
        assert_eq!(flatpak_app_name(OTHER), Some("com.example.Evil"));
        assert_eq!(flatpak_app_name("[Instance]\nname=x\n"), None);
        assert_eq!(
            flatpak_app_name("# comment\n[Application]\n  name = spaced.App  \n"),
            Some("spaced.App")
        );
    }

    fn fake_proc(contents: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("42").join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        if let Some(contents) = contents {
            std::fs::write(root.join(".flatpak-info"), contents).expect("write");
        }
        dir
    }

    #[test]
    fn our_flatpak_is_allowed_and_others_are_not() {
        let ours = fake_proc(Some(OURS));
        let sandbox = caller_sandbox_in(ours.path(), 42).expect("read");
        assert_eq!(sandbox, Sandbox::Flatpak(FLATPAK_APP_ID.to_owned()));
        assert!(sandbox.allowed());

        let other = fake_proc(Some(OTHER));
        let sandbox = caller_sandbox_in(other.path(), 42).expect("read");
        assert_eq!(sandbox, Sandbox::Flatpak("com.example.Evil".to_owned()));
        assert!(!sandbox.allowed());

        let nameless = fake_proc(Some("[Instance]\n"));
        let sandbox = caller_sandbox_in(nameless.path(), 42).expect("read");
        assert!(!sandbox.allowed());
    }

    #[test]
    fn missing_info_is_unsandboxed_but_missing_process_is_an_error() {
        let plain = fake_proc(None);
        assert_eq!(
            caller_sandbox_in(plain.path(), 42).expect("read"),
            Sandbox::None
        );
        assert!(caller_sandbox_in(plain.path(), 43).is_err());
    }

    #[test]
    fn our_own_process_reads_as_unsandboxed() {
        let sandbox = caller_sandbox(std::process::id()).expect("read self");
        // The test suite itself does not run inside a Flatpak.
        assert_eq!(sandbox, Sandbox::None);
    }

    #[test]
    fn sandbox_probe() {
        let dir = tempfile::tempdir().expect("tempdir");
        let probe = dir.path().join(".flatpak-info");
        assert!(!sandboxed_by(&probe));
        std::fs::write(&probe, OURS).expect("write");
        assert!(sandboxed_by(&probe));
    }

    #[test]
    fn daemon_exe_names() {
        assert!(is_daemon_exe(Path::new("/usr/bin/playdropd")));
        assert!(is_daemon_exe(Path::new("/app/bin/playdropd")));
        assert!(is_daemon_exe(Path::new("/usr/bin/playdropd (deleted)")));
        assert!(!is_daemon_exe(Path::new("/usr/bin/playdropd-evil")));
        assert!(!is_daemon_exe(Path::new("/tmp/notplaydropd")));
        assert!(!is_daemon_exe(Path::new("/usr/bin/python3")));
        // The test binary is readable through procfs and is not the daemon.
        let own = process_exe(std::process::id()).expect("own exe readable");
        assert!(!is_daemon_exe(&own));
    }
}
