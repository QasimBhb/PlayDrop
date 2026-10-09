import Foundation

// Strings of the Devices tab and the Device screen, keyed like Android `strings_ui.xml`.
// See `L10n.swift` for the scheme.

extension L10n {
    /// Devices tab.
    enum Devices {
        /// "Devices".
        static var title: String { String(localized: "ui_devices_title", defaultValue: "Devices") }
        /// "Add device".
        static var addDevice: String { String(localized: "ui_add_device", defaultValue: "Add device") }
        /// "Send files".
        static var sendFiles: String { String(localized: "ui_send_files", defaultValue: "Send files") }
        /// "last seen 5 min ago".
        static func lastSeen(_ ago: String) -> String {
            String(localized: "ui_device_last_seen", defaultValue: "last seen \(ago)")
        }
        /// "removed by the other device".
        static var removedShort: String {
            String(localized: "ui_device_removed_short", defaultValue: "removed by the other device")
        }
        /// Empty state title.
        static var emptyTitle: String {
            String(localized: "ui_devices_empty_title", defaultValue: "No paired devices yet")
        }
        /// Empty state body.
        static var emptyBody: String {
            String(
                localized: "ui_devices_empty_body",
                defaultValue: "Open PlayDrop on your computer, choose Add device on both, then check that both screens show the same six digits."
            )
        }
        /// Banner when the Local Network permission looks denied.
        static var localNetworkBanner: String {
            String(
                localized: "ui_devices_local_network_banner",
                defaultValue: "PlayDrop cannot reach your network. Tap to fix it."
            )
        }
    }

    /// Device screen.
    enum Detail {
        /// "Device".
        static var title: String { String(localized: "ui_detail_title", defaultValue: "Device") }
        /// The device is gone.
        static var gone: String {
            String(localized: "ui_detail_gone", defaultValue: "This device is no longer paired.")
        }
        /// The other side removed us.
        static var removed: String {
            String(
                localized: "ui_detail_removed",
                defaultValue: "The other device removed this iPhone. Pair again to use it."
            )
        }
        /// "Pair again".
        static var pairAgain: String { String(localized: "ui_detail_pair_again", defaultValue: "Pair again") }
        /// "paired 12 Oct 2026".
        static func pairedOn(_ date: String) -> String {
            String(localized: "ui_detail_paired_on", defaultValue: "paired \(date)")
        }
        /// "Set as default".
        static var setDefault: String { String(localized: "ui_detail_set_default", defaultValue: "Set as default") }
        /// Explains the default device.
        static var isDefault: String {
            String(
                localized: "ui_detail_is_default",
                defaultValue: "Default device: Send files goes here with one tap."
            )
        }
        /// Confirmation after setting the default.
        static var defaultSet: String {
            String(localized: "ui_detail_default_set", defaultValue: "Set as the default device")
        }
        /// "Send clipboard".
        static var sendClipboard: String {
            String(localized: "action_send_clipboard", defaultValue: "Send clipboard")
        }
        /// "Ping".
        static var ping: String { String(localized: "ui_detail_ping", defaultValue: "Ping") }
        /// Ping dialog body.
        static var pingBody: String {
            String(
                localized: "ui_detail_ping_body",
                defaultValue: "Shows a notification on the other device. The message is optional."
            )
        }
        /// Ping message placeholder.
        static var pingPlaceholder: String {
            String(localized: "ui_detail_ping_placeholder", defaultValue: "Message")
        }
        /// "Send".
        static var pingSend: String { String(localized: "ui_detail_ping_send", defaultValue: "Send") }
        /// "Ping sent".
        static var pingSent: String { String(localized: "ui_detail_ping_sent", defaultValue: "Ping sent") }
        /// "Find".
        static var find: String { String(localized: "ui_detail_find", defaultValue: "Find") }
        /// "Stop ringing".
        static var findStop: String { String(localized: "ui_detail_find_stop", defaultValue: "Stop ringing") }
        /// "Ringing".
        static var ringing: String { String(localized: "ui_detail_ringing", defaultValue: "Ringing") }
        /// "Connect now".
        static var connect: String { String(localized: "ui_detail_connect", defaultValue: "Connect now") }
        /// "Rename".
        static var rename: String { String(localized: "ui_detail_rename", defaultValue: "Rename") }
        /// "Rename device".
        static var renameTitle: String {
            String(localized: "ui_detail_rename_title", defaultValue: "Rename device")
        }
        /// Rename dialog body.
        static var renameBody: String {
            String(
                localized: "ui_detail_rename_body",
                defaultValue: "The name is only used on this iPhone. The device cannot change it."
            )
        }
        /// "Last-known addresses".
        static var addresses: String {
            String(localized: "ui_detail_addresses", defaultValue: "Last-known addresses")
        }
        /// "None yet".
        static var noAddresses: String { String(localized: "ui_detail_no_addresses", defaultValue: "None yet") }
        /// "Unpair".
        static var unpair: String { String(localized: "ui_detail_unpair", defaultValue: "Unpair") }
        /// "Unpair Desk?".
        static func unpairTitle(_ label: String) -> String {
            String(localized: "ui_detail_unpair_title", defaultValue: "Unpair \(label)?")
        }
        /// Unpair dialog body.
        static var unpairBody: String {
            String(
                localized: "ui_detail_unpair_body",
                defaultValue: "It will need to pair again before it can send or receive anything."
            )
        }
        /// "Allow this device to".
        static var grantsTitle: String {
            String(localized: "ui_grants_title", defaultValue: "Allow this device to")
        }
        /// Clipboard sent.
        static func copiedTo(_ label: String) -> String {
            String(localized: "toast_copied_to", defaultValue: "Copied to \(label)")
        }
        /// The pasteboard is empty.
        static var nothingToSend: String {
            String(localized: "toast_nothing_to_send", defaultValue: "Nothing to send")
        }
        /// The echo rule skipped the clip.
        static var clipboardUnchanged: String {
            String(localized: "toast_clipboard_unchanged", defaultValue: "Already sent")
        }
        /// No paired devices.
        static var noPairedDevices: String {
            String(localized: "toast_no_paired_devices", defaultValue: "No paired devices. Add one from the Devices tab.")
        }
        /// No device has the clipboard grant.
        static var noClipboardGrant: String {
            String(
                localized: "toast_no_clipboard_grant",
                defaultValue: "No device accepts this iPhone's clipboard. Turn it on in the device settings."
            )
        }
        /// The target is offline.
        static func peerOffline(_ label: String) -> String {
            String(localized: "toast_peer_offline", defaultValue: "\(label) is offline")
        }
    }
}
