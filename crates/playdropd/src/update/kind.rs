//! How this copy of `PlayDrop` was installed, which decides what "Update" can do
//! (`docs/design/update-check.md` section 1).
//!
//! The decision is a pure function of a few facts ([`decide`]); [`Probe`] gathers them, and
//! tests hand in a fake one. Only a copy whose files the update would really replace may
//! install: the `.deb` is trusted only when the running program is the packaged
//! `/usr/bin/playdropd` on amd64 and `dpkg` says the package is installed, and the Windows
//! installer only when its uninstaller sits beside the program. Anything else (a source
//! build, a copied binary, another architecture) gets the notice with the release page.

use std::path::{Path, PathBuf};

use playdrop_core::update::InstallKind;

/// The packaged daemon's path on Debian-family systems.
pub const DEB_DAEMON: &str = "/usr/bin/playdropd";
/// The main package's name.
pub const DEB_PACKAGE: &str = "playdrop";
/// The optional COSMIC applet's package name.
pub const APPLET_PACKAGE: &str = "playdrop-cosmic-applet";
/// Inno Setup's uninstaller, written beside the installed program.
const WINDOWS_UNINSTALLER: &str = "unins000.exe";

/// Facts about this process that decide the install kind.
pub trait Probe {
    /// The file a Flatpak sandbox marks itself with exists (`/.flatpak-info`).
    fn in_flatpak(&self) -> bool;
    /// The running program's path.
    fn exe(&self) -> Option<PathBuf>;
    /// `std::env::consts::OS`.
    fn os(&self) -> &str;
    /// `std::env::consts::ARCH`.
    fn arch(&self) -> &str;
    /// Whether `dpkg` reports `package` as installed.
    fn deb_installed(&self, package: &str) -> bool;
    /// Whether `path` exists.
    fn exists(&self, path: &Path) -> bool;
}

/// What the probes found: the kind, and for a `.deb` whether the applet is installed too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Install {
    /// How `PlayDrop` was installed.
    pub kind: InstallKind,
    /// The `.deb` of the COSMIC applet is installed and is updated with the main one.
    pub applet: bool,
}

/// Decides the install kind from `probe`. `dpkg` is only asked when the program is the
/// packaged one, so a source build never runs it.
pub fn decide(probe: &dyn Probe) -> Install {
    let other = Install {
        kind: InstallKind::Other,
        applet: false,
    };
    if probe.os() == "linux" && probe.in_flatpak() {
        return Install {
            kind: InstallKind::Flatpak,
            applet: false,
        };
    }
    let Some(exe) = probe.exe() else {
        return other;
    };
    if probe.arch() != "x86_64" {
        return other;
    }
    match probe.os() {
        "linux" if exe == Path::new(DEB_DAEMON) && probe.deb_installed(DEB_PACKAGE) => Install {
            kind: InstallKind::Deb,
            applet: probe.deb_installed(APPLET_PACKAGE),
        },
        "windows"
            if exe
                .parent()
                .is_some_and(|dir| probe.exists(&dir.join(WINDOWS_UNINSTALLER))) =>
        {
            Install {
                kind: InstallKind::Windows,
                applet: false,
            }
        }
        _ => other,
    }
}

/// Reads `dpkg-query -W -f='${Status}'` output: installed when the third word is
/// `installed` ("install ok installed"); "deinstall ok config-files" and the like are not.
pub fn dpkg_status_installed(output: &str) -> bool {
    output.split_whitespace().nth(2) == Some("installed")
}

/// Asks `dpkg-query` whether `package` is installed, telling "not installed" apart from
/// "could not tell". The update's install step relies on the difference: reading a failed
/// question as "not installed" would leave the applet out of the update, and `apt-get` would
/// then remove it (it depends on `playdrop (= version)`).
///
/// `dpkg-query` exits 0 with the status line for a package it knows, 1 when it knows no
/// such package (dpkg-query(1), "EXIT STATUS"), and 2 on a fatal error.
pub fn dpkg_query(package: &str) -> Result<bool, String> {
    // An absolute path, so `PATH` cannot point it elsewhere.
    let output = std::process::Command::new("/usr/bin/dpkg-query")
        .args(["-W", "-f=${Status}", package])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("dpkg-query could not be started: {e}"))?;
    dpkg_answer(
        output.status.code(),
        &String::from_utf8_lossy(&output.stdout),
    )
}

/// [`dpkg_query`]'s reading of an exit code (`None`: killed by a signal) and output.
pub fn dpkg_answer(code: Option<i32>, stdout: &str) -> Result<bool, String> {
    match code {
        Some(0) => Ok(dpkg_status_installed(stdout)),
        Some(1) => Ok(false),
        Some(code) => Err(format!("dpkg-query failed (exit code {code})")),
        None => Err("dpkg-query was stopped".to_owned()),
    }
}

/// Whether the COSMIC applet's package is installed right now (asked again at install time,
/// never taken from the answer cached when the offer was made).
pub fn applet_installed_now() -> Result<bool, String> {
    dpkg_query(APPLET_PACKAGE)
}

/// The running program's path. Linux appends " (deleted)" once the file was replaced (for
/// example by an `apt` upgrade while the daemon runs); the path it names is still the
/// install's.
fn running_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let text = exe.to_str()?;
    Some(PathBuf::from(
        text.strip_suffix(" (deleted)").unwrap_or(text),
    ))
}

/// The real system.
pub struct SystemProbe;

impl Probe for SystemProbe {
    fn in_flatpak(&self) -> bool {
        Path::new("/.flatpak-info").exists()
    }

    fn exe(&self) -> Option<PathBuf> {
        running_exe()
    }

    fn os(&self) -> &str {
        std::env::consts::OS
    }

    fn arch(&self) -> &str {
        std::env::consts::ARCH
    }

    fn deb_installed(&self, package: &str) -> bool {
        // Here the answer only decides what the offer says (and a missing `dpkg-query`, on
        // a system that is not Debian, means "not a .deb"); the install step asks again
        // with [`applet_installed_now`] and stops when the question fails.
        dpkg_query(package).unwrap_or(false)
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake {
        flatpak: bool,
        exe: Option<&'static str>,
        os: &'static str,
        arch: &'static str,
        debs: &'static [&'static str],
        files: Vec<PathBuf>,
        asked: std::cell::RefCell<Vec<String>>,
    }

    impl Fake {
        fn linux(exe: &'static str, debs: &'static [&'static str]) -> Self {
            Self {
                flatpak: false,
                exe: Some(exe),
                os: "linux",
                arch: "x86_64",
                debs,
                files: Vec::new(),
                asked: std::cell::RefCell::default(),
            }
        }
    }

    impl Probe for Fake {
        fn in_flatpak(&self) -> bool {
            self.flatpak
        }
        fn exe(&self) -> Option<PathBuf> {
            self.exe.map(PathBuf::from)
        }
        fn os(&self) -> &str {
            self.os
        }
        fn arch(&self) -> &str {
            self.arch
        }
        fn deb_installed(&self, package: &str) -> bool {
            self.asked.borrow_mut().push(package.to_owned());
            self.debs.contains(&package)
        }
        fn exists(&self, path: &Path) -> bool {
            self.files.iter().any(|f| f == path)
        }
    }

    fn kind(p: &Fake) -> InstallKind {
        decide(p).kind
    }

    #[test]
    fn the_packaged_daemon_on_amd64_is_a_deb_with_or_without_the_applet() {
        let p = Fake::linux(DEB_DAEMON, &[DEB_PACKAGE]);
        assert_eq!(
            decide(&p),
            Install {
                kind: InstallKind::Deb,
                applet: false
            }
        );
        let p = Fake::linux(DEB_DAEMON, &[DEB_PACKAGE, APPLET_PACKAGE]);
        assert_eq!(
            decide(&p),
            Install {
                kind: InstallKind::Deb,
                applet: true
            }
        );
    }

    #[test]
    fn anything_short_of_the_packaged_daemon_gets_a_notice() {
        // A source build never asks dpkg.
        let p = Fake::linux("/home/me/PlayDrop/target/release/playdropd", &[DEB_PACKAGE]);
        assert_eq!(kind(&p), InstallKind::Other);
        assert!(p.asked.borrow().is_empty());
        // The path is right but dpkg does not know the package (copied by hand).
        assert_eq!(kind(&Fake::linux(DEB_DAEMON, &[])), InstallKind::Other);
        // Another architecture.
        let mut p = Fake::linux(DEB_DAEMON, &[DEB_PACKAGE]);
        p.arch = "aarch64";
        assert_eq!(kind(&p), InstallKind::Other);
        let mut p = Fake::linux(DEB_DAEMON, &[DEB_PACKAGE]);
        p.exe = None;
        assert_eq!(kind(&p), InstallKind::Other);
    }

    #[test]
    fn the_flatpak_wins_over_everything() {
        let mut p = Fake::linux(DEB_DAEMON, &[DEB_PACKAGE]);
        p.flatpak = true;
        assert_eq!(kind(&p), InstallKind::Flatpak);
        assert!(p.asked.borrow().is_empty());
    }

    #[test]
    fn windows_needs_the_uninstaller_beside_the_program_and_x64() {
        let mut p = Fake::linux(r"C:\Program Files\PlayDrop\playdropd.exe", &[]);
        p.os = "windows";
        assert_eq!(kind(&p), InstallKind::Other);
        // Joined the way `decide` joins it, so the test holds on any host's path rules.
        let exe = PathBuf::from(r"C:\Program Files\PlayDrop\playdropd.exe");
        p.files = vec![exe.parent().expect("parent").join(WINDOWS_UNINSTALLER)];
        assert_eq!(kind(&p), InstallKind::Windows);
        p.arch = "aarch64";
        assert_eq!(kind(&p), InstallKind::Other);
    }

    #[test]
    fn a_failed_dpkg_question_is_not_a_no() {
        assert_eq!(dpkg_answer(Some(0), "install ok installed"), Ok(true));
        assert_eq!(dpkg_answer(Some(0), "deinstall ok config-files"), Ok(false));
        assert_eq!(dpkg_answer(Some(1), ""), Ok(false));
        assert!(dpkg_answer(Some(2), "").is_err());
        assert!(dpkg_answer(None, "").is_err());
    }

    #[test]
    fn dpkg_status_lines() {
        assert!(dpkg_status_installed("install ok installed"));
        assert!(!dpkg_status_installed("deinstall ok config-files"));
        assert!(!dpkg_status_installed("install ok half-configured"));
        assert!(!dpkg_status_installed(""));
    }
}
