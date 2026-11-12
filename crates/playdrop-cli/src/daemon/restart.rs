//! `playdrop restart`: stop the running daemon and start the installed one again, for
//! after a package was installed by hand (`sudo apt install ./playdrop-<ver>.deb` leaves
//! the old daemon running; the built-in update restarts it by itself).
//!
//! 1. Connect without starting anything and note the daemon's version. Nobody running is
//!    fine: the command then only starts it.
//! 2. Note whether it runs as the systemd user unit `playdropd.service`. Only one daemon can
//!    own the bus name, so an active unit means the running daemon is the unit's.
//! 3. Ask it to shut down (`Request::Shutdown`) and wait until its name or pipe is gone.
//! 4. Start it again the way this session runs it, first that works:
//!    - the unit, with `systemctl --user start`, when the old daemon was the unit's;
//!    - D-Bus activation (Linux, the Flatpak included): the bus starts whatever its service
//!      file names, which is how the window starts it, and through the unit when the
//!      system file's `SystemdService=` applies. A daemon the bus activated without the
//!      unit (the daemon's own per-user service file has no `SystemdService=`) comes back
//!      the same way, which `systemctl --user restart` cannot do;
//!    - a plain start of `playdropd` installed beside this command, detached from the
//!      terminal (Windows, where `playdrop.exe` sits in `bin\` under the install folder;
//!      Linux without a service file), as the login autostart would.
//! 5. Wait until the new daemon answers and print both versions.
//!
//! A daemon started by hand with options (`playdropd --data-dir ...`) comes back without
//! them: the command cannot see another process's arguments.
//!
//! The steps go through [`Daemon`], so the tests drive a fake daemon.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use playdrop_ipc::client::{Client, ConnectError};
use playdrop_ipc::{ErrorCode, Request};
use serde::Serialize;

use super::{output, Exit, Failure};

/// Options of `playdrop restart`.
#[derive(Debug, clap::Args)]
pub struct RestartArgs {
    /// Print the old and new version and how the daemon was started as one JSON object.
    #[arg(long)]
    pub json: bool,
}

/// How the daemon was started again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPath {
    /// `systemctl --user start playdropd.service`.
    Systemd,
    /// The session bus started it (D-Bus activation).
    Activation,
    /// This command ran `playdropd`.
    Spawn,
}

/// What a restart did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Report {
    /// The version that was running, `None` when no daemon was.
    pub old_version: Option<String>,
    /// The version running now.
    pub new_version: String,
    /// How it was started.
    pub started_by: StartPath,
}

/// How long each wait may take and how often it looks.
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    /// For the old daemon to go away after it was asked to.
    pub stop: Duration,
    /// For the new one to answer after it was started.
    pub start: Duration,
    /// Between two looks.
    pub poll: Duration,
}

impl Timing {
    /// The real waits. A daemon stops its host and IPC server in well under a second; a
    /// slow disk or a busy session gets far more room than that.
    pub const REAL: Self = Self {
        stop: Duration::from_secs(30),
        start: Duration::from_secs(30),
        poll: Duration::from_millis(100),
    };
}

/// The steps of a restart, so tests can stand in for the daemon and the system.
pub trait Daemon {
    /// The running daemon's version, `None` when nobody runs it. Never starts one.
    async fn version(&mut self) -> Result<Option<String>, Failure>;
    /// Asks the running daemon to shut down.
    async fn shutdown(&mut self) -> Result<(), Failure>;
    /// Whether the running daemon is the systemd user unit's process.
    async fn under_systemd(&mut self) -> bool;
    /// Starts the daemon by `path`: `Ok(false)` when that way is not available here, so the
    /// next one is tried.
    async fn start(&mut self, path: StartPath) -> Result<bool, Failure>;
    /// For a daemon this command ran: how it ended once it has exited, `None` while it runs
    /// or when this command did not run one.
    fn exited(&mut self) -> Option<Ended>;
}

/// How a daemon this command ran ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// It exited with this code.
    Code(i32),
    /// A signal ended it.
    Signal,
}

/// Runs `playdrop restart`.
pub async fn run(args: RestartArgs) -> Result<(), Failure> {
    let report = restart(&mut System::default(), Timing::REAL).await?;
    if args.json {
        println!("{}", output::json(&report));
    } else {
        println!("{}", summary(&report));
    }
    Ok(())
}

/// The line `playdrop restart` prints.
pub fn summary(report: &Report) -> String {
    let how = match report.started_by {
        StartPath::Systemd => "the systemd user unit",
        StartPath::Activation => "D-Bus activation",
        StartPath::Spawn => "running playdropd",
    };
    match &report.old_version {
        None => format!(
            "PlayDrop was not running; started {} through {how}",
            report.new_version
        ),
        Some(old) if *old == report.new_version => {
            format!("restarted PlayDrop {old} through {how}")
        }
        Some(old) => format!(
            "restarted PlayDrop: {old} -> {} through {how}",
            report.new_version
        ),
    }
}

/// Stops the running daemon, if any, and starts it again (module docs).
pub async fn restart(daemon: &mut impl Daemon, timing: Timing) -> Result<Report, Failure> {
    let old_version = daemon.version().await?;
    let systemd = old_version.is_some() && daemon.under_systemd().await;
    if let Some(old) = &old_version {
        eprintln!("stopping the PlayDrop daemon ({old})");
        daemon.shutdown().await?;
        if !gone(daemon, timing).await {
            return Err(Failure::new(
                Exit::Failed,
                format!(
                    "the daemon was asked to stop but was still running after {} s; \
                     try again, or log out and back in",
                    timing.stop.as_secs()
                ),
            ));
        }
    }
    let order: &[StartPath] = if systemd {
        &[StartPath::Systemd, StartPath::Activation, StartPath::Spawn]
    } else {
        &[StartPath::Activation, StartPath::Spawn]
    };
    let mut started_by = None;
    for &path in order {
        if daemon.start(path).await? {
            started_by = Some(path);
            break;
        }
    }
    let Some(started_by) = started_by else {
        return Err(Failure::new(
            Exit::Failed,
            "PlayDrop could not be started again: playdropd was not found next to this \
             command or on PATH; reinstall PlayDrop",
        ));
    };
    let new_version = answered(daemon, timing).await?;
    Ok(Report {
        old_version,
        new_version,
        started_by,
    })
}

/// Waits until no daemon answers. A daemon on its way out may fail a connect in other ways
/// (its process image already gone, the bus mid-change); those count as "not gone yet".
async fn gone(daemon: &mut impl Daemon, timing: Timing) -> bool {
    let deadline = Instant::now() + timing.stop;
    loop {
        if matches!(daemon.version().await, Ok(None)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(timing.poll).await;
    }
}

/// Waits until the new daemon answers and returns its version.
async fn answered(daemon: &mut impl Daemon, timing: Timing) -> Result<String, Failure> {
    let deadline = Instant::now() + timing.start;
    loop {
        if let Ok(Some(version)) = daemon.version().await {
            return Ok(version);
        }
        if let Some(ended) = daemon.exited() {
            let how = match ended {
                Ended::Code(c) => format!("stopped at once (exit code {c})"),
                Ended::Signal => "was stopped at once".to_owned(),
            };
            return Err(Failure::new(
                Exit::Failed,
                format!(
                    "playdropd started but {how}; run playdropd in a terminal to see why{}",
                    log_hint()
                ),
            ));
        }
        if Instant::now() >= deadline {
            return Err(Failure::new(
                Exit::Failed,
                format!(
                    "the daemon was started but did not answer within {} s{}",
                    timing.start.as_secs(),
                    log_hint()
                ),
            ));
        }
        tokio::time::sleep(timing.poll).await;
    }
}

/// Where the daemon's log is, when it writes one (Windows), as the end of a sentence.
fn log_hint() -> String {
    playdrop_platform::startup::log_path("playdropd")
        .map_or_else(String::new, |p| format!("; its log is {}", p.display()))
}

/// The real daemon and system.
#[derive(Default)]
pub struct System {
    /// The daemon this command ran, if it ran one.
    spawned: Option<Child>,
}

/// The systemd user unit (`packaging/linux/playdropd.service`).
#[cfg(target_os = "linux")]
const SYSTEMD_UNIT: &str = "playdropd.service";
/// `systemctl`, by absolute path as the daemon's own restart calls it.
#[cfg(target_os = "linux")]
const SYSTEMCTL: &str = "/usr/bin/systemctl";

/// Runs `systemctl --user <args> playdropd.service`: `Some(success)`, or `None` when it
/// could not run at all (no systemd, or inside the Flatpak sandbox).
#[cfg(target_os = "linux")]
async fn systemctl(args: &'static [&'static str]) -> Option<bool> {
    tokio::task::spawn_blocking(move || {
        Command::new(SYSTEMCTL)
            .arg("--user")
            .args(args)
            .arg(SYSTEMD_UNIT)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .ok()
            .map(|s| s.success())
    })
    .await
    .ok()
    .flatten()
}

impl Daemon for System {
    async fn version(&mut self) -> Result<Option<String>, Failure> {
        match Client::connect_without_activation().await {
            Ok(client) => Ok(Some(client.daemon_version().to_owned())),
            Err(ConnectError::NotRunning) => Ok(None),
            Err(e) => Err(Failure::from(e)),
        }
    }

    async fn shutdown(&mut self) -> Result<(), Failure> {
        let client = Client::connect_without_activation().await?;
        match client.request(Request::Shutdown).await {
            // `Internal` is how a dropped connection reads: the daemon may go before its
            // answer arrives. The wait that follows tells whether it went.
            Ok(_) => Ok(()),
            Err(e) if e.code == ErrorCode::Internal => Ok(()),
            Err(e) => Err(Failure::from(e)),
        }
    }

    async fn under_systemd(&mut self) -> bool {
        #[cfg(target_os = "linux")]
        {
            systemctl(&["is-active", "--quiet"]).await == Some(true)
        }
        // No systemd user units elsewhere; the await keeps one signature on every platform.
        #[cfg(not(target_os = "linux"))]
        {
            std::future::ready(false).await
        }
    }

    async fn start(&mut self, path: StartPath) -> Result<bool, Failure> {
        match path {
            StartPath::Systemd => {
                #[cfg(target_os = "linux")]
                {
                    eprintln!("starting it again through systemctl --user start {SYSTEMD_UNIT}");
                    Ok(systemctl(&["start"]).await == Some(true))
                }
                #[cfg(not(target_os = "linux"))]
                {
                    Ok(false)
                }
            }
            StartPath::Activation => {
                if !cfg!(target_os = "linux") {
                    return Ok(false);
                }
                // Asks the bus to start the daemon and waits for its name, as opening the
                // window does. No service file, or no bus: try the next way.
                match Client::connect().await {
                    Ok(_) => {
                        eprintln!("started it again through D-Bus activation");
                        Ok(true)
                    }
                    Err(ConnectError::NotRunning | ConnectError::Transport(_)) => Ok(false),
                    Err(e) => Err(Failure::from(e)),
                }
            }
            StartPath::Spawn => {
                let Some(program) = daemon_program() else {
                    return Ok(false);
                };
                eprintln!("starting {}", program.display());
                let child = detached(&program).spawn().map_err(|e| {
                    Failure::new(
                        Exit::Failed,
                        format!("could not start {}: {e}", program.display()),
                    )
                })?;
                self.spawned = Some(child);
                Ok(true)
            }
        }
    }

    fn exited(&mut self) -> Option<Ended> {
        let status = self.spawned.as_mut()?.try_wait().ok()??;
        Some(status.code().map_or(Ended::Signal, Ended::Code))
    }
}

/// File name of the daemon binary on this platform.
const DAEMON_BIN: &str = if cfg!(windows) {
    "playdropd.exe"
} else {
    "playdropd"
};

/// The installed daemon: beside this command (the `.deb`'s `/usr/bin`, the Flatpak's
/// `/app/bin`), one folder up (the Windows installer puts `playdrop.exe` in `bin\` under
/// the install folder), or else by name on `PATH`.
fn daemon_program() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    daemon_program_near(exe.as_deref(), Path::is_file).or_else(|| on_path(DAEMON_BIN))
}

/// [`daemon_program`]'s look beside `exe` and one folder up, with `is_file` passed in.
fn daemon_program_near(exe: Option<&Path>, is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    let dir = exe?.parent()?;
    [Some(dir), dir.parent()]
        .into_iter()
        .flatten()
        .map(|d| d.join(DAEMON_BIN))
        .find(|p| is_file(p))
}

/// The first `name` in a `PATH` folder.
fn on_path(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}

/// A command for the daemon that outlives this one: no terminal input or output, and out of
/// the terminal's reach (its own process group on Unix, so Ctrl-C and the hang-up of a
/// closed terminal do not reach it; detached from the console on Windows).
fn detached(program: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` (process creation flags).
        const FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
        command.creation_flags(FLAGS);
    }
    command
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A stand-in daemon and system. `answers` is what each [`Daemon::version`] call sees,
    /// in order; once used up, the last one repeats.
    #[derive(Default)]
    struct Fake {
        answers: VecDeque<Result<Option<&'static str>, Exit>>,
        last: Option<Result<Option<&'static str>, Exit>>,
        systemd: bool,
        /// Which start paths work here.
        works: Vec<StartPath>,
        /// What a started daemon answers from then on.
        after_start: Option<&'static str>,
        ended: Option<Ended>,
        shutdowns: usize,
        tried: Vec<StartPath>,
    }

    impl Fake {
        fn answers(list: &[Result<Option<&'static str>, Exit>]) -> Self {
            Self {
                answers: list.iter().copied().collect(),
                ..Self::default()
            }
        }
    }

    // The stand-in answers at once; the trait's methods are async for the real system.
    #[allow(
        clippy::unused_async_trait_impl,
        reason = "a stand-in for calls that wait on the real system"
    )]
    impl Daemon for Fake {
        async fn version(&mut self) -> Result<Option<String>, Failure> {
            if let Some(next) = self.answers.pop_front() {
                self.last = Some(next);
            }
            match self.last.unwrap_or(Ok(None)) {
                Ok(v) => Ok(v.map(str::to_owned)),
                Err(exit) => Err(Failure::new(exit, "bus trouble")),
            }
        }
        async fn shutdown(&mut self) -> Result<(), Failure> {
            self.shutdowns += 1;
            Ok(())
        }
        async fn under_systemd(&mut self) -> bool {
            self.systemd
        }
        async fn start(&mut self, path: StartPath) -> Result<bool, Failure> {
            self.tried.push(path);
            let ok = self.works.contains(&path);
            if ok {
                self.answers.clear();
                self.last = Some(Ok(self.after_start));
            }
            Ok(ok)
        }
        fn exited(&mut self) -> Option<Ended> {
            self.ended
        }
    }

    const FAST: Timing = Timing {
        stop: Duration::from_millis(200),
        start: Duration::from_millis(200),
        poll: Duration::from_millis(1),
    };

    #[tokio::test]
    async fn a_daemon_the_bus_started_is_stopped_and_activated_again() {
        // Answers 0.11.0, still there once after the shutdown, then gone.
        let mut d = Fake::answers(&[Ok(Some("0.11.0")), Ok(Some("0.11.0")), Ok(None)]);
        d.works = vec![StartPath::Activation, StartPath::Spawn];
        d.after_start = Some("0.12.0");
        let report = restart(&mut d, FAST).await.expect("restarted");
        assert_eq!(
            report,
            Report {
                old_version: Some("0.11.0".into()),
                new_version: "0.12.0".into(),
                started_by: StartPath::Activation,
            }
        );
        assert_eq!(d.shutdowns, 1);
        assert_eq!(d.tried, [StartPath::Activation]);
        assert_eq!(
            summary(&report),
            "restarted PlayDrop: 0.11.0 -> 0.12.0 through D-Bus activation"
        );
    }

    #[tokio::test]
    async fn the_systemd_unit_is_started_through_systemd() {
        let mut d = Fake::answers(&[Ok(Some("0.12.0")), Ok(None)]);
        d.systemd = true;
        d.works = vec![StartPath::Systemd, StartPath::Activation];
        d.after_start = Some("0.12.0");
        let report = restart(&mut d, FAST).await.expect("restarted");
        assert_eq!(report.started_by, StartPath::Systemd);
        assert_eq!(d.tried, [StartPath::Systemd]);
        assert_eq!(
            summary(&report),
            "restarted PlayDrop 0.12.0 through the systemd user unit"
        );
    }

    #[tokio::test]
    async fn a_unit_that_will_not_start_falls_back_to_the_bus() {
        let mut d = Fake::answers(&[Ok(Some("0.11.0")), Ok(None)]);
        d.systemd = true;
        d.works = vec![StartPath::Activation];
        d.after_start = Some("0.12.0");
        let report = restart(&mut d, FAST).await.expect("restarted");
        assert_eq!(report.started_by, StartPath::Activation);
        assert_eq!(d.tried, [StartPath::Systemd, StartPath::Activation]);
    }

    #[tokio::test]
    async fn without_a_bus_service_the_installed_daemon_is_run() {
        // Windows, or Linux without a D-Bus service file.
        let mut d = Fake::answers(&[Ok(Some("0.11.0")), Ok(None)]);
        d.works = vec![StartPath::Spawn];
        d.after_start = Some("0.12.0");
        let report = restart(&mut d, FAST).await.expect("restarted");
        assert_eq!(report.started_by, StartPath::Spawn);
        assert_eq!(d.tried, [StartPath::Activation, StartPath::Spawn]);
    }

    #[tokio::test]
    async fn a_daemon_that_is_not_running_is_only_started() {
        let mut d = Fake::answers(&[Ok(None)]);
        d.systemd = true; // ignored: nothing ran, so nothing ran under the unit
        d.works = vec![StartPath::Activation];
        d.after_start = Some("0.12.0");
        let report = restart(&mut d, FAST).await.expect("started");
        assert_eq!(report.old_version, None);
        assert_eq!(d.shutdowns, 0);
        assert_eq!(d.tried, [StartPath::Activation]);
        assert_eq!(
            summary(&report),
            "PlayDrop was not running; started 0.12.0 through D-Bus activation"
        );
    }

    #[tokio::test]
    async fn a_daemon_that_will_not_stop_is_left_alone_and_reported() {
        let mut d = Fake::answers(&[Ok(Some("0.11.0"))]);
        d.works = vec![StartPath::Activation];
        let f = restart(&mut d, FAST).await.expect_err("never stopped");
        assert_eq!(f.exit, Exit::Failed);
        assert!(f.message.contains("still running"), "{}", f.message);
        assert!(d.tried.is_empty(), "nothing started beside it");
    }

    #[tokio::test]
    async fn trouble_while_it_stops_is_not_taken_for_gone() {
        let mut d = Fake::answers(&[
            Ok(Some("0.11.0")),
            Err(Exit::Failed),
            Err(Exit::NoDaemon),
            Ok(None),
        ]);
        d.works = vec![StartPath::Activation];
        d.after_start = Some("0.12.0");
        restart(&mut d, FAST).await.expect("restarted");
        assert_eq!(d.answers.len(), 0, "every answer was read before the start");
    }

    #[tokio::test]
    async fn the_first_question_failing_is_the_answer() {
        let mut d = Fake::answers(&[Err(Exit::NoDaemon)]);
        let f = restart(&mut d, FAST).await.expect_err("no bus");
        assert_eq!(f.exit, Exit::NoDaemon);
        assert_eq!(d.shutdowns, 0);
    }

    #[tokio::test]
    async fn no_way_to_start_it_says_so() {
        let mut d = Fake::answers(&[Ok(Some("0.11.0")), Ok(None)]);
        let f = restart(&mut d, FAST).await.expect_err("nothing works");
        assert_eq!(f.exit, Exit::Failed);
        assert!(f.message.contains("reinstall PlayDrop"), "{}", f.message);
    }

    #[tokio::test]
    async fn a_daemon_that_stops_at_once_is_reported_with_its_code() {
        let mut d = Fake::answers(&[Ok(None)]);
        d.works = vec![StartPath::Spawn];
        d.after_start = None;
        d.ended = Some(Ended::Code(1));
        let f = restart(&mut d, FAST).await.expect_err("stopped at once");
        assert_eq!(f.exit, Exit::Failed);
        assert!(f.message.contains("exit code 1"), "{}", f.message);
    }

    #[tokio::test]
    async fn a_daemon_that_never_answers_is_reported() {
        let mut d = Fake::answers(&[Ok(None)]);
        d.works = vec![StartPath::Activation];
        d.after_start = None;
        let f = restart(&mut d, FAST).await.expect_err("no answer");
        assert!(f.message.contains("did not answer"), "{}", f.message);
    }

    #[test]
    fn the_installed_daemon_is_found_beside_or_one_folder_up() {
        let here = Path::new("/usr/bin/playdrop");
        assert_eq!(
            daemon_program_near(Some(here), |p| p == Path::new("/usr/bin").join(DAEMON_BIN)),
            Some(Path::new("/usr/bin").join(DAEMON_BIN))
        );
        // The Windows layout: bin\playdrop.exe under the folder that holds the daemon.
        let bin = Path::new("/opt/PlayDrop/bin/playdrop");
        assert_eq!(
            daemon_program_near(Some(bin), |p| p
                == Path::new("/opt/PlayDrop").join(DAEMON_BIN)),
            Some(Path::new("/opt/PlayDrop").join(DAEMON_BIN))
        );
        assert_eq!(daemon_program_near(Some(here), |_| false), None);
        assert_eq!(daemon_program_near(None, |_| true), None);
    }

    #[test]
    fn the_json_report_names_the_start_path() {
        let report = Report {
            old_version: Some("0.11.0".into()),
            new_version: "0.12.0".into(),
            started_by: StartPath::Systemd,
        };
        assert_eq!(
            output::json(&report),
            r#"{"old_version":"0.11.0","new_version":"0.12.0","started_by":"systemd"}"#
        );
    }
}
