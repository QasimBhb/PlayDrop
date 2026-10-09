//! Add device, the pairing prompt (SR-8) and the QR code (SR-9).

use std::rc::Rc;
use std::time::{Duration, Instant};

use playdrop_ipc::{ErrorCode, IpcError, Ok, Request};
use slint::{ComponentHandle, Image, ModelRc, TimerMode};

use super::{with, Controller};
use crate::model::{self, format_sas, parse_pair_addr};
use crate::{qr, App, PairingState};

/// How long the daemon keeps the add-device window open (SR-6).
const WINDOW_FOR: Duration = Duration::from_secs(120);

pub(super) fn install(app: &App, c: &Rc<Controller>) {
    let p = app.global::<PairingState>();
    p.set_candidates(ModelRc::from(Rc::clone(&c.candidates)));
    p.on_open_window(|| {
        with(open_window);
    });
    p.on_close_window(|| {
        with(close_window);
    });
    p.on_pair_with(|addr| {
        with(|c| pair_with(c, addr.as_str()));
    });
    p.on_respond(|accept| {
        with(|c| respond(c, accept));
    });
    p.on_show_qr(|| {
        with(show_qr);
    });
    p.on_hide_qr(|| {
        with(hide_qr);
    });
}

/// Runs `f` with the pairing global, if the window still exists. A global borrows its
/// component, so it is looked up per call instead of stored.
fn with_state(c: &Controller, f: impl FnOnce(&PairingState<'_>)) {
    if let Some(app) = c.app() {
        f(&app.global::<PairingState>());
    }
}

/// Shows a pairing outcome or problem on the Add device screen.
pub(super) fn set_status(c: &Controller, text: &str, error: bool) {
    with_state(c, |p| {
        p.set_status(text.into());
        p.set_status_error(error);
    });
}

/// Shows the daemon's refusal: the locked reason goes in its own banner (SR-6).
fn show_error(c: &Controller, what: &str, e: &IpcError) {
    if e.code == ErrorCode::Forbidden {
        with_state(c, |p| {
            p.set_blocked_reason(format!("{what}: {}.", e.message).into());
        });
    } else {
        set_status(c, &format!("{what}: {}", e.message), true);
    }
}

/// Mirrors the daemon's lock state (SR-6).
pub(super) fn refresh_lock(c: &Controller) {
    let locked = c.store.borrow().locked();
    let open = c
        .store
        .borrow()
        .status
        .as_ref()
        .is_some_and(|s| s.add_device_window_open);
    with_state(c, |p| {
        p.set_blocked_reason(if locked {
            "The desktop is locked. Unlock it to add a device.".into()
        } else {
            "".into()
        });
    });
    if locked {
        window_closed(c);
    } else if open && c.add_deadline.get().is_none() {
        // Opened by another client (the tray): show it here too.
        window_opened(c);
    }
}

pub(super) fn refresh_candidates(c: &Controller) {
    let rows: Vec<_> = c
        .store
        .borrow()
        .candidates
        .iter()
        .map(model::candidate_row)
        .collect();
    c.candidates.set_vec(rows);
}

pub(super) fn refresh_prompt(c: &Controller) {
    let prompt = c.store.borrow().prompt.clone();
    with_state(c, |p| {
        if let Some(v) = prompt {
            p.set_prompt_is_sas(v.sas.is_some());
            p.set_prompt_sas(v.sas.as_deref().map(format_sas).unwrap_or_default().into());
            p.set_prompt_name(v.name.into());
            p.set_prompt_short_id(v.short_id.into());
            p.set_prompt_visible(true);
        } else {
            p.set_prompt_visible(false);
            p.set_prompt_sas("".into());
        }
    });
    if c.store.borrow().prompt.is_some() {
        if let Some(app) = c.app() {
            // Bring the window forward so the user sees the code next to the phone's.
            let _ = app.show();
        }
    }
}

fn format_remaining(left: Duration) -> String {
    let s = left.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

fn window_opened(c: &Controller) {
    c.add_deadline.set(Some(Instant::now() + WINDOW_FOR));
    with_state(c, |p| {
        p.set_window_open(true);
        p.set_remaining(format_remaining(WINDOW_FOR).into());
        p.set_status("".into());
    });
    c.add_timer
        .start(TimerMode::Repeated, Duration::from_secs(1), || {
            with(|c| {
                let Some(deadline) = c.add_deadline.get() else {
                    c.add_timer.stop();
                    return;
                };
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    window_closed(c);
                } else {
                    with_state(c, |p| p.set_remaining(format_remaining(left).into()));
                }
            });
        });
    c.ipc().request(Request::AddDeviceList, |r| {
        with(|c| match r {
            Result::Ok(Ok::AddDeviceList(list)) => {
                c.store.borrow_mut().candidates = model::dedup_candidates(list);
                refresh_candidates(c);
            }
            other => c.report("Could not list nearby devices", other),
        });
    });
}

/// The window ended (timeout, success, lock, or Stop).
pub(super) fn window_closed(c: &Controller) {
    c.add_timer.stop();
    c.add_deadline.set(None);
    c.store.borrow_mut().candidates.clear();
    refresh_candidates(c);
    with_state(c, |p| p.set_window_open(false));
}

fn open_window(c: &Controller) {
    c.ipc().request(Request::OpenAddDeviceWindow, |r| {
        with(|c| match r {
            Result::Ok(_) => {
                with_state(c, |p| p.set_blocked_reason("".into()));
                window_opened(c);
            }
            Result::Err(e) => show_error(c, "Cannot add a device", &e),
        });
    });
}

fn close_window(c: &Controller) {
    window_closed(c);
    c.ipc().request(Request::CloseAddDeviceWindow, |r| {
        with(|c| c.done_or_report("", "Could not stop looking", r));
    });
}

fn pair_with(c: &Controller, text: &str) {
    let Some(addr) = parse_pair_addr(text) else {
        set_status(
            c,
            &format!(
                "\"{}\" is not an address. Type an IP such as 192.168.1.20.",
                text.trim()
            ),
            true,
        );
        return;
    };
    set_status(
        c,
        &format!("Connecting to {addr}. Compare the six digits on both screens."),
        false,
    );
    c.ipc().request(Request::PairWith { addr }, |r| {
        with(|c| {
            if let Result::Err(e) = r {
                show_error(c, "Could not pair", &e);
            }
        });
    });
}

fn respond(c: &Controller, accept: bool) {
    c.store.borrow_mut().prompt = None;
    refresh_prompt(c);
    if accept {
        set_status(c, "Waiting for the other device to confirm...", false);
    } else {
        set_status(c, "Pairing rejected.", false);
    }
    c.ipc().request(Request::PairingRespond { accept }, |r| {
        with(|c| {
            if let Result::Err(e) = r {
                set_status(c, &format!("Could not answer: {}", e.message), true);
            }
        });
    });
}

fn show_qr(c: &Controller) {
    with_state(c, |p| {
        p.set_qr_ready(false);
        p.set_qr_uri("".into());
        p.set_qr_image(Image::default());
        p.set_qr_warning("".into());
        p.set_qr_visible(true);
    });
    // Protect the window before the code is drawn (SR-9).
    let protected = c
        .app()
        .map_or(Ok(()), |app| qr::set_capture_excluded(app.window(), true));
    if let Err(e) = protected {
        tracing::warn!("capture protection unavailable: {e}");
        with_state(c, |p| {
            p.set_qr_warning(
                format!("Screen capture protection is not available ({e}). Make sure nobody records the screen.")
                    .into(),
            );
        });
    }
    c.ipc().request(Request::ShowQr, |r| {
        with(|c| match r {
            Result::Ok(Ok::QrUri(uri)) => {
                // The URI is a one-use secret: shown, never logged (SR-32).
                match qr::render(&uri) {
                    Result::Ok(image) => with_state(c, |p| {
                        p.set_qr_image(image);
                        p.set_qr_uri(uri.into());
                        p.set_qr_ready(true);
                    }),
                    Result::Err(e) => {
                        hide_qr(c);
                        set_status(c, &format!("Could not draw the QR code: {e}"), true);
                    }
                }
                if c.add_deadline.get().is_none() {
                    window_opened(c);
                }
            }
            Result::Ok(_) => hide_qr(c),
            Result::Err(e) => {
                hide_qr(c);
                show_error(c, "Cannot show a QR code", &e);
            }
        });
    });
}

fn hide_qr(c: &Controller) {
    with_state(c, |p| {
        p.set_qr_visible(false);
        p.set_qr_ready(false);
        p.set_qr_uri("".into());
        p.set_qr_image(Image::default());
    });
    let lifted = c
        .app()
        .map_or(Ok(()), |app| qr::set_capture_excluded(app.window(), false));
    if let Err(e) = lifted {
        tracing::warn!("could not lift capture protection: {e}");
    }
}
