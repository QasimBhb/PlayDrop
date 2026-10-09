import Foundation

// Strings of Settings, Diagnostics and the Local Network fix-it screen, keyed like Android
// `strings_ui.xml` where Android has the same text. See `L10n.swift` for the scheme.

extension L10n {
    /// Settings tab.
    enum Settings {
        /// "Settings".
        static var title: String { String(localized: "ui_settings_title", defaultValue: "Settings") }
        /// "This iPhone's name".
        static var nameSection: String { String(localized: "ui_settings_name", defaultValue: "This iPhone's name") }
        /// "Name".
        static var name: String { String(localized: "ui_settings_name_label", defaultValue: "Name") }
        /// "Address".
        static var address: String { String(localized: "ui_settings_address", defaultValue: "Address") }
        /// Name hint.
        static var nameHint: String {
            String(
                localized: "ui_settings_name_hint",
                defaultValue: "Other devices see this name. A change applies the next time PlayDrop opens."
            )
        }
        /// Empty name.
        static var nameEmpty: String { String(localized: "ui_settings_name_empty", defaultValue: "The name cannot be empty.") }
        /// Saved, needs restart.
        static var savedRestart: String {
            String(localized: "ui_settings_saved_restart", defaultValue: "Saved. Applies the next time PlayDrop opens.")
        }
        /// "Default device".
        static var defaultDevice: String { String(localized: "ui_settings_default", defaultValue: "Default device") }
        /// "None chosen".
        static var defaultNone: String { String(localized: "ui_settings_default_none", defaultValue: "None chosen") }
        /// Default hint.
        static var defaultHint: String {
            String(
                localized: "ui_settings_default_hint",
                defaultValue: "Send files and the share sheet go to this device with one tap."
            )
        }
        /// "Received files".
        static var receiveSection: String {
            String(localized: "ui_settings_receive", defaultValue: "Received files")
        }
        /// Where received files go and when receiving works.
        static var receiveHint: String {
            String(
                localized: "ui_settings_receive_hint",
                defaultValue: "Saved in Files > On My iPhone > PlayDrop. This iPhone receives while PlayDrop is open; files your computer or Android phone queue for it arrive when you open the app."
            )
        }
        /// "Clipboard".
        static var clipboard: String { String(localized: "ui_settings_clipboard", defaultValue: "Clipboard") }
        /// Apply incoming clipboard toggle.
        static var applyClipboard: String {
            String(localized: "ui_settings_apply_clipboard", defaultValue: "Put received text on the clipboard")
        }
        /// Apply incoming clipboard hint.
        static var applyClipboardHint: String {
            String(
                localized: "ui_settings_apply_clipboard_hint",
                defaultValue: "While PlayDrop is open, text copied on a device that may use this iPhone's clipboard is ready to paste here. iOS does not let apps read the clipboard in the background, so use Send clipboard on a device to send yours."
            )
        }
        /// Pull outbox toggle.
        static var pullOutbox: String {
            String(localized: "ui_settings_pull_outbox", defaultValue: "Fetch queued files when opened")
        }
        /// Pull outbox hint.
        static var pullOutboxHint: String {
            String(
                localized: "ui_settings_pull_outbox_hint",
                defaultValue: "Your computers and Android phones keep files for this iPhone while it is away. PlayDrop collects them as soon as you open it."
            )
        }
        /// "Notifications".
        static var notifications: String { String(localized: "ui_settings_notifications", defaultValue: "Notifications") }
        /// Notifications allowed.
        static var notificationsOn: String {
            String(
                localized: "ui_settings_notifications_on",
                defaultValue: "Allowed: received files, offers and pairing show up."
            )
        }
        /// Notifications off.
        static var notificationsOff: String {
            String(
                localized: "ui_settings_notifications_off",
                defaultValue: "Off: you will not see received files or incoming offers."
            )
        }
        /// "Network".
        static var network: String { String(localized: "ui_settings_network", defaultValue: "Network") }
        /// "Port".
        static var port: String { String(localized: "ui_settings_port", defaultValue: "Port") }
        /// Port hint.
        static var portHint: String {
            String(
                localized: "ui_settings_port_hint",
                defaultValue: "42187 unless something else uses it. A change applies the next time PlayDrop opens."
            )
        }
        /// Port invalid.
        static var portInvalid: String {
            String(localized: "ui_settings_port_invalid", defaultValue: "Use a port between 1024 and 65535.")
        }
        /// Port reserved.
        static var portReserved: String {
            String(
                localized: "ui_settings_port_reserved",
                defaultValue: "That port belongs to KDE Connect or LocalSend. Pick another one."
            )
        }
        /// Diagnostics row hint.
        static var diagnosticsHint: String {
            String(localized: "ui_settings_diagnostics_hint", defaultValue: "Check why a device cannot be found")
        }
        /// "About".
        static var about: String { String(localized: "ui_settings_about", defaultValue: "About") }
        /// "Device ID".
        static var deviceId: String { String(localized: "ui_settings_device_id", defaultValue: "Device ID") }
        /// "App version".
        static var appVersion: String { String(localized: "ui_settings_app_version", defaultValue: "App version") }
        /// "PlayDrop core".
        static var coreVersion: String { String(localized: "ui_settings_core_version_label", defaultValue: "PlayDrop core") }
        /// Licence.
        static var licence: String {
            String(
                localized: "ui_settings_licence",
                defaultValue: "Free software under the GNU GPL, version 3 or later (GPL-3.0-or-later)."
            )
        }
        /// "Source code".
        static var sourceCode: String { String(localized: "ui_settings_source", defaultValue: "Source code") }
    }

    /// Diagnostics screen.
    enum Diag {
        /// "Diagnostics".
        static var title: String { String(localized: "ui_diagnostics_title", defaultValue: "Diagnostics") }
        /// "Checking".
        static var running: String { String(localized: "ui_diag_running", defaultValue: "Checking") }
        /// No report.
        static var unavailable: String {
            String(localized: "ui_diag_unavailable", defaultValue: "No report. Start PlayDrop, then refresh.")
        }
        /// "Summary".
        static var summary: String { String(localized: "ui_diag_summary", defaultValue: "Summary") }
        /// "OK".
        static var ok: String { String(localized: "ui_diag_ok", defaultValue: "OK") }
        /// "Problem".
        static var problem: String { String(localized: "ui_diag_problem", defaultValue: "Problem") }
        /// "Listening".
        static var listener: String { String(localized: "ui_diag_listener", defaultValue: "Listening") }
        /// "On 192.168.1.5:42187".
        static func listeningOn(_ addr: String) -> String {
            String(localized: "ui_diag_listening_on", defaultValue: "On \(addr)")
        }
        /// "Not listening on port 42187".
        static func notListening(_ port: UInt16) -> String {
            String(localized: "ui_diag_not_listening", defaultValue: "Not listening on port \(Int(port))")
        }
        /// "Network interfaces".
        static var interfaces: String { String(localized: "ui_diag_interfaces", defaultValue: "Network interfaces") }
        /// No interface.
        static var noInterface: String {
            String(localized: "ui_diag_no_interface", defaultValue: "No usable network interface")
        }
        /// One interface.
        static func interfaceLine(_ addr: String, _ mask: String, _ index: UInt32) -> String {
            String(localized: "ui_diag_interface_line", defaultValue: "\(addr) mask \(mask) (interface \(Int(index)))")
        }
        /// "Multicast beacon".
        static var multicast: String { String(localized: "ui_diag_multicast", defaultValue: "Multicast beacon") }
        /// "Turned off".
        static var off: String { String(localized: "ui_diag_off", defaultValue: "Turned off") }
        /// "Joined on ...".
        static func joinedOn(_ addr: String) -> String {
            String(localized: "ui_diag_joined_on", defaultValue: "Joined on \(addr)")
        }
        /// "Join failed on ...".
        static func joinFailed(_ addr: String) -> String {
            String(localized: "ui_diag_join_failed", defaultValue: "Join failed on \(addr)")
        }
        /// Beacon heard.
        static func beaconAgo(_ ago: String) -> String {
            String(localized: "ui_diag_beacon_ago", defaultValue: "Beacon from another device \(ago) ago")
        }
        /// No beacon.
        static var noBeacon: String {
            String(localized: "ui_diag_no_beacon", defaultValue: "No beacon from another device since start")
        }
        /// "mDNS".
        static var mdns: String { String(localized: "ui_diag_mdns", defaultValue: "mDNS") }
        /// Registered.
        static var mdnsRegistered: String {
            String(localized: "ui_diag_mdns_registered", defaultValue: "This iPhone is registered")
        }
        /// Not registered.
        static var mdnsNotRegistered: String {
            String(localized: "ui_diag_mdns_not_registered", defaultValue: "This iPhone is not registered")
        }
        /// "Error: ...".
        static func error(_ text: String) -> String {
            String(localized: "ui_diag_error", defaultValue: "Error: \(text)")
        }
        /// Advert heard.
        static func advertAgo(_ ago: String) -> String {
            String(localized: "ui_diag_advert_ago", defaultValue: "Advert from another device \(ago) ago")
        }
        /// No advert.
        static var noAdvert: String {
            String(localized: "ui_diag_no_advert", defaultValue: "No advert from another device since start")
        }
        /// "Paired devices".
        static var peers: String { String(localized: "ui_diag_peers", defaultValue: "Paired devices") }
        /// One peer line.
        static func peerLine(_ label: String, _ shortId: String, _ state: String, _ mdns: String, _ beacon: String) -> String {
            String(
                localized: "ui_diag_peer_line",
                defaultValue: "\(label) (\(shortId)): \(state); seen via mDNS \(mdns), beacon \(beacon)"
            )
        }
        /// Last failure of a peer.
        static func peerFailure(_ path: String, _ failure: String) -> String {
            String(localized: "ui_diag_peer_failure", defaultValue: "Last failure via \(path): \(failure)")
        }
        /// "Raw report".
        static var raw: String { String(localized: "ui_diag_raw", defaultValue: "Raw report") }
        /// Path names.
        static var pathMdns: String { String(localized: "ui_path_mdns", defaultValue: "mDNS") }
        /// Path names.
        static var pathBeacon: String { String(localized: "ui_path_beacon", defaultValue: "multicast beacon") }
        /// Path names.
        static var pathLastKnown: String { String(localized: "ui_path_last_known", defaultValue: "last-known address") }

        /// Issue texts, port of Android `issueText`.
        static var allFine: String { String(localized: "ui_issue_all_fine", defaultValue: "All paths fine") }
        /// Not listening.
        static func issueNotListening(_ port: UInt16) -> String {
            String(
                localized: "ui_issue_not_listening",
                defaultValue: "PlayDrop is not listening on port \(Int(port)): port blocked or used by another app. Reopen PlayDrop or change the port."
            )
        }
        /// No interface.
        static var issueNoInterface: String {
            String(localized: "ui_issue_no_interface", defaultValue: "No network interface is up. Turn on Wi-Fi.")
        }
        /// Multicast join failed.
        static func issueMulticastJoin(_ addr: String, _ error: String) -> String {
            String(localized: "ui_issue_multicast_join", defaultValue: "Multicast blocked on \(addr): \(error)")
        }
        /// Multicast blocked.
        static func issueMulticastBlocked(_ iface: String, _ peer: String) -> String {
            String(
                localized: "ui_issue_multicast_blocked",
                defaultValue: "Multicast blocked on \(iface): no beacon heard although \(peer) is connected on this network"
            )
        }
        /// Discovery off.
        static var issueDiscoveryOff: String {
            String(
                localized: "ui_issue_discovery_off",
                defaultValue: "Discovery is off: devices are found only at their last-known address"
            )
        }
        /// mDNS failed.
        static func issueMdns(_ error: String) -> String {
            String(localized: "ui_issue_mdns", defaultValue: "mDNS failed: \(error)")
        }
        /// Other network.
        static func issueOtherNetwork(_ peer: String) -> String {
            String(localized: "ui_issue_other_network", defaultValue: "No interface on the same network as \(peer)")
        }
        /// Peer not reached.
        static func issuePeer(_ peer: String, _ path: String, _ failure: String) -> String {
            String(localized: "ui_issue_peer", defaultValue: "\(peer) not reached: \(path) failed (\(failure))")
        }
    }

    /// Local Network fix-it screen.
    enum LocalNetwork {
        /// Title.
        static var title: String {
            String(localized: "ui_lan_title", defaultValue: "Local Network access")
        }
        /// What happened.
        static var whatHappened: String {
            String(
                localized: "ui_lan_what",
                defaultValue: "PlayDrop cannot reach devices on your Wi-Fi. Most often this is because Local Network access is off for PlayDrop. iOS asks once; if the answer was Don't Allow, PlayDrop can neither find your devices nor be found."
            )
        }
        /// "How to turn it on".
        static var howTo: String { String(localized: "ui_lan_how", defaultValue: "How to turn it on") }
        /// Settings path.
        static var path: String {
            String(
                localized: "ui_lan_path",
                defaultValue: "Settings > Privacy & Security > Local Network > PlayDrop"
            )
        }
        /// After turning it on.
        static var after: String {
            String(
                localized: "ui_lan_after",
                defaultValue: "Turn the switch on, come back here and tap Try again."
            )
        }
        /// "Open Settings".
        static var openSettings: String { String(localized: "ui_open_settings", defaultValue: "Open Settings") }
        /// "What PlayDrop saw".
        static var detail: String { String(localized: "ui_lan_detail", defaultValue: "What PlayDrop saw") }
        /// Running again.
        static var nowRunning: String {
            String(localized: "ui_lan_running", defaultValue: "PlayDrop is running.")
        }
        /// "Fix Local Network access".
        static var entry: String {
            String(localized: "ui_lan_entry", defaultValue: "Fix Local Network access")
        }
    }
}
