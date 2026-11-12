//! The install step, behind [`Installer`] so tests replace the system prompt
//! (`docs/design/update-check.md` section 6, SR-47).
//!
//! - `.deb`: `pkexec /usr/bin/apt-get install` of the verified files by absolute path.
//!   `pkexec` shows the system's own password prompt; `PlayDrop` never sees the password
//!   and runs nothing elevated itself. `apt-get` reads an argument that contains a `/` and
//!   ends in `.deb` as a local package file (checked with `apt-get --simulate` on apt
//!   2.8.4), resolves its dependencies and refuses a broken package. `--no-remove` makes
//!   it refuse, rather than carry out, a plan that would remove any package (the COSMIC
//!   applet depends on `playdrop (= version)`, so updating the main package alone would
//!   otherwise remove it), and `DPkg::Lock::Timeout=60` waits up to a minute for a running
//!   unattended upgrade instead of failing at once.
//! - Windows: the verified installer is started through the shell (`ShellExecuteExW`),
//!   which runs Inno Setup's loader; the loader starts the elevated setup, so UAC appears.
//!   `/SILENT /RELAUNCH=1` (a window asked) or `/SILENT /RELAUNCH=daemon` (no window did)
//!   makes it stop the daemon and the window, replace them and start again what it should.
//!   The daemon waits for the loader to exit: a successful install stops this daemon first,
//!   so a daemon that sees the exit is one the installer did not replace, and it reports
//!   why from the exit code (Inno Setup's documented setup exit codes).

use std::path::{Path, PathBuf};

/// `pkexec` itself, by absolute path so `PATH` cannot point it elsewhere.
const PKEXEC: &str = "/usr/bin/pkexec";
/// `apt-get`, by absolute path: `pkexec` runs exactly this program.
const APT_GET: &str = "/usr/bin/apt-get";
/// `pkexec`: the user closed the authentication dialog.
const PKEXEC_DISMISSED: i32 = 126;
/// `pkexec`: not authorised, or no authentication agent could be reached.
const PKEXEC_NOT_AUTHORISED: i32 = 127;

/// The sentence for a dismissed password prompt.
pub const DISMISSED: &str = "The password prompt was closed; nothing was changed.";
/// The sentence for a refused one.
pub const NOT_AUTHORISED: &str =
    "This account may not install software, or no password prompt could be shown; nothing was changed.";

/// Why the install step failed, one plain sentence for the user.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct InstallFailure(pub String);

/// What the Windows installer starts again once it is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relaunch {
    /// The daemon and the window (`/RELAUNCH=1`): the update was started from the window.
    Window,
    /// The daemon only (`/RELAUNCH=daemon`): no window asked for the update.
    Daemon,
}

impl Relaunch {
    /// The installer's command line (`packaging/windows/playdrop.iss`).
    #[cfg_attr(
        not(windows),
        allow(dead_code, reason = "only Windows runs the installer")
    )]
    pub fn parameters(self) -> &'static str {
        match self {
            Self::Window => "/SILENT /RELAUNCH=1",
            Self::Daemon => "/SILENT /RELAUNCH=daemon",
        }
    }
}

/// Installs verified release files. Every call blocks until the OS prompt is answered.
pub trait Installer: Send + Sync {
    /// Installs `.deb` files (the main package, and the applet's when it is installed),
    /// all in one transaction.
    fn install_debs(&self, files: &[PathBuf]) -> Result<(), InstallFailure>;
    /// Runs the Windows installer and waits for it to exit. A successful install stops
    /// this daemon before it returns, so a return at all means the installer did not
    /// replace it: `Ok` for a clean exit, an error with the plain reason otherwise.
    fn run_windows_installer(&self, file: &Path, relaunch: Relaunch) -> Result<(), InstallFailure>;
}

/// The program and arguments for the `.deb` install, or why the files cannot be handed to
/// `apt-get`: every file must be an absolute path, since `apt-get` reads a bare name as a
/// package to fetch from the archive rather than as this local file.
///
/// `--no-install-recommends` so an update never pulls in more than the package needs.
/// `--no-remove` so a plan that would remove a package (the applet, if it was missed) is
/// refused instead of carried out. `DPkg::Lock::Timeout=60` waits for a running
/// unattended upgrade to let go of the package lock. The two `Dpkg::Options` keep a
/// configuration file the user changed (the autostart entry and the ufw profile under
/// `/etc` are conffiles) without asking: nothing can answer a question here, and the user's
/// own edit is the right answer.
pub fn deb_command(
    files: &[PathBuf],
) -> Result<(&'static str, Vec<std::ffi::OsString>), InstallFailure> {
    if files.is_empty() || files.iter().any(|f| !f.is_absolute()) {
        return Err(InstallFailure(
            "The update files are not where PlayDrop expected them; nothing was changed."
                .to_owned(),
        ));
    }
    let mut args: Vec<std::ffi::OsString> = [
        APT_GET,
        "install",
        "--yes",
        "--no-install-recommends",
        "--no-remove",
        "-o",
        "DPkg::Lock::Timeout=60",
        "-o",
        "Dpkg::Options::=--force-confdef",
        "-o",
        "Dpkg::Options::=--force-confold",
    ]
    .iter()
    .map(Into::into)
    .collect();
    args.extend(files.iter().map(|f| f.as_os_str().to_owned()));
    Ok((PKEXEC, args))
}

/// The sentence for the Windows installer's exit code, when this daemon is still running
/// after it (so it was not replaced). Codes from Inno Setup's documentation, "Setup Exit
/// Codes". A declined permission prompt ends the loader before the setup starts, which it
/// reports as 1 ("failed to initialize") as far as the documentation says; the sentence
/// names it as the likely cause rather than claiming it.
pub fn inno_exit_sentence(code: u32) -> String {
    match code {
        0 => "The installer ended without replacing PlayDrop; the current version keeps running."
            .to_owned(),
        1 => "The installer did not start (the permission prompt may have been declined); the current version keeps running."
            .to_owned(),
        2 | 5 => "The installer was cancelled; the current version keeps running.".to_owned(),
        7 | 8 => "The installer could not go ahead on this computer; the current version keeps running."
            .to_owned(),
        code => format!(
            "The installer failed (exit code {code}); the current version keeps running."
        ),
    }
}

/// The sentence for `pkexec`'s exit status: `None` when it was killed by a signal.
pub fn pkexec_failure(code: Option<i32>) -> String {
    match code {
        Some(PKEXEC_DISMISSED) => DISMISSED.to_owned(),
        Some(PKEXEC_NOT_AUTHORISED) => NOT_AUTHORISED.to_owned(),
        Some(code) => format!(
            "The package manager could not install the update (exit code {code}); the current version keeps running."
        ),
        None => "The package manager stopped before it finished; the current version keeps running."
            .to_owned(),
    }
}

/// The real system: `pkexec` on Linux, the shell on Windows.
pub struct SystemInstaller;

impl Installer for SystemInstaller {
    fn install_debs(&self, files: &[PathBuf]) -> Result<(), InstallFailure> {
        let (program, args) = deb_command(files)?;
        tracing::info!(files = files.len(), "asking pkexec to install the update");
        let output = std::process::Command::new(program)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| {
                InstallFailure(format!(
                    "The system password prompt (pkexec) could not be started: {e}. The current version keeps running."
                ))
            })?;
        if output.status.success() {
            tracing::info!("update installed");
            return Ok(());
        }
        // apt's own words help whoever reads the log; they hold package names and paths
        // under our data dir, nothing about the user.
        let stderr = String::from_utf8_lossy(&output.stderr);
        let tail: Vec<&str> = stderr.lines().rev().take(5).collect();
        tracing::warn!(code = ?output.status.code(), tail = ?tail, "update not installed");
        Err(InstallFailure(pkexec_failure(output.status.code())))
    }

    #[cfg(windows)]
    fn run_windows_installer(&self, file: &Path, relaunch: Relaunch) -> Result<(), InstallFailure> {
        windows_shell::run(file, relaunch)
    }

    #[cfg(not(windows))]
    fn run_windows_installer(
        &self,
        _file: &Path,
        _relaunch: Relaunch,
    ) -> Result<(), InstallFailure> {
        Err(InstallFailure(
            "The Windows installer can only run on Windows.".to_owned(),
        ))
    }
}

#[cfg(windows)]
mod windows_shell {
    use std::path::Path;

    use windows::core::{w, HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, ERROR_CANCELLED, HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{inno_exit_sentence, InstallFailure, Relaunch};

    /// The sentence for a closed UAC prompt, when the shell itself saw it.
    const UAC_DISMISSED: &str = "The permission prompt was closed; nothing was changed.";

    /// Closes the process handle however `run` ends.
    struct Owned(HANDLE);

    impl Drop for Owned {
        fn drop(&mut self) {
            // SAFETY: the handle came from `ShellExecuteExW` with `SEE_MASK_NOCLOSEPROCESS`
            // and is closed exactly once, here.
            #[allow(unsafe_code)]
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    /// Starts `file` through the shell with the installer's switches and waits for the
    /// process the shell started (Inno Setup's loader, which waits for the elevated setup
    /// and passes on its exit code). Blocks for as long as the installer runs.
    pub(super) fn run(file: &Path, relaunch: Relaunch) -> Result<(), InstallFailure> {
        let file = HSTRING::from(file.as_os_str());
        let params = HSTRING::from(relaunch.parameters());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>())
                .expect("the struct is small"),
            // Keep the process handle to wait on; finish the launch before returning; no
            // error boxes, the daemon reports errors itself.
            fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpVerb: w!("open"),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: `info` is a properly sized SHELLEXECUTEINFOW whose strings are
        // NUL-terminated wide strings that outlive the call (held in `file` and `params`,
        // or static); no window handle and no working folder are passed, which the API
        // allows.
        #[allow(unsafe_code)]
        let started = unsafe { ShellExecuteExW(&raw mut info) };
        if let Err(e) = started {
            tracing::warn!(error = %e, "update installer did not start");
            if e.code() == ERROR_CANCELLED.to_hresult() {
                return Err(InstallFailure(UAC_DISMISSED.to_owned()));
            }
            return Err(InstallFailure(format!(
                "The installer could not be started ({e}); the current version keeps running."
            )));
        }
        if info.hProcess.is_invalid() {
            // The shell handed the file to a process it did not tell us about; nothing
            // to wait for.
            return Err(InstallFailure(
                "The installer was started but PlayDrop cannot follow it; the current version keeps running."
                    .to_owned(),
            ));
        }
        let process = Owned(info.hProcess);
        tracing::info!("update installer started; waiting for it to end");
        // SAFETY: `process.0` is a valid process handle owned by `process`.
        #[allow(unsafe_code)]
        let waited = unsafe { WaitForSingleObject(process.0, INFINITE) };
        if waited != WAIT_OBJECT_0 {
            return Err(InstallFailure(
                "PlayDrop lost track of the installer; the current version keeps running."
                    .to_owned(),
            ));
        }
        let mut code = 0u32;
        // SAFETY: as above; `code` is a valid place for the exit code.
        #[allow(unsafe_code)]
        let read = unsafe { GetExitCodeProcess(process.0, &raw mut code) };
        if read.is_err() {
            return Err(InstallFailure(
                "PlayDrop could not read how the installer ended; the current version keeps running."
                    .to_owned(),
            ));
        }
        tracing::info!(
            code,
            "update installer ended and this daemon is still running"
        );
        if code == 0 {
            Ok(())
        } else {
            Err(InstallFailure(inno_exit_sentence(code)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deb_command_is_pkexec_apt_get_with_absolute_files() {
        let files = [
            PathBuf::from("/home/u/.local/share/PlayDrop/updates/1/playdrop-1.2.3-linux-amd64.deb"),
            PathBuf::from(
                "/home/u/.local/share/PlayDrop/updates/1/playdrop-cosmic-applet-1.2.3-linux-amd64.deb",
            ),
        ];
        let (program, args) = deb_command(&files).unwrap();
        assert_eq!(program, "/usr/bin/pkexec");
        let args: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "/usr/bin/apt-get",
                "install",
                "--yes",
                "--no-install-recommends",
                "--no-remove",
                "-o",
                "DPkg::Lock::Timeout=60",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
                "/home/u/.local/share/PlayDrop/updates/1/playdrop-1.2.3-linux-amd64.deb",
                "/home/u/.local/share/PlayDrop/updates/1/playdrop-cosmic-applet-1.2.3-linux-amd64.deb",
            ]
        );
    }

    #[test]
    fn relative_or_missing_files_are_never_handed_to_apt() {
        assert!(deb_command(&[]).is_err());
        assert!(deb_command(&[PathBuf::from("playdrop-1.2.3-linux-amd64.deb")]).is_err());
        assert!(deb_command(&[
            PathBuf::from("/tmp/a/playdrop-1.2.3-linux-amd64.deb"),
            PathBuf::from("updates/playdrop-cosmic-applet-1.2.3-linux-amd64.deb"),
        ])
        .is_err());
    }

    #[test]
    fn the_windows_installer_switches_and_exit_codes() {
        assert_eq!(Relaunch::Window.parameters(), "/SILENT /RELAUNCH=1");
        assert_eq!(Relaunch::Daemon.parameters(), "/SILENT /RELAUNCH=daemon");
        assert!(inno_exit_sentence(2).contains("cancelled"));
        assert!(inno_exit_sentence(5).contains("cancelled"));
        assert!(inno_exit_sentence(1).contains("permission prompt"));
        assert!(inno_exit_sentence(4).contains("exit code 4"));
        for code in [0, 1, 2, 3, 4, 5, 6, 7, 8, 99] {
            assert!(inno_exit_sentence(code).contains("keeps running"), "{code}");
        }
    }

    #[test]
    fn pkexec_exit_codes_become_plain_sentences() {
        assert_eq!(pkexec_failure(Some(126)), DISMISSED);
        assert_eq!(pkexec_failure(Some(127)), NOT_AUTHORISED);
        assert!(pkexec_failure(Some(100)).contains("exit code 100"));
        assert!(pkexec_failure(None).contains("keeps running"));
    }
}
