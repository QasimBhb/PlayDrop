//! Starting the new daemon after a `.deb` update (`docs/design/update-check.md` section 6).
//!
//! `apt-get` replaced `/usr/bin/playdropd` on disk; the running process is still the old
//! program. Two ways to put the new one in its place, picked by how this process was
//! started:
//!
//! - **As the systemd user unit `playdropd.service`** (D-Bus activation goes through it
//!   when the session has a user manager). The unit is `Type=dbus`: systemd stops a unit
//!   of that type whose bus name goes away, so releasing the name and `exec`ing would get
//!   the new process killed as it starts. Instead the daemon asks systemd to restart the
//!   unit (`systemctl --user --no-block restart`); systemd sends `SIGTERM`, the daemon
//!   shuts down cleanly as for any `SIGTERM`, and systemd starts the new binary.
//! - **Otherwise** (XDG autostart, a terminal, the D-Bus daemon's own activation): after
//!   the usual clean shutdown (host stopped, IPC server stopped so the bus name is
//!   released, tray gone) the process `exec`s `/usr/bin/playdropd` with its own arguments.
//!   The PID stays, so a parent that waits for it keeps waiting for the right process.
//!   Every descriptor Rust and tokio open is close-on-exec, so the old bus connection and
//!   listening socket close with the old image, and the new process takes the bus name
//!   afresh: owning it is the single-instance check, so if anything else started a daemon
//!   in the gap (a `playdrop` command activating one), the new process says "already
//!   running" and exits, leaving exactly one daemon, of the new version. Clients saw
//!   `Restarting` and then `ShuttingDown`, and the window and the applet follow the name
//!   back without starting a daemon themselves.

use std::path::Path;

use super::kind::DEB_DAEMON;

/// The systemd user unit name (`packaging/linux/playdropd.service`).
pub const SYSTEMD_UNIT: &str = "playdropd.service";

/// How to start the new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plan {
    /// Ask systemd to restart the unit; it stops this process with `SIGTERM`.
    Systemd,
    /// Shut down, then `exec` the new binary in this process.
    Exec,
}

/// The plan for a process whose `/proc/self/cgroup` reads `cgroup`: the unit's own cgroup
/// path ends in `/playdropd.service`.
pub fn plan_for(cgroup: &str) -> Plan {
    let in_unit = cgroup.lines().any(|line| {
        line.rsplit(':')
            .next()
            .is_some_and(|path| path.trim_end().ends_with(&format!("/{SYSTEMD_UNIT}")))
    });
    if in_unit {
        Plan::Systemd
    } else {
        Plan::Exec
    }
}

/// The plan for this process.
pub fn plan() -> Plan {
    std::fs::read_to_string("/proc/self/cgroup").map_or(Plan::Exec, |c| plan_for(&c))
}

/// What `systemctl` ending means for the restart it was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// Systemd took the restart: it stops this process and starts the new binary.
    Accepted,
    /// Systemd refused; the caller falls back to [`Plan::Exec`].
    Refused,
}

/// Reads how `systemctl restart` ended: `code` is its exit code, `None` when a signal
/// ended it.
///
/// `systemctl` runs as a child of the daemon, so it lives in the unit's own cgroup, and
/// stopping a unit signals every process in that cgroup. A `systemctl` ended by a signal
/// is therefore the restart under way, not a refusal: systemd only signals the cgroup
/// once it has taken the job. Reading it as a refusal made the daemon `exec` itself in
/// the middle of systemd's stop (seen on the first real update, 0.11.0 to 0.12.0: the unit
/// sat in "deactivating" until the stop timeout).
pub fn asked(code: Option<i32>) -> Asked {
    match code {
        Some(0) | None => Asked::Accepted,
        Some(_) => Asked::Refused,
    }
}

/// Asks systemd to restart the unit, without waiting for it (systemd stops this process
/// as part of the restart). Returns `false` when `systemctl` could not be started or
/// refused, so the caller falls back to [`Plan::Exec`].
pub fn ask_systemd() -> bool {
    let started = std::process::Command::new("/usr/bin/systemctl")
        .args(["--user", "--no-block", "restart", SYSTEMD_UNIT])
        .stdin(std::process::Stdio::null())
        .status();
    match started {
        Ok(status) if asked(status.code()) == Asked::Accepted => {
            tracing::info!(code = ?status.code(), "asked systemd to restart {SYSTEMD_UNIT} on the new version");
            true
        }
        Ok(status) => {
            tracing::warn!(code = ?status.code(), "systemctl refused the restart; restarting in place");
            false
        }
        Err(e) => {
            tracing::warn!(error = %e, "systemctl could not be started; restarting in place");
            false
        }
    }
}

/// Replaces this process with the installed `/usr/bin/playdropd`, same arguments and
/// environment. Returns only when `exec` failed; the caller then exits, and the next UI or
/// login starts the new version.
#[cfg(unix)]
pub fn exec_new_daemon() -> std::io::Error {
    use std::os::unix::process::CommandExt as _;
    tracing::info!("starting the new daemon in place");
    std::process::Command::new(Path::new(DEB_DAEMON))
        .args(std::env::args_os().skip(1))
        .exec()
}

/// Windows never restarts in place: the installer does it.
#[cfg(not(unix))]
pub fn exec_new_daemon() -> std::io::Error {
    let _ = Path::new(DEB_DAEMON);
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "restarting in place is for the .deb only",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_systemctl_ended_by_the_units_own_stop_is_not_a_refusal() {
        assert_eq!(asked(Some(0)), Asked::Accepted);
        assert_eq!(asked(None), Asked::Accepted);
        assert_eq!(asked(Some(1)), Asked::Refused);
        assert_eq!(asked(Some(5)), Asked::Refused);
    }

    #[test]
    fn the_systemd_unit_is_recognised_from_its_cgroup() {
        assert_eq!(
            plan_for(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/playdropd.service\n"
            ),
            Plan::Systemd
        );
        // XDG autostart under GNOME or COSMIC runs in an app scope; a terminal in a
        // session scope; cgroup v1 lists several lines.
        for other in [
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.playdrop.Daemon-1234.scope\n",
            "0::/user.slice/user-1000.slice/session-2.scope\n",
            "12:pids:/user.slice\n1:name=systemd:/user.slice/session-2.scope\n",
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/not-playdropd.service.d\n",
            "",
        ] {
            assert_eq!(plan_for(other), Plan::Exec, "{other:?}");
        }
    }
}
