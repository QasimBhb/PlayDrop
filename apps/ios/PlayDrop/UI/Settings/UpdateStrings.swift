import Foundation

// Strings of the update notice and the "Updates" part of Settings. See `L10n.swift` for the
// scheme. The iPhone never installs anything itself, so the text points at how the app got
// here in the first place.

extension L10n {
    /// Update check.
    enum Update {
        /// Section title.
        static var section: String { String(localized: "ui_update_section", defaultValue: "Updates") }
        /// The switch.
        static var toggle: String {
            String(localized: "ui_update_toggle", defaultValue: "Check for updates when PlayDrop opens")
        }
        /// What the check sends, under the switch.
        static var hint: String {
            String(
                localized: "ui_update_hint",
                defaultValue: "Asks GitHub for the latest version, at most once a day. No name, ID or version is sent. GitHub sees your IP address, as with any download."
            )
        }
        /// The button.
        static var checkNow: String { String(localized: "ui_update_check_now", defaultValue: "Check now") }
        /// "Last checked".
        static var lastChecked: String { String(localized: "ui_update_last_checked", defaultValue: "Last checked") }
        /// No check has got an answer yet.
        static var never: String { String(localized: "ui_update_never", defaultValue: "Never") }
        /// A check is running.
        static var checking: String { String(localized: "ui_update_checking", defaultValue: "Checking...") }
        /// After a manual check: nothing newer.
        static var upToDate: String {
            String(localized: "ui_update_up_to_date", defaultValue: "PlayDrop is up to date.")
        }
        /// After a manual check: a newer version.
        static func available(_ version: String) -> String {
            String(localized: "ui_update_available_line", defaultValue: "PlayDrop \(version) is available.")
        }
        /// After a manual check that did not work.
        static var failed: String {
            String(
                localized: "ui_update_failed",
                defaultValue: "Could not check for updates. Check the connection and try again."
            )
        }
        /// Alert title.
        static func title(_ version: String) -> String {
            String(localized: "ui_update_title", defaultValue: "PlayDrop \(version) is available")
        }
        /// Alert text.
        static var installHint: String {
            String(
                localized: "ui_update_install_hint",
                defaultValue: "Install it the way you installed PlayDrop (SideStore, AltStore or xtool)."
            )
        }
        /// "Open release page".
        static var openPage: String { String(localized: "ui_update_open_page", defaultValue: "Open release page") }
        /// "Later".
        static var later: String { String(localized: "ui_update_later", defaultValue: "Later") }
    }
}
