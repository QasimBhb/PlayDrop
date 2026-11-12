//! The update banner above every tab and the update section of Settings
//! (`docs/design/update-check.md`, `docs/ipc.md` 1.9).
//!
//! The window only asks and shows: the daemon does the check, the download and the
//! install. The first connection of a window is "a UI opened", so it asks for a check
//! (which the daemon makes at most once a day); later reconnections only read the state.

use playdrop_ipc::{ErrorCode, IpcError, Ok, Request, SettingsPatch, UpdateState};
use slint::ComponentHandle;

use super::{settings, with, Controller};
use crate::model::{self, UpdateAction};
use crate::ui::UpdateUi;
use crate::App;

pub(super) fn install(app: &App) {
    let u = app.global::<UpdateUi>();
    u.on_primary(|| {
        with(primary);
    });
    u.on_later(|| {
        with(|c| ask(c, Request::UpdateLater, false));
    });
    u.on_open_page(|| {
        with(open_page);
    });
    u.on_set_enabled(|on| {
        with(|c| {
            settings::patch(
                c,
                SettingsPatch {
                    update_check: Some(on),
                    ..SettingsPatch::default()
                },
            );
        });
    });
    u.on_check_now(|| {
        with(|c| ask(c, Request::UpdateCheck { force: true }, true));
    });
}

/// Called on every connection to the daemon.
pub(super) fn on_connected(c: &Controller) {
    let first = !c.update_asked.replace(true);
    let request = if first {
        Request::UpdateCheck { force: false }
    } else {
        Request::UpdateState
    };
    ask(c, request, false);
}

/// Sends an update request and stores the state it answers. With `check_now` a result the
/// banner does not show ("up to date", a failure) gets a notice, so the button visibly did
/// something.
fn ask(c: &Controller, request: Request, check_now: bool) {
    c.ipc().request(request, move |r| {
        with(|c| match r {
            Result::Ok(Ok::Update(status)) => {
                if check_now {
                    if let Some((text, error)) = model::check_now_notice(&status) {
                        c.notice(&text, error);
                    }
                }
                c.store.borrow_mut().update = Some(status);
                refresh(c);
            }
            // A daemon older than 0.11.0 does not know the request.
            Result::Err(IpcError {
                code: ErrorCode::BadRequest,
                ..
            }) => {
                c.store.borrow_mut().update_unsupported = true;
                refresh(c);
            }
            other => c.report("Update", other),
        });
    });
}

/// The banner's main button: install, or the release page where this copy cannot install.
fn primary(c: &Controller) {
    let action = model::update_action(c.store.borrow().update.as_ref());
    match action {
        // The window asks, so the Windows installer starts the window again too.
        UpdateAction::Install => ask(c, Request::UpdateInstall { window: true }, false),
        UpdateAction::Cancel => ask(c, Request::UpdateCancel, false),
        UpdateAction::OpenPage => open_page(c),
        UpdateAction::None => {}
    }
}

/// "What is new" and "Open release page": the daemon opens the page of the release it
/// offers.
fn open_page(c: &Controller) {
    c.ipc().request(Request::UpdateOpenPage, |r| {
        with(|c| c.done_or_report("", "Could not open the release page", r));
    });
}

/// Shows the stored update state in the banner and in Settings.
pub(super) fn refresh(c: &Controller) {
    let Some(app) = c.app() else { return };
    let store = c.store.borrow();
    let status = store.update.as_ref();
    let banner = model::update_banner(
        status,
        store.status.as_ref().map(|s| s.daemon_version.as_str()),
        env!("CARGO_PKG_VERSION"),
        cfg!(windows),
    );
    let u = app.global::<UpdateUi>();
    u.set_banner_visible(banner.visible);
    u.set_banner_text(banner.text.into());
    u.set_banner_detail(banner.detail.into());
    u.set_banner_error(banner.error);
    u.set_primary_text(banner.primary.into());
    u.set_show_later(banner.later);
    u.set_show_link(banner.link);
    u.set_enabled(
        store
            .status
            .as_ref()
            .is_none_or(|s| s.settings.update_check),
    );
    // An older daemon, or a build that is not a release: no switch and no "Check now",
    // and the line beside them says why.
    u.set_supported(!store.update_unsupported && status.is_none_or(|s| s.unavailable.is_none()));
    u.set_checking(status.is_some_and(|s| s.state == UpdateState::Checking));
    u.set_last_checked(
        model::update_last_checked(status, store.update_unsupported, model::unix_now()).into(),
    );
    u.set_last_failure(model::update_failure_text(status).into());
}
