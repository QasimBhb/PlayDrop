//! Devices screen: rows, one-click send, ping, clipboard, rename, grants, unpair.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use playdrop_ipc::{Ok, Request};
use slint::{ComponentHandle, Model, ModelRc};

use super::{with, Controller};
use crate::ipc::post;
use crate::model::{self, clipboard_sent_text};
use crate::{App, DevicesState};

/// Largest clipboard text the daemon sends (SR-24: 1 MiB; over 32 KiB it travels as a blob,
/// protocol.md 5.15).
const CLIPBOARD_MAX: usize = 1024 * 1024;

/// winit reports a multi-file drop as one `DroppedFile` per file, back to back. Files
/// arriving within this window are sent as one transfer.
const DROP_BATCH: Duration = Duration::from_millis(100);

thread_local! {
    /// Files dropped in the current batch, on the Slint thread.
    static DROPPED: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn install(app: &App, c: &Rc<Controller>) {
    let d = app.global::<DevicesState>();
    d.set_rows(ModelRc::from(Rc::clone(&c.devices)));
    d.on_send_files(|id| {
        with(|c| pick_and_send(c, id.to_string(), false));
    });
    d.on_send_folder(|id| {
        with(|c| pick_and_send(c, id.to_string(), true));
    });
    d.on_ping(|id| {
        with(|c| ping(c, id.as_str()));
    });
    d.on_send_clipboard(|id, text| {
        with(|c| send_clipboard(c, id.as_str(), text.as_str()));
    });
    d.on_rename(|id, label| {
        with(|c| rename(c, id.as_str(), label.as_str()));
    });
    d.on_set_grant(|id, grant, on| {
        with(|c| set_grant(c, id.as_str(), grant.as_str(), on));
    });
    d.on_unpair(|id| {
        with(|c| unpair(c, id.as_str()));
    });
    install_drop_target(app);
}

/// Drag and drop to send: files dropped on the window go to the selected device.
///
/// Works where winit delivers `WindowEvent::DroppedFile`: X11 (including `XWayland`) and
/// Windows. On a native Wayland surface winit 0.30 implements no drag-and-drop at all, so
/// nothing arrives here; the "Send files..." and "Send folder..." pickers (xdg portal)
/// stay the way to send on COSMIC and GNOME Wayland.
fn install_drop_target(app: &App) {
    use slint::winit_030::{winit::event::WindowEvent, EventResult, WinitWindowAccessor as _};

    app.window().on_winit_window_event(|_, event| {
        if let WindowEvent::DroppedFile(path) = event {
            let first = DROPPED.with(|d| {
                let mut d = d.borrow_mut();
                d.push(path.clone());
                d.len() == 1
            });
            if first {
                slint::Timer::single_shot(DROP_BATCH, || {
                    with(flush_drops);
                });
            }
            return EventResult::PreventDefault;
        }
        EventResult::Propagate
    });
}

/// Sends the current drop batch to the one selected device, or says why it cannot.
fn flush_drops(c: &Controller) {
    let paths = DROPPED.with(|d| std::mem::take(&mut *d.borrow_mut()));
    if paths.is_empty() {
        return;
    }
    let selected = c
        .app()
        .map_or(-1, |app| app.global::<DevicesState>().get_selected());
    let target = model::drop_target(selected, &c.store.borrow().peers);
    match target {
        Some(peer) => send_paths(c, peer, paths),
        None => c.notice("Select one device first, then drop the files on it.", true),
    }
}

/// Rebuilds every row.
pub(super) fn refresh_all(c: &Controller) {
    let now = model::unix_now();
    let store = c.store.borrow();
    let rows: Vec<_> = store
        .peers
        .iter()
        .map(|p| model::device_row(p, now))
        .collect();
    c.devices.set_vec(rows);
    if let Some(app) = c.app() {
        let d = app.global::<DevicesState>();
        let count = i32::try_from(store.peers.len()).unwrap_or(i32::MAX);
        if d.get_selected() >= count {
            d.set_selected(-1);
        }
        // With one device there is nothing to choose: show its actions straight away.
        if count == 1 && d.get_selected() < 0 {
            d.set_selected(0);
        }
    }
}

/// Updates one row in place.
pub(super) fn refresh_row(c: &Controller, i: usize) {
    let store = c.store.borrow();
    if let Some(p) = store.peers.get(i) {
        if i < c.devices.row_count() {
            c.devices
                .set_row_data(i, model::device_row(p, model::unix_now()));
        }
    }
}

/// Opens the system picker (the xdg portal on Linux) off the Slint thread, then sends what
/// was picked with no further question: one pick is one send.
fn pick_and_send(c: &Controller, peer: String, folder: bool) {
    c.ipc().runtime().spawn(async move {
        let dialog = rfd::AsyncFileDialog::new();
        let paths: Vec<PathBuf> = if folder {
            dialog
                .set_title("Send a folder with PlayDrop")
                .pick_folders()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|h| h.path().to_path_buf())
                .collect()
        } else {
            dialog
                .set_title("Send files with PlayDrop")
                .pick_files()
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|h| h.path().to_path_buf())
                .collect()
        };
        if paths.is_empty() {
            return;
        }
        post(move || {
            with(|c| send_paths(c, peer, paths));
        });
    });
}

/// Sends `paths` to `peer` and reports the start. Shared by the picker and any future
/// drop target.
fn send_paths(c: &Controller, peer: String, paths: Vec<PathBuf>) {
    let name = c.store.borrow().peer_name(&peer);
    let count = paths.len();
    c.ipc().send_files(peer, paths, move |r| {
        with(|c| match r {
            Result::Ok(Ok::TransferStarted { .. }) => {
                let what = if count == 1 {
                    "1 item".to_owned()
                } else {
                    format!("{count} items")
                };
                c.notice(&format!("Sending {what} to {name}"), false);
            }
            other => c.report(&format!("Could not send to {name}"), other),
        });
    });
}

fn ping(c: &Controller, peer: &str) {
    let name = c.store.borrow().peer_name(peer);
    c.ipc().request(
        Request::Ping {
            peer: peer.to_owned(),
            message: None,
        },
        move |r| {
            with(|c| c.done_or_report(&format!("Ping sent to {name}"), "Ping failed", r));
        },
    );
}

fn send_clipboard(c: &Controller, peer: &str, text: &str) {
    if text.is_empty() {
        c.notice("The clipboard is empty or does not hold text.", true);
        return;
    }
    if text.len() > CLIPBOARD_MAX {
        c.notice(
            "The clipboard text is larger than 1 MiB. Send it as a file instead.",
            true,
        );
        return;
    }
    c.ipc().request(
        Request::SendClipboard {
            peer: peer.to_owned(),
            text: text.to_owned(),
            sensitive: false,
        },
        |r| {
            with(|c| match r {
                Result::Ok(Ok::ClipboardPushed {
                    skipped_echo: true, ..
                }) => c.notice("That text was just synced; nothing new to send.", false),
                Result::Ok(Ok::ClipboardPushed { sent_to, .. }) => {
                    c.notice(&clipboard_sent_text(sent_to.len()), sent_to.is_empty());
                }
                other => c.report("Could not send the clipboard", other),
            });
        },
    );
}

fn rename(c: &Controller, peer: &str, label: &str) {
    let label = label.trim();
    if label.is_empty() {
        return;
    }
    c.ipc().request(
        Request::SetLabel {
            peer: peer.to_owned(),
            label: label.to_owned(),
        },
        |r| {
            with(|c| {
                if r.is_ok() {
                    c.load(model::Refetch::Peers);
                }
                c.done_or_report("", "Could not rename", r);
            });
        },
    );
}

fn set_grant(c: &Controller, peer: &str, grant: &str, on: bool) {
    let Some(i) = c.store.borrow().peer_index(peer) else {
        return;
    };
    let mut grants = c.store.borrow().peers[i].grants;
    let old = grants;
    match grant {
        "files" => grants.files = on,
        "clipboard_in" => grants.clipboard_in = on,
        "clipboard_out" => grants.clipboard_out = on,
        "find" => grants.find = on,
        // Input and media are shown but not offered until their phases (SR-14).
        _ => return,
    }
    // Show the change at once; snap back if the daemon refuses.
    c.store.borrow_mut().peers[i].grants = grants;
    refresh_row(c, i);
    let peer_id = peer.to_owned();
    c.ipc().request(
        Request::SetGrants {
            peer: peer.to_owned(),
            grants,
        },
        move |r| {
            with(|c| {
                if r.is_err() {
                    let idx = c.store.borrow().peer_index(&peer_id);
                    if let Some(i) = idx {
                        c.store.borrow_mut().peers[i].grants = old;
                        refresh_row(c, i);
                    }
                }
                c.done_or_report("", "Could not change permissions", r);
            });
        },
    );
}

fn unpair(c: &Controller, peer: &str) {
    let name = c.store.borrow().peer_name(peer);
    c.ipc().request(
        Request::Unpair {
            peer: peer.to_owned(),
        },
        move |r| {
            with(|c| {
                if r.is_ok() {
                    c.load(model::Refetch::Peers);
                }
                c.done_or_report(&format!("Unpaired {name}"), "Could not unpair", r);
            });
        },
    );
}
