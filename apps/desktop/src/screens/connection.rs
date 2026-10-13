//! Not-connected screen: connecting, "daemon is not running" with Start, other errors
//! with Try again.

use std::time::Duration;

use slint::{ComponentHandle, TimerMode};

use super::{with, Controller};
use crate::ipc::ConnectFailure;
use crate::{daemon, App, ConnPhase, ConnState};

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
    if let Some(app) = c.app() {
        let p = app.global::<crate::PairingState>();
        p.set_prompt_visible(false);
    }
    // The daemon is gone; whatever sessions it had are no longer ours to show.
    c.store.borrow_mut().clear_input_sessions();
    super::refresh_input_banner(c);
    c.ipc.connect();
}
