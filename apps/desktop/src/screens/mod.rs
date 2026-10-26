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

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use playdrop_ipc::{IpcError, Ok, Request};
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
/// How often relative times ("5 min ago") are refreshed.
const CLOCK_TICK: Duration = Duration::from_secs(30);

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
    diagnostics::install(app);

    c.clock.start(TimerMode::Repeated, CLOCK_TICK, || {
        with(devices::refresh_all);
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
    }

    /// Re-requests state from the daemon.
    pub fn load(&self, what: Refetch) {
        if what == Refetch::All {
            self.ipc.request(Request::Status, |r| {
                with(|c| match r {
                    Result::Ok(Ok::Status(s)) => {
                        c.store.borrow_mut().set_status(s);
                        refresh_input_banner(c);
                        settings::refresh(c);
                        pairing::refresh_lock(c);
                    }
                    other => c.report("Could not read the daemon status", other),
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
                    other => c.report("Could not list transfers", other),
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
                other => c.report("Could not list devices", other),
            });
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
                Change::Settings => settings::refresh(self),
                Change::Lock => pairing::refresh_lock(self),
                Change::InputSessions => refresh_input_banner(self),
                Change::Refetch(what) => self.load(what),
                Change::ShuttingDown => {
                    self.ipc.disconnect();
                    connection::on_disconnected(self);
                }
            }
        }
    }

    /// Shows a one-line notice that clears itself.
    pub fn notice(&self, text: &str, error: bool) {
        let Some(app) = self.app() else { return };
        let n = app.global::<NoticeState>();
        n.set_text(text.into());
        n.set_error(error);
        self.notice_timer
            .start(TimerMode::SingleShot, NOTICE_FOR, || {
                with(Controller::clear_notice);
            });
    }

    fn clear_notice(&self) {
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
        _ => "unknown",
    }
}
