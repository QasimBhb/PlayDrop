//! Settings screen: receive folder, device name, clipboard sync and images, autostart,
//! notifications, and this computer's identity.

use playdrop_ipc::{Ok, Request, SettingsPatch};
use slint::ComponentHandle;

use super::{with, Controller};
use crate::ipc::post;
use crate::model::{remote_input_line, remote_input_reason};
use crate::{App, DevicesState, SettingsState};

pub(super) fn install(app: &App) {
    let s = app.global::<SettingsState>();
    s.on_choose_receive_dir(|| {
        with(choose_receive_dir);
    });
    s.on_set_device_name(|name| {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        with(|c| {
            patch(
                c,
                SettingsPatch {
                    device_name: Some(name),
                    ..SettingsPatch::default()
                },
            );
        });
    });
    s.on_set_clipboard_sync(|on| {
        with(|c| {
            patch(
                c,
                SettingsPatch {
                    clipboard_sync: Some(on),
                    ..SettingsPatch::default()
                },
            );
        });
    });
    s.on_set_clipboard_images(|on| {
        with(|c| {
            patch(
                c,
                SettingsPatch {
                    clipboard_images: Some(on),
                    ..SettingsPatch::default()
                },
            );
        });
    });
    s.on_set_autostart(|on| {
        with(|c| {
            patch(
                c,
                SettingsPatch {
                    autostart: Some(on),
                    ..SettingsPatch::default()
                },
            );
        });
    });
    s.on_set_notifications(|on| {
        with(|c| {
            patch(
                c,
                SettingsPatch {
                    notifications: Some(on),
                    ..SettingsPatch::default()
                },
            );
        });
    });
}

/// Shows the stored status and settings.
pub(super) fn refresh(c: &Controller) {
    let store = c.store.borrow();
    let (Some(status), Some(app)) = (store.status.as_ref(), c.app()) else {
        return;
    };
    let s = app.global::<SettingsState>();
    let set = &status.settings;
    s.set_receive_dir(set.receive_dir.display().to_string().into());
    s.set_device_name(set.device_name.as_str().into());
    s.set_clipboard_sync(set.clipboard_sync);
    s.set_clipboard_images(set.clipboard_images);
    s.set_clipboard_watch_supported(set.clipboard_watch_supported);
    s.set_autostart(set.autostart);
    s.set_notifications(set.notifications);
    s.set_remote_input(remote_input_line(set).into());
    let d = app.global::<DevicesState>();
    d.set_input_supported(set.remote_input_supported);
    d.set_input_reason(remote_input_reason(set).into());
    s.set_short_id(status.short_id.as_str().into());
    s.set_device_id(status.device_id.as_str().into());
    s.set_about(
        format!(
            "Listening on {} · daemon {} · window {}",
            status.addr,
            status.daemon_version,
            env!("CARGO_PKG_VERSION")
        )
        .into(),
    );
}

fn patch(c: &Controller, patch: SettingsPatch) {
    c.ipc().request(Request::SetSettings(patch), |r| {
        with(|c| match r {
            Result::Ok(Ok::Settings {
                settings,
                restarted,
            }) => {
                c.store.borrow_mut().set_settings(settings);
                refresh(c);
                if restarted {
                    c.notice("Settings saved. Reconnecting devices...", false);
                }
            }
            other => {
                // Snap the switches back to what the daemon has.
                refresh(c);
                c.report("Could not save the setting", other);
            }
        });
    });
}

fn choose_receive_dir(c: &Controller) {
    c.ipc().runtime().spawn(async {
        let Some(folder) = rfd::AsyncFileDialog::new()
            .set_title("Folder for received files")
            .pick_folder()
            .await
        else {
            return;
        };
        let path = folder.path().to_path_buf();
        post(move || {
            with(|c| {
                patch(
                    c,
                    SettingsPatch {
                        receive_dir: Some(path),
                        ..SettingsPatch::default()
                    },
                );
            });
        });
    });
}
