//! Transfers screen: offers, progress, results, cancel, open and show in folder.

use std::path::PathBuf;
use std::rc::Rc;

use playdrop_core::host::{TransferDirection, TransferStatus};
use playdrop_ipc::Request;
use slint::{ComponentHandle, Model, ModelRc};

use super::{with, Controller};
use crate::model;
use crate::{App, TransfersState};

pub(super) fn install(app: &App, c: &Rc<Controller>) {
    let t = app.global::<TransfersState>();
    t.set_rows(ModelRc::from(Rc::clone(&c.transfers)));
    t.on_respond(|id, accept| {
        with(|c| respond(c, id.to_string(), accept));
    });
    t.on_cancel(|id| {
        with(|c| cancel(c, id.to_string()));
    });
    t.on_open(|id| {
        with(|c| open(c, id.as_str(), false));
    });
    t.on_show_in_folder(|id| {
        with(|c| open(c, id.as_str(), true));
    });
    t.on_clear_finished(|| {
        with(|c| {
            c.store.borrow_mut().clear_finished();
            refresh_all(c);
        });
    });
}

fn row(c: &Controller, i: usize) -> Option<crate::TransferRow> {
    let store = c.store.borrow();
    let t = store.transfers.get(i)?;
    Some(model::transfer_row(t, &store.peer_name(&t.peer)))
}

fn update_pending(c: &Controller) {
    let pending = c
        .store
        .borrow()
        .transfers
        .iter()
        .filter(|t| t.direction == TransferDirection::Receive && t.state == TransferStatus::Offered)
        .count();
    if let Some(app) = c.app() {
        app.global::<TransfersState>()
            .set_pending_offers(i32::try_from(pending).unwrap_or(i32::MAX));
    }
}

/// Rebuilds every row, newest first.
pub(super) fn refresh_all(c: &Controller) {
    let n = c.store.borrow().transfers.len();
    let rows: Vec<_> = (0..n).rev().filter_map(|i| row(c, i)).collect();
    c.transfers.set_vec(rows);
    update_pending(c);
}

/// Updates one row in place. Store order is oldest first; the model shows newest first.
pub(super) fn refresh_row(c: &Controller, i: usize) {
    let n = c.store.borrow().transfers.len();
    let Some(r) = row(c, i) else { return };
    let at = n - 1 - i;
    if at < c.transfers.row_count() && n == c.transfers.row_count() {
        c.transfers.set_row_data(at, r);
        update_pending(c);
    } else {
        refresh_all(c);
    }
}

fn respond(c: &Controller, id: String, accept: bool) {
    c.ipc().request(Request::RespondOffer { id, accept }, |r| {
        with(|c| c.done_or_report("", "Could not answer the offer", r));
    });
}

fn cancel(c: &Controller, id: String) {
    c.ipc().request(Request::CancelTransfer { id }, |r| {
        with(|c| c.done_or_report("", "Could not cancel", r));
    });
}

/// "Open" opens the single received file; "Show in folder" reveals the first one. The
/// daemon applies the SR-23 rule (executables open their folder instead).
fn open(c: &Controller, id: &str, reveal: bool) {
    let path: Option<PathBuf> = c
        .store
        .borrow()
        .transfers
        .iter()
        .find(|t| t.id == id && t.direction == TransferDirection::Receive)
        .and_then(|t| t.saved.first().cloned());
    let Some(path) = path else { return };
    let request = if reveal {
        Request::ShowInFolder { path }
    } else {
        Request::OpenReceived { path }
    };
    c.ipc().request(request, |r| {
        with(|c| c.done_or_report("", "Could not open", r));
    });
}
