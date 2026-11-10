//! What the tray menu shows, as plain data built from a [`TrayView`].
//!
//! Both tray backends render this list (`ksni` on Linux, `tray-icon` on Windows), so the
//! two menus say the same thing and the wording is unit-tested on every platform. A
//! renderer only maps each [`MenuEntry`] to a native item and sends the [`TrayCommand`] it
//! carries when clicked.

use super::{device_line, TrayCommand, TrayView};

/// One line of the tray menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuEntry {
    /// A text item. `command` is what a click sends; `None` makes it a caption.
    Item {
        /// Menu text.
        label: String,
        /// Clickable (greyed out when false).
        enabled: bool,
        /// Sent to the daemon on click.
        command: Option<TrayCommand>,
    },
    /// "Pause clipboard sync", a check item. A click flips `checked` and sends
    /// [`TrayCommand::SetClipboardPaused`] with the new state.
    PauseClipboard {
        /// Clipboard sync runs at all (setting on and platform supported).
        enabled: bool,
        /// Sync is paused.
        checked: bool,
    },
    /// A separator line.
    Separator,
}

/// The label of the pause item.
pub const PAUSE_LABEL: &str = "Pause clipboard sync";

impl MenuEntry {
    fn action(label: &str, command: TrayCommand) -> Self {
        Self::Item {
            label: label.to_owned(),
            enabled: true,
            command: Some(command),
        }
    }
}

/// The menu for `view`, top to bottom: one line per paired device (or "No paired
/// devices"), the actions, the pause check item, the remote input kill switch while a peer
/// controls this desktop (SR-25), and "Quit".
pub fn menu_entries(view: &TrayView) -> Vec<MenuEntry> {
    let mut items = Vec::with_capacity(view.devices.len() + 10);
    if view.devices.is_empty() {
        items.push(MenuEntry::Item {
            label: "No paired devices".to_owned(),
            enabled: false,
            command: None,
        });
    }
    for d in &view.devices {
        items.push(MenuEntry::action(&device_line(d), TrayCommand::OpenWindow));
    }
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::action("Add device", TrayCommand::AddDevice));
    items.push(MenuEntry::action(
        "Send clipboard",
        TrayCommand::SendClipboard,
    ));
    items.push(MenuEntry::action("Open window", TrayCommand::OpenWindow));
    items.push(MenuEntry::PauseClipboard {
        enabled: view.clipboard_sync,
        checked: view.clipboard_paused,
    });
    if !view.input_from.is_empty() {
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::action(
            &format!("Stop remote input from {}", view.input_from.join(", ")),
            TrayCommand::StopInput,
        ));
    }
    items.push(MenuEntry::Separator);
    items.push(MenuEntry::action("Quit", TrayCommand::Quit));
    items
}

/// The command a click on the item with `id` sends, where `ids` holds each entry's item ID
/// in the same order as `entries`; `None` for an unknown ID, a separator or a caption.
///
/// A click on the pause item records the new state in `entries`, so a later view that
/// agrees with it compares equal and the menu is not rebuilt. `pause_checked` is the
/// native item's state after the click when the toolkit flips it itself (Windows); without
/// it the entry is flipped here.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn clicked<I: PartialEq>(
    entries: &mut [MenuEntry],
    ids: &[I],
    id: &I,
    pause_checked: Option<bool>,
) -> Option<TrayCommand> {
    let at = ids.iter().position(|i| i == id)?;
    match entries.get_mut(at)? {
        MenuEntry::Item { command, .. } => *command,
        MenuEntry::PauseClipboard { checked, .. } => {
            let paused = pause_checked.unwrap_or(!*checked);
            *checked = paused;
            Some(TrayCommand::SetClipboardPaused(paused))
        }
        MenuEntry::Separator => None,
    }
}

/// Menu text for Win32: `&` marks a mnemonic there and a tab starts the accelerator
/// column, so a peer-chosen label is shown literally by doubling `&` and turning tabs into
/// spaces. Not used on Linux, whose menu text is plain.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub fn win32_menu_text(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    for c in label.chars() {
        match c {
            '&' => out.push_str("&&"),
            '\t' => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// "1 of 2 devices online", the tray tooltip's second line.
pub fn online_summary(view: &TrayView) -> String {
    let online = view.devices.iter().filter(|d| d.online).count();
    format!("{online} of {} devices online", view.devices.len())
}

#[cfg(test)]
mod tests {
    use super::super::TrayDevice;
    use super::*;

    fn device(label: &str, online: bool, battery: Option<u8>, seen: Option<u64>) -> TrayDevice {
        TrayDevice {
            label: label.into(),
            online,
            battery,
            seen_secs_ago: seen,
        }
    }

    fn labels(items: &[MenuEntry]) -> Vec<String> {
        items
            .iter()
            .map(|e| match e {
                MenuEntry::Item { label, .. } => label.clone(),
                MenuEntry::PauseClipboard { .. } => PAUSE_LABEL.to_owned(),
                MenuEntry::Separator => "---".to_owned(),
            })
            .collect()
    }

    #[test]
    fn empty_list_says_no_paired_devices() {
        let items = menu_entries(&TrayView::default());
        assert_eq!(
            labels(&items),
            [
                "No paired devices",
                "---",
                "Add device",
                "Send clipboard",
                "Open window",
                PAUSE_LABEL,
                "---",
                "Quit"
            ]
        );
        assert_eq!(
            items[0],
            MenuEntry::Item {
                label: "No paired devices".into(),
                enabled: false,
                command: None,
            }
        );
        assert_eq!(
            online_summary(&TrayView::default()),
            "0 of 0 devices online"
        );
    }

    #[test]
    fn devices_online_and_offline_with_and_without_battery() {
        let view = TrayView {
            devices: vec![
                device("Phone", true, Some(80), Some(0)),
                device("Tablet", false, None, Some(7200)),
                device("Old phone", false, Some(12), None),
                device("Laptop", true, None, None),
            ],
            ..TrayView::default()
        };
        let items = menu_entries(&view);
        assert_eq!(
            labels(&items)[..4],
            [
                "Phone - online, battery 80%",
                "Tablet - offline, seen 2 h ago",
                "Old phone - offline, battery 12%",
                "Laptop - online",
            ]
        );
        // Every device line is clickable and opens the window, as on Linux.
        for e in &items[..4] {
            let MenuEntry::Item {
                enabled, command, ..
            } = e
            else {
                panic!("device line should be an item");
            };
            assert!(enabled);
            assert_eq!(*command, Some(TrayCommand::OpenWindow));
        }
        assert!(!labels(&items).contains(&"No paired devices".to_owned()));
        assert_eq!(online_summary(&view), "2 of 4 devices online");
    }

    #[test]
    fn remote_input_adds_the_kill_switch() {
        let mut view = TrayView {
            devices: vec![device("Phone", true, None, None)],
            input_from: vec!["Phone".into(), "Tablet".into()],
            ..TrayView::default()
        };
        let items = menu_entries(&view);
        let stop = MenuEntry::Item {
            label: "Stop remote input from Phone, Tablet".into(),
            enabled: true,
            command: Some(TrayCommand::StopInput),
        };
        let at = items.iter().position(|e| *e == stop).expect("kill switch");
        assert_eq!(
            labels(&items[at - 2..]),
            [
                PAUSE_LABEL,
                "---",
                "Stop remote input from Phone, Tablet",
                "---",
                "Quit"
            ]
        );
        assert_eq!(items.len(), at + 3);

        view.input_from.clear();
        assert!(!menu_entries(&view).contains(&stop));
    }

    #[test]
    fn clicks_map_to_commands_by_id() {
        let view = TrayView {
            devices: vec![device("Phone", true, None, None)],
            clipboard_sync: true,
            ..TrayView::default()
        };
        let mut entries = menu_entries(&view);
        let ids: Vec<usize> = (100..100 + entries.len()).collect();
        // Phone, separator, Add device, Send clipboard, Open window, Pause, separator, Quit.
        assert_eq!(
            clicked(&mut entries, &ids, &100, None),
            Some(TrayCommand::OpenWindow)
        );
        assert_eq!(clicked(&mut entries, &ids, &101, None), None);
        assert_eq!(
            clicked(&mut entries, &ids, &102, None),
            Some(TrayCommand::AddDevice)
        );
        assert_eq!(
            clicked(&mut entries, &ids, &107, None),
            Some(TrayCommand::Quit)
        );
        // An ID that is not in this menu (an old menu, another item) sends nothing.
        assert_eq!(clicked(&mut entries, &ids, &999, None), None);
        assert_eq!(clicked(&mut entries, &ids, &99, Some(true)), None);
        assert_eq!(entries, menu_entries(&view));

        // The caption of an empty list sends nothing.
        let mut empty = menu_entries(&TrayView::default());
        assert_eq!(clicked(&mut empty, &[0usize], &0, None), None);
    }

    #[test]
    fn pause_click_updates_the_entry_so_an_equal_view_does_not_rebuild() {
        let mut view = TrayView {
            clipboard_sync: true,
            ..TrayView::default()
        };
        let mut entries = menu_entries(&view);
        let ids: Vec<usize> = (0..entries.len()).collect();
        let pause = entries
            .iter()
            .position(|e| matches!(e, MenuEntry::PauseClipboard { .. }))
            .expect("pause item");
        // Windows: the native item already flipped itself and reports checked.
        assert_eq!(
            clicked(&mut entries, &ids, &pause, Some(true)),
            Some(TrayCommand::SetClipboardPaused(true))
        );
        view.clipboard_paused = true;
        assert_eq!(entries, menu_entries(&view), "no rebuild needed");
        // Without a native state the entry flips itself back.
        assert_eq!(
            clicked(&mut entries, &ids, &pause, None),
            Some(TrayCommand::SetClipboardPaused(false))
        );
        view.clipboard_paused = false;
        assert_eq!(entries, menu_entries(&view));
    }

    #[test]
    fn win32_text_shows_ampersands_and_tabs_literally() {
        assert_eq!(win32_menu_text("Tom & Jerry"), "Tom && Jerry");
        assert_eq!(win32_menu_text("&&a&"), "&&&&a&&");
        assert_eq!(win32_menu_text("Phone\tCtrl+Q"), "Phone Ctrl+Q");
        assert_eq!(win32_menu_text("Phone - online"), "Phone - online");
        // The Linux menu keeps the label as it is.
        let view = TrayView {
            devices: vec![device("Tom & Jerry\tA", true, None, None)],
            ..TrayView::default()
        };
        assert_eq!(labels(&menu_entries(&view))[0], "Tom & Jerry\tA - online");
    }

    #[test]
    fn pause_item_follows_the_view() {
        let view = TrayView {
            clipboard_sync: true,
            clipboard_paused: true,
            ..TrayView::default()
        };
        assert!(menu_entries(&view).contains(&MenuEntry::PauseClipboard {
            enabled: true,
            checked: true,
        }));
        assert!(
            menu_entries(&TrayView::default()).contains(&MenuEntry::PauseClipboard {
                enabled: false,
                checked: false,
            })
        );
    }
}
