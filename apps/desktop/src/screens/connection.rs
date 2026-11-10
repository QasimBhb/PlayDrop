//! Not-connected screen: connecting, "daemon is not running" with Start, other errors
//! with Try again.

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
    if let Err(message) = daemon::spawn() {
        set_phase(c, ConnPhase::Failed, &message);
        return;
    }
    set_starting(c, true);
    c.start_attempts.set(0);
    c.start_timer
        .start(TimerMode::Repeated, START_RETRY_EVERY, || {
            with(|c| {
                if c.ipc.is_connected() {
                    c.start_timer.stop();
                    return;
                }
                let n = c.start_attempts.get() + 1;
                c.start_attempts.set(n);
                if n > START_RETRIES {
                    c.start_timer.stop();
                    set_starting(c, false);
                    set_phase(
                        c,
                        ConnPhase::Failed,
                        "The daemon was started but did not answer within 10 seconds. Run playdropd in a terminal to see why.",
                    );
                    return;
                }
                c.ipc.connect();
            });
        });
}

pub(super) fn on_connect_failed(c: &Controller, f: &ConnectFailure) {
    // While "Start" is retrying, a failed attempt is expected; keep the spinner.
    if c.start_timer.running() {
        return;
    }
    if f.not_running {
        set_phase(c, ConnPhase::NotRunning, "");
    } else {
        set_phase(c, ConnPhase::Failed, &f.message);
    }
}

pub(super) fn on_disconnected(c: &Controller) {
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
