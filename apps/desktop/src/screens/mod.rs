//! The window controller: owns the view state on the Slint thread, wires each screen's
//! callbacks, and applies daemon events to the models.
//!
//! The controller lives in a thread-local on the Slint thread. Callbacks and IPC answers
//! reach it through [`with`], so nothing that is not `Send` crosses threads: the tokio side
//! only ever posts closures that look the controller up again.

mod connection;
mod devices;
mod diagnostics;
mod pairing;
mod settings;
mod transfers;
mod update;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use playdrop_ipc::{ConsentToken, ErrorCode, IpcError, Ok, Request};
use slint::{ComponentHandle, Timer, TimerMode, VecModel};

use crate::ipc::{Ipc, IpcMsg};
use crate::model::{self, Change, Refetch, Store};
use crate::{
    App, CandidateRow, ConnPhase, ConnState, DeviceRow, InputState, Nav, NoticeState, TransferRow,
};

/// Tab index of the diagnostics page (see `ui/app.slint`).
const DIAGNOSTICS_PAGE: i32 = 4;

/// How long a notice stays before it clears itself.
const NOTICE_FOR: Duration = Duration::from_secs(6);
/// How long the notice that asks to send a marked clip stays: time to read it and press
/// again, well inside the daemon's 60 s token lifetime (SR-24).
const CONSENT_NOTICE_FOR: Duration = Duration::from_secs(30);
/// How often relative times ("5 min ago") are refreshed.
const CLOCK_TICK: Duration = Duration::from_secs(30);
/// How long to wait before asking again when the daemon answered that it is still starting.
const STARTING_RETRY_EVERY: Duration = Duration::from_millis(500);
/// How many times in a row to ask again (20 s) before the answer is shown as an error.
const STARTING_RETRIES: u32 = 40;

/// View state and handles, owned by the Slint thread.
pub struct Controller {
    app: slint::Weak<App>,
    ipc: Arc<Ipc>,
    store: RefCell<Store>,
    devices: Rc<VecModel<DeviceRow>>,
    transfers: Rc<VecModel<TransferRow>>,
    candidates: Rc<VecModel<CandidateRow>>,
    notice_timer: Timer,
    clock: Timer,
    /// Add-device window countdown.
    add_timer: Timer,
    add_deadline: Cell<Option<std::time::Instant>>,
    /// "Start" retries.
    start_timer: Timer,
    start_attempts: Cell<u32>,
    /// Bounds the wait for a daemon that said it is shutting down to really go.
    stopping_timer: Timer,
    /// Asks for the state again while the daemon says it is still starting (right after
    /// it came up, for example on the new version after an update).
    starting_timer: Timer,
    starting_attempts: Cell<u32>,
    /// The device and consent token behind the notice that asks to send a marked clip
    /// (SR-24). Set only by [`Controller::ask_consent`] and cleared whenever that notice is
    /// replaced, times out or is dismissed, so a press counts as consent only while the
    /// question is on screen.
    consent: RefCell<Option<(String, ConsentToken)>>,
    /// The update check was asked for once: this window's first connection is "a UI
    /// opened"; reconnections only read the state.
    update_asked: Cell<bool>,
}

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<Controller>>> = const { RefCell::new(None) };
}

/// Runs `f` with the controller. Returns `None` before [`install`] or after the window
/// closed.
pub fn with<R>(f: impl FnOnce(&Controller) -> R) -> Option<R> {
    let c = CONTROLLER.with(|slot| slot.borrow().clone())?;
    Some(f(&c))
}

/// Creates the controller for `app`, wires every screen, and starts connecting.
pub fn install(app: &App, ipc: Arc<Ipc>) {
    let c = Rc::new(Controller {
        app: app.as_weak(),
        ipc,
        store: RefCell::new(Store::default()),
        devices: Rc::new(VecModel::default()),
        transfers: Rc::new(VecModel::default()),
        candidates: Rc::new(VecModel::default()),
        notice_timer: Timer::default(),
        clock: Timer::default(),
        add_timer: Timer::default(),
        add_deadline: Cell::new(None),
        start_timer: Timer::default(),
        start_attempts: Cell::new(0),
        stopping_timer: Timer::default(),
        starting_timer: Timer::default(),
        starting_attempts: Cell::new(0),
        consent: RefCell::new(None),
        update_asked: Cell::new(false),
    });
    CONTROLLER.with(|slot| *slot.borrow_mut() = Some(Rc::clone(&c)));

    let notice = app.global::<NoticeState>();
    notice.on_dismiss(|| {
        with(Controller::clear_notice);
    });
    app.global::<InputState>().on_stop(|| {
        with(stop_input);
    });
    connection::install(app);
    devices::install(app, &c);
    pairing::install(app, &c);
    transfers::install(app, &c);
    settings::install(app);
    update::install(app);
    diagnostics::install(app);

    c.clock.start(TimerMode::Repeated, CLOCK_TICK, || {
        with(|c| {
            devices::refresh_all(c);
            update::refresh(c);
        });
    });
    c.ipc.connect();
}

/// Drops the controller (window closed).
pub fn uninstall() {
    CONTROLLER.with(|slot| slot.borrow_mut().take());
}

/// Entry point for messages from the IPC side, on the Slint thread.
pub fn deliver(msg: IpcMsg) {
    with(|c| c.on_ipc(msg));
}

impl Controller {
    fn app(&self) -> Option<App> {
        self.app.upgrade()
    }

    /// The IPC bridge.
    pub fn ipc(&self) -> &Arc<Ipc> {
        &self.ipc
    }

    fn on_ipc(&self, msg: IpcMsg) {
        match msg {
            IpcMsg::Connected => self.on_connected(),
            IpcMsg::ConnectFailed(f) => connection::on_connect_failed(self, &f),
            IpcMsg::Event(e) => {
                let changes = self.store.borrow_mut().apply(e, model::unix_now());
                self.apply_changes(changes);
            }
            IpcMsg::Lagged => self.load(Refetch::All),
            IpcMsg::Disconnected => connection::on_disconnected(self),
        }
    }

    fn on_connected(&self) {
        tracing::info!("connected to the daemon");
        self.start_timer.stop();
        self.start_attempts.set(0);
        if let Some(app) = self.app() {
            let conn = app.global::<ConnState>();
            conn.set_starting(false);
            conn.set_message("".into());
            conn.set_phase(ConnPhase::Connected);
            if app.global::<Nav>().get_page() == DIAGNOSTICS_PAGE {
                diagnostics::refresh(self);
            }
        }
        self.load(Refetch::All);
        update::on_connected(self);
    }

    /// Re-requests state from the daemon.
    pub fn load(&self, what: Refetch) {
        if what == Refetch::All {
            self.ipc.request(Request::Status, |r| {
                with(|c| match r {
                    Result::Ok(Ok::Status(s)) => {
                        c.starting_attempts.set(0);
                        c.store.borrow_mut().set_status(s);
                        refresh_input_banner(c);
                        settings::refresh(c);
                        update::refresh(c);
                        pairing::refresh_lock(c);
                    }
                    other => c.load_failed("Could not read the daemon status", other),
                });
            });
        }
        if what != Refetch::Peers {
            self.ipc.request(Request::Transfers, |r| {
                with(|c| match r {
                    Result::Ok(Ok::Transfers(t)) => {
                        c.store.borrow_mut().set_transfers(t);
                        transfers::refresh_all(c);
                    }
                    other => c.load_failed("Could not list transfers", other),
                });
            });
        }
        if what == Refetch::Transfers {
            return;
        }
        self.ipc.request(Request::Peers, |r| {
            with(|c| match r {
                Result::Ok(Ok::Peers(p)) => {
                    let online = p.iter().filter(|p| p.online).count();
                    tracing::info!(devices = p.len(), online, "devices loaded");
                    c.store.borrow_mut().set_peers(p, model::unix_now());
                    devices::refresh_all(c);
                    transfers::refresh_all(c);
                }
                other => c.load_failed("Could not list devices", other),
            });
        });
    }

    /// A state request failed. A daemon that has just come up answers that it is
    /// starting (`InvalidState`) until its host runs: that is not an error to
    /// show, so everything is asked for again shortly, a bounded number of times.
    fn load_failed(&self, what: &str, result: Result<Ok, IpcError>) {
        let starting = matches!(&result, Result::Err(e) if e.code == ErrorCode::InvalidState);
        if !starting || self.starting_attempts.get() >= STARTING_RETRIES {
            self.report(what, result);
            return;
        }
        // One timer for the three requests of a load: starting it again only moves it.
        if !self.starting_timer.running() {
            self.starting_attempts.set(self.starting_attempts.get() + 1);
        }
        self.starting_timer
            .start(TimerMode::SingleShot, STARTING_RETRY_EVERY, || {
                with(|c| c.load(Refetch::All));
            });
    }

    fn apply_changes(&self, changes: Vec<Change>) {
        for change in changes {
            match change {
                Change::Peers => {
                    devices::refresh_all(self);
                    transfers::refresh_all(self);
                }
                Change::Peer(i) => devices::refresh_row(self, i),
                Change::Transfers => transfers::refresh_all(self),
                Change::Transfer(i) => transfers::refresh_row(self, i),
                Change::Candidates => pairing::refresh_candidates(self),
                Change::Prompt => pairing::refresh_prompt(self),
                Change::WindowClosed => pairing::window_closed(self),
                Change::PairingStatus(text, error) => pairing::set_status(self, &text, error),
                Change::Notice(text, error) => self.notice(&text, error),
                Change::Settings => {
                    settings::refresh(self);
                    update::refresh(self);
                }
                Change::Update => update::refresh(self),
                Change::Lock => pairing::refresh_lock(self),
                Change::InputSessions => refresh_input_banner(self),
                Change::Refetch(what) => self.load(what),
                Change::ShuttingDown => connection::on_shutting_down(self),
            }
        }
    }

    /// Shows a one-line notice that clears itself.
    pub fn notice(&self, text: &str, error: bool) {
        self.show_notice(text, error, NOTICE_FOR);
    }

    /// Shows a notice for `for_`. Any notice replaces the consent question, so the token
    /// behind that question goes with it.
    fn show_notice(&self, text: &str, error: bool, for_: Duration) {
        self.consent.borrow_mut().take();
        let Some(app) = self.app() else { return };
        let n = app.global::<NoticeState>();
        n.set_text(text.into());
        n.set_error(error);
        self.notice_timer.start(TimerMode::SingleShot, for_, || {
            with(Controller::clear_notice);
        });
    }

    /// Asks whether to send a marked clip to `peer` (SR-24) and holds `token` for exactly as
    /// long as the question is shown.
    pub fn ask_consent(&self, peer: String, token: ConsentToken) {
        self.show_notice(model::SENSITIVE_CLIP_NOTICE, true, CONSENT_NOTICE_FOR);
        *self.consent.borrow_mut() = Some((peer, token));
    }

    /// The consent token, once, if the question for `peer` is on screen: this press is the
    /// user's yes. Any other press drops it.
    pub fn take_consent(&self, peer: &str) -> Option<ConsentToken> {
        self.consent
            .borrow_mut()
            .take()
            .and_then(|(p, token)| (p == peer).then_some(token))
    }

    fn clear_notice(&self) {
        self.consent.borrow_mut().take();
        self.notice_timer.stop();
        if let Some(app) = self.app() {
            app.global::<NoticeState>().set_text("".into());
        }
    }

    /// Shows a failed answer as an error notice; an unexpected success kind is logged.
    pub fn report(&self, what: &str, result: Result<Ok, IpcError>) {
        match result {
            Result::Err(e) => self.notice(&format!("{what}: {}", e.message), true),
            Result::Ok(other) => {
                tracing::warn!("{what}: unexpected answer {:?}", kind_name(&other));
            }
        }
    }

    /// Notice for a request whose success carries no data.
    pub fn done_or_report(&self, ok_text: &str, what: &str, result: Result<Ok, IpcError>) {
        match result {
            Result::Ok(_) => {
                if !ok_text.is_empty() {
                    self.notice(ok_text, false);
                }
            }
            Result::Err(e) => self.notice(&format!("{what}: {}", e.message), true),
        }
    }
}

/// Shows or hides the remote input banner from the stored sessions (SR-25).
pub(super) fn refresh_input_banner(c: &Controller) {
    let text = model::input_banner_text(&c.store.borrow().input_sessions);
    if let Some(app) = c.app() {
        app.global::<InputState>().set_banner(text.into());
    }
}

/// The banner's Stop button: the SR-25 kill switch. The daemon ends every session and
/// sends `InputSession { active: false }` for each, which hides the banner.
fn stop_input(c: &Controller) {
    c.ipc().request(Request::StopInput, |r| {
        with(|c| {
            if r.is_ok() {
                c.store.borrow_mut().clear_input_sessions();
                refresh_input_banner(c);
            }
            c.done_or_report("Remote input stopped", "Could not stop remote input", r);
        });
    });
}

/// The variant name of an answer, for logs (never the payload, SR-32).
fn kind_name(ok: &Ok) -> &'static str {
    match ok {
        Ok::Done => "done",
        Ok::Status(_) => "status",
        Ok::Peers(_) => "peers",
        Ok::Transfers(_) => "transfers",
        Ok::Diagnostics(_) => "diagnostics",
        Ok::AddDeviceList(_) => "add_device_list",
        Ok::QrUri(_) => "qr_uri",
        Ok::TransferStarted { .. } => "transfer_started",
        Ok::ClipboardPushed { .. } => "clipboard_pushed",
        Ok::Settings { .. } => "settings",
        Ok::Update(_) => "update",
        _ => "unknown",
    }
}
