//! Diagnostics screen: run `Diagnose`, show the summary first and the layers after, and
//! tell the daemon the network changed. A failed layer brings up the "Can't find your
//! device?" panel with the fix the summary points at first.

use std::rc::Rc;

use playdrop_ipc::{Ok, Request};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{with, Controller};
use crate::model;
use crate::{App, DiagnosticsState, HelpState};

pub(super) fn install(app: &App) {
    let d = app.global::<DiagnosticsState>();
    d.on_refresh(|| {
        with(refresh);
    });
    d.on_network_changed(|| {
        with(|c| {
            c.ipc().request(Request::NetworkChanged, |r| {
                with(|c| {
                    c.done_or_report(
                        "Looking for devices on the new network...",
                        "Could not retry",
                        r,
                    );
                    refresh(c);
                });
            });
        });
    });
}

fn with_state(c: &Controller, f: impl FnOnce(&DiagnosticsState<'_>)) {
    if let Some(app) = c.app() {
        f(&app.global::<DiagnosticsState>());
    }
}

/// Fetches diagnostics. Also called on connect when the window opened on this tab
/// (`--page diagnostics`), since the tab change callback does not fire for the first tab.
pub fn refresh(c: &Controller) {
    with_state(c, |d| {
        d.set_loading(true);
        d.set_error("".into());
    });
    c.ipc().request(Request::Diagnose, |r| {
        with(|c| {
            with_state(c, |d| {
                d.set_loading(false);
                match r {
                    Result::Ok(Ok::Diagnostics(report)) => {
                        let (summary, sections) = model::diagnostics_view(&report);
                        d.set_failed(model::network_failed(&summary));
                        if let Some(app) = c.app() {
                            let (focus, fix) = model::help_focus(&summary);
                            let help = app.global::<HelpState>();
                            help.set_focus(focus);
                            help.set_firewall_fix(fix.into());
                        }
                        let summary: Vec<SharedString> =
                            summary.into_iter().map(SharedString::from).collect();
                        d.set_summary(ModelRc::from(Rc::new(VecModel::from(summary))));
                        d.set_sections(ModelRc::from(Rc::new(VecModel::from(sections))));
                    }
                    Result::Ok(_) => d.set_error("Unexpected answer from the daemon.".into()),
                    Result::Err(e) => {
                        d.set_error(format!("Diagnostics failed: {}", e.message).into());
                    }
                }
            });
        });
    });
}
