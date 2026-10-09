import Foundation

// Every user-visible string of the app's screens, keyed like the Android resources
// (`apps/android/app/src/main/res/values/strings_ui.xml`) so a strings catalog can be added
// later without touching a view. Each entry is `String(localized:defaultValue:)`: the key is
// stable, the English text is the default value, and interpolated arguments become format
// specifiers in the catalog. Screen-specific strings live next to their screens as
// extensions of `L10n`; this file holds the shared ones and the error texts.
//
// Plurals: English needs only "one" and "other", so plural entries use two keys
// (`<key>_one`, `<key>_other`) chosen by `L10n.plural`. A catalog can fold them into one
// key with plural variations later.

/// Namespace for the localized strings of the UI.
enum L10n {
    /// Picks the singular or plural text for `n`.
    static func plural(_ n: Int, one: @autoclosure () -> String, other: @autoclosure () -> String) -> String {
        n == 1 ? one() : other()
    }

    /// "Back".
    static var back: String { String(localized: "ui_back", defaultValue: "Back") }
    /// "Cancel".
    static var cancel: String { String(localized: "ui_cancel", defaultValue: "Cancel") }
    /// "Save".
    static var save: String { String(localized: "ui_save", defaultValue: "Save") }
    /// "Start".
    static var start: String { String(localized: "ui_start", defaultValue: "Start") }
    /// "Next".
    static var next: String { String(localized: "ui_next", defaultValue: "Next") }
    /// "Allow".
    static var allow: String { String(localized: "ui_allow", defaultValue: "Allow") }
    /// "Not now".
    static var notNow: String { String(localized: "ui_not_now", defaultValue: "Not now") }
    /// "Refresh".
    static var refresh: String { String(localized: "ui_refresh", defaultValue: "Refresh") }
    /// "Done".
    static var done: String { String(localized: "ui_done", defaultValue: "Done") }
    /// "Try again".
    static var tryAgain: String { String(localized: "ui_try_again", defaultValue: "Try again") }
    /// "yes".
    static var yes: String { String(localized: "ui_yes", defaultValue: "yes") }
    /// "no".
    static var no: String { String(localized: "ui_no", defaultValue: "no") }
    /// "42%".
    static func percent(_ value: Int) -> String {
        String(localized: "ui_percent", defaultValue: "\(value)%")
    }
    /// Banner shown while the core is down.
    static var notRunningBanner: String {
        String(
            localized: "ui_service_not_running",
            defaultValue: "PlayDrop is not running. Devices cannot reach this iPhone."
        )
    }

    /// Tab titles.
    enum Tab {
        /// "Devices".
        static var devices: String { String(localized: "ui_tab_devices", defaultValue: "Devices") }
        /// "Transfers".
        static var transfers: String { String(localized: "ui_tab_transfers", defaultValue: "Transfers") }
        /// "Settings".
        static var settings: String { String(localized: "ui_tab_settings", defaultValue: "Settings") }
    }

    /// Relative times.
    enum Ago {
        /// "just now".
        static var justNow: String { String(localized: "ui_ago_just_now", defaultValue: "just now") }
        /// "never".
        static var never: String { String(localized: "ui_ago_never", defaultValue: "never") }
        /// "5 min ago".
        static func minutes(_ n: Int) -> String {
            String(localized: "ui_ago_minutes", defaultValue: "\(n) min ago")
        }
        /// "1 hour ago", "3 hours ago".
        static func hours(_ n: Int) -> String {
            L10n.plural(
                n,
                one: String(localized: "ui_ago_hours_one", defaultValue: "\(n) hour ago"),
                other: String(localized: "ui_ago_hours_other", defaultValue: "\(n) hours ago")
            )
        }
        /// "1 day ago", "3 days ago".
        static func days(_ n: Int) -> String {
            L10n.plural(
                n,
                one: String(localized: "ui_ago_days_one", defaultValue: "\(n) day ago"),
                other: String(localized: "ui_ago_days_other", defaultValue: "\(n) days ago")
            )
        }
    }

    /// Error texts (anti-pattern 18: every failure is said in words). Details from the core
    /// are plain reasons; no SAS, QR secret or clipboard text ever reaches an error (SR-32).
    enum Err {
        /// `NotPaired`.
        static var notPaired: String {
            String(localized: "ui_err_not_paired", defaultValue: "That device is not paired with this iPhone.")
        }
        /// `Forbidden`.
        static var forbidden: String {
            String(localized: "ui_err_forbidden", defaultValue: "The other device does not allow that.")
        }
        /// `Locked`.
        static var locked: String {
            String(
                localized: "ui_err_locked",
                defaultValue: "Not possible while the other device is locked. Unlock it and try again."
            )
        }
        /// `NotFound`.
        static var notFound: String {
            String(localized: "ui_err_not_found", defaultValue: "The device is not connected right now.")
        }
        /// `TooLarge`.
        static var tooLarge: String {
            String(localized: "ui_err_too_large", defaultValue: "That is too large to send this way.")
        }
        /// `PortHeld`.
        static func portHeld(_ port: UInt16) -> String {
            String(
                localized: "ui_err_port_held",
                defaultValue: "Port \(Int(port)) is used by another app. Pick another port in Settings."
            )
        }
        /// `InsecurePermissions`.
        static var insecure: String {
            String(localized: "ui_err_insecure", defaultValue: "PlayDrop's private files have unsafe permissions.")
        }
        /// `NoSpace`.
        static var noSpace: String {
            String(localized: "ui_err_no_space", defaultValue: "Not enough free space.")
        }
        /// `PathInvalid`.
        static var pathInvalid: String {
            String(localized: "ui_err_path_invalid", defaultValue: "That file cannot be sent.")
        }
        /// `PairRejected`.
        static var pairRejected: String {
            String(localized: "ui_err_pair_rejected", defaultValue: "Pairing was declined.")
        }
        /// `Io`.
        static func io(_ detail: String) -> String {
            String(localized: "ui_err_io", defaultValue: "Network or file error: \(detail)")
        }
        /// `Other` and anything unexpected.
        static func other(_ detail: String) -> String {
            String(localized: "ui_err_other", defaultValue: "Something went wrong: \(detail)")
        }
        /// `CoreHostError.notRunning`.
        static var notRunning: String {
            String(
                localized: "ui_err_not_running",
                defaultValue: "PlayDrop is not running. Close and reopen the app, or check Settings > Diagnostics."
            )
        }
        /// `CoreHostError.startFailed`.
        static func startFailed(_ detail: String) -> String {
            String(localized: "ui_err_start_failed", defaultValue: "PlayDrop could not start: \(detail)")
        }
    }
}

extension L10n {
    /// Strings shared by several screens.
    enum Common {
        /// VoiceOver text for a Short ID; spelled out character by character.
        static func shortIdAccessibility(_ shortId: String) -> String {
            let spelled = shortId.map(String.init).joined(separator: " ")
            return String(localized: "ui_short_id_a11y", defaultValue: "Short ID \(spelled)")
        }
        /// "Short ID".
        static var shortId: String { String(localized: "ui_settings_short_id", defaultValue: "Short ID") }
        /// "Default device".
        static var defaultDevice: String { String(localized: "ui_default_device", defaultValue: "Default device") }
        /// "online".
        static var online: String { String(localized: "ui_device_online", defaultValue: "online") }
        /// "offline".
        static var offline: String { String(localized: "ui_diag_offline", defaultValue: "offline") }
        /// "asleep".
        static var asleep: String { String(localized: "ui_device_asleep", defaultValue: "asleep") }
        /// "Battery 42%", "Battery 42%, charging".
        static func battery(_ level: Int, charging: Bool) -> String {
            charging
                ? String(localized: "ui_battery_charging_a11y", defaultValue: "Battery \(level)%, charging")
                : String(localized: "ui_battery_a11y", defaultValue: "Battery \(level)%")
        }
    }
}
