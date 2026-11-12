//! Not-connected screen: connecting, "daemon is not running" with Start, other errors
//! with Try again.
//!
//! After "Start" the screen says why the daemon did not come up when it can: the binary is
//! missing, it could not be run, it exited at once (with its exit code and, on Windows,
//! the path of its log), or it never answered.

use std::cell::RefCell;
use std::time::Duration;

use slint::{ComponentHandle, TimerMode};

use super::{with, Controller};
use crate::ipc::ConnectFailure;
use crate::{daemon, App, ConnPhase, ConnState};

/// How long a daemon that announced its shutdown may take before the window stops waiting
/// for its connection to close and checks the bus itself.
const STOPPING_PATIENCE: Duration = Duration::from_secs(15);

/// After "Start", how often to try connecting.
const START_RETRY_EVERY: Duration = Duration::from_secs(1);
/// After "Start", how many tries before giving up (10 s).
const START_RETRIES: u32 = 10;

thread_local! {
    /// The daemon "Start" ran, until it answers or the screen gives up on it. Slint thread
    /// only, like the controller.
    static LAUNCH: RefCell<Option<daemon::Launch>> = const { RefCell::new(None) };
    /// Why the last "Start" did not lead to a connection. Shown with "not running" until a
    /// new "Start", or until a connection made since is lost (the reason is stale by then).
    static REASON: RefCell<Option<String>> = const { RefCell::new(None) };
}

fn remember_reason(reason: Option<String>) {
    REASON.with(|r| *r.borrow_mut() = reason);
}

/// The text under "not running": the last "Start" failure, else where a daemon that stopped
/// by itself leaves its reason (Windows), else nothing.
fn not_running_message() -> String {
    REASON
        .with(|r| r.borrow().clone())
        .unwrap_or_else(|| daemon::not_running_hint(daemon::daemon_log().as_deref()))
}

pub(super) fn install(app: &App) {
    let conn = app.global::<ConnState>();
    conn.on_start_daemon(|| {
        with(start_daemon);
    });
    conn.on_retry(|| {
        with(|c| {
            set_phase(c, ConnPhase::Connecting, "");
            c.ipc.connect();
        });
    });
}

fn set_phase(c: &Controller, phase: ConnPhase, message: &str) {
    if let Some(app) = c.app() {
        let conn = app.global::<ConnState>();
        conn.set_phase(phase);
        conn.set_message(message.into());
    }
}

fn set_starting(c: &Controller, on: bool) {
    if let Some(app) = c.app() {
        app.global::<ConnState>().set_starting(on);
    }
}

fn start_daemon(c: &Controller) {
    remember_reason(None);
    let launch = match daemon::spawn() {
        Ok(launch) => launch,
        Err(e) => {
            set_phase(c, ConnPhase::Failed, &e.to_string());
            return;
        }
    };
    LAUNCH.with(|l| *l.borrow_mut() = Some(launch));
    set_starting(c, true);
    c.start_attempts.set(0);
    c.start_timer
        .start(TimerMode::Repeated, START_RETRY_EVERY, || {
            with(start_tick);
        });
}

/// One "Start" retry: done once connected, stops early when the new daemon has already
/// failed, and gives up after [`START_RETRIES`].
fn start_tick(c: &Controller) {
    if c.ipc.is_connected() {
        c.start_timer.stop();
        LAUNCH.with(|l| l.borrow_mut().take());
        return;
    }
    // A clean exit (code 0) is not a failure yet: that is what a second daemon does when
    // another one already owns the bus name or pipe, and the next attempt reaches that one.
    let failed_code = LAUNCH.with(|l| {
        l.borrow()
            .as_ref()
            .and_then(daemon::Launch::exited)
            .filter(|status| !status.success())
            .map(|status| status.code())
    });
    if let Some(code) = failed_code {
        give_up(
            c,
            ConnPhase::NotRunning,
            daemon::exit_reason(code, daemon::daemon_log().as_deref()),
        );
        return;
    }
    let n = c.start_attempts.get() + 1;
    c.start_attempts.set(n);
    if n > START_RETRIES {
        give_up(
            c,
            ConnPhase::Failed,
            daemon::no_answer_reason(daemon::daemon_log().as_deref()),
        );
        return;
    }
    c.ipc.connect();
}

/// Ends a "Start" that did not connect and shows `reason`. A reason that goes with "not
/// running" is kept for later reports of that state, so a connect attempt still in flight
/// cannot blank it.
fn give_up(c: &Controller, phase: ConnPhase, reason: String) {
    c.start_timer.stop();
    LAUNCH.with(|l| l.borrow_mut().take());
    set_starting(c, false);
    set_phase(c, phase, &reason);
    if phase == ConnPhase::NotRunning {
        remember_reason(Some(reason));
    }
}

pub(super) fn on_connect_failed(c: &Controller, f: &ConnectFailure) {
    // While "Start" is retrying, a failed attempt is expected; keep the spinner.
    if c.start_timer.running() {
        return;
    }
    if f.not_running {
        set_phase(c, ConnPhase::NotRunning, &not_running_message());
    } else {
        set_phase(c, ConnPhase::Failed, &f.message);
    }
}

pub(super) fn on_disconnected(c: &Controller) {
    remember_reason(None);
    set_phase(
        c,
        ConnPhase::Connecting,
        "The connection to the daemon was lost. Reconnecting...",
    );
    forget_daemon_state(c);
    // Never `connect` here: on Linux that asks the bus to start the daemon, which would undo
    // a crash report at best and the user's "Quit" at worst.
    c.ipc.follow();
}

/// The daemon said it is shutting down (the user chose Quit, or the session is ending). It
/// still owns its name for a moment, so "Start" is not offered yet: a start now would find
/// the old daemon, exit, and leave the user to click again. Once this daemon is really gone
/// the connection bridge reports "not running", which shows the button, and it picks up the
/// next daemon by itself.
pub(super) fn on_shutting_down(c: &Controller) {
    remember_reason(None);
    c.ipc.disconnect();
    set_phase(c, ConnPhase::Connecting, "PlayDrop is stopping...");
    forget_daemon_state(c);
    // A daemon that hangs while stopping never ends its event stream, and this screen has no
    // button. After a while look for ourselves: that either finds the daemon still there or
    // ends in "not running" or an error, each of which has a way on.
    c.stopping_timer
        .start(TimerMode::SingleShot, STOPPING_PATIENCE, || {
            with(|c| c.ipc.follow());
        });
}

fn forget_daemon_state(c: &Controller) {
    if let Some(app) = c.app() {
        let p = app.global::<crate::PairingState>();
        p.set_prompt_visible(false);
    }
    // The daemon is gone; whatever sessions it had are no longer ours to show.
    c.store.borrow_mut().clear_input_sessions();
    super::refresh_input_banner(c);
}
