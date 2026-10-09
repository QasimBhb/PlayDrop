import Foundation

// Strings of the Add device screen, the QR views and the pairing sheet, keyed like Android
// `strings_ui.xml`. See `L10n.swift` for the scheme.

extension L10n {
    /// Add device screen.
    enum Add {
        /// "Nearby".
        static var tabNearby: String { String(localized: "ui_add_tab_nearby", defaultValue: "Nearby") }
        /// "Show QR".
        static var tabShowQr: String { String(localized: "ui_add_tab_show_qr", defaultValue: "Show QR") }
        /// "Scan QR".
        static var tabScanQr: String { String(localized: "ui_add_tab_scan_qr", defaultValue: "Scan QR") }
        /// Intro on the Nearby tab.
        static var intro: String {
            String(
                localized: "ui_add_intro",
                defaultValue: "Choose Add device on the other device too. It shows up below; pick it, then check that both screens show the same six digits."
            )
        }
        /// "Looking for devices, 1:42 left".
        static func looking(_ left: String) -> String {
            String(localized: "ui_add_looking", defaultValue: "Looking for devices, \(left) left")
        }
        /// "Stopped looking.".
        static var windowClosed: String { String(localized: "ui_add_window_closed", defaultValue: "Stopped looking.") }
        /// "Look again".
        static var lookAgain: String { String(localized: "ui_add_look_again", defaultValue: "Look again") }
        /// No candidates yet.
        static var noneYet: String {
            String(
                localized: "ui_add_none_yet",
                defaultValue: "No devices yet. Make sure the other device is on the same Wi-Fi with Add device open."
            )
        }
        /// "Pair".
        static var pair: String { String(localized: "ui_add_pair", defaultValue: "Pair") }
        /// "Or type its address".
        static var typeAddress: String {
            String(localized: "ui_add_type_address", defaultValue: "Or type its address")
        }
        /// Placeholder of the address field.
        static var addressHint: String {
            String(localized: "ui_add_address_hint", defaultValue: "192.168.1.20 or 192.168.1.20:42187")
        }
        /// The typed address is not valid.
        static var badAddress: String {
            String(
                localized: "ui_add_bad_address",
                defaultValue: "That is not an address. Type it like 192.168.1.20 or 192.168.1.20:42187."
            )
        }
        /// "Pairing".
        static var pairing: String { String(localized: "ui_add_pairing", defaultValue: "Pairing") }
        /// "Pairing with Desk".
        static func pairingWith(_ name: String) -> String {
            String(localized: "ui_add_pairing_with", defaultValue: "Pairing with \(name)")
        }
        /// "Paired with Desk".
        static func paired(_ name: String) -> String {
            String(localized: "ui_add_paired", defaultValue: "Paired with \(name)")
        }
        /// "Pairing failed: reason".
        static func pairFailed(_ reason: String) -> String {
            String(localized: "ui_add_pair_failed", defaultValue: "Pairing failed: \(reason)")
        }
        /// Section title for this iPhone's own details.
        static var thisDevice: String {
            String(localized: "ui_add_this_device", defaultValue: "This iPhone")
        }
        /// Explains the own-details section.
        static var thisDeviceHint: String {
            String(
                localized: "ui_add_this_device_hint",
                defaultValue: "If the other device does not show this iPhone, type this address there."
            )
        }
        /// Own identity not known yet.
        static var identityUnknown: String {
            String(
                localized: "ui_settings_identity_unknown",
                defaultValue: "Start PlayDrop to see this iPhone's ID."
            )
        }
    }

    /// QR views (SR-9).
    enum Qr {
        /// Intro of Show QR.
        static var showIntro: String {
            String(localized: "ui_qr_show_intro", defaultValue: "Scan this code with the other device.")
        }
        /// VoiceOver label of the code.
        static var contentDescription: String {
            String(localized: "ui_qr_content_description", defaultValue: "Pairing QR code")
        }
        /// "Valid for 1:42".
        static func validFor(_ left: String) -> String {
            String(localized: "ui_qr_valid_for", defaultValue: "Valid for \(left)")
        }
        /// The secrecy warning.
        static var warning: String {
            String(
                localized: "ui_qr_warning",
                defaultValue: "Works once, for two minutes. Anyone who sees this code can pair, so do not share it. Screenshots and screen recordings leave it out."
            )
        }
        /// "The code expired.".
        static var expired: String { String(localized: "ui_qr_expired", defaultValue: "The code expired.") }
        /// "Show a new code".
        static var newCode: String { String(localized: "ui_qr_new", defaultValue: "Show a new code") }
        /// Intro of Scan QR.
        static var scanIntro: String {
            String(localized: "ui_qr_scan_intro", defaultValue: "Point the camera at the code the other device shows.")
        }
        /// Before asking for the camera.
        static var cameraNeeded: String {
            String(
                localized: "ui_qr_camera_needed",
                defaultValue: "Scanning needs the camera. PlayDrop uses it only on this screen."
            )
        }
        /// Camera denied.
        static var cameraDenied: String {
            String(
                localized: "ui_qr_camera_denied",
                defaultValue: "Camera access is off. Turn it on in Settings > PlayDrop > Camera, or use Nearby or Show QR instead."
            )
        }
        /// Camera restricted by Screen Time or a profile.
        static var cameraRestricted: String {
            String(
                localized: "ui_qr_camera_restricted",
                defaultValue: "The camera is restricted on this iPhone. Use Nearby or Show QR instead."
            )
        }
        /// "Allow camera".
        static var allowCamera: String { String(localized: "ui_qr_allow_camera", defaultValue: "Allow camera") }
        /// "Open Settings".
        static var openSettings: String { String(localized: "ui_open_settings", defaultValue: "Open Settings") }
        /// The camera could not start.
        static var cameraFailed: String {
            String(localized: "ui_add_camera_failed", defaultValue: "The camera could not start.")
        }
        /// A QR code that is not a PlayDrop pairing code.
        static var notPairingCode: String {
            String(localized: "ui_qr_not_pairing", defaultValue: "That is not a PlayDrop pairing code.")
        }
    }

    /// Pairing sheet (SR-8).
    enum Pair {
        /// "Pair with Desk?".
        static func sasTitle(_ name: String) -> String {
            String(localized: "ui_pair_sas_title", defaultValue: "Pair with \(name)?")
        }
        /// SAS explanation.
        static var sasBody: String {
            String(
                localized: "ui_pair_sas_body",
                defaultValue: "Accept only if the other screen shows the same six digits."
            )
        }
        /// "Add Desk (K7QX-M2PA)?".
        static func confirmTitle(_ name: String, _ shortId: String) -> String {
            String(localized: "ui_pair_confirm_title", defaultValue: "Add \(name) (\(shortId))?")
        }
        /// Scanner-confirm explanation.
        static var confirmBody: String {
            String(localized: "ui_pair_confirm_body", defaultValue: "This device scanned the code on this iPhone.")
        }
        /// "Name: Desk".
        static func name(_ name: String) -> String {
            String(localized: "ui_pair_name", defaultValue: "Name: \(name)")
        }
        /// "Pair".
        static var accept: String { String(localized: "ui_pair_accept", defaultValue: "Pair") }
        /// "Reject".
        static var reject: String { String(localized: "ui_pair_reject", defaultValue: "Reject") }
        /// "Add".
        static var add: String { String(localized: "ui_pair_add", defaultValue: "Add") }
        /// The 30 s limit.
        static var timeoutHint: String {
            String(
                localized: "ui_pair_timeout_hint",
                defaultValue: "Answer within 30 seconds, or pairing stops and you start again."
            )
        }
        /// VoiceOver reading of the SAS, digit by digit.
        static func sasAccessibility(_ sas: String) -> String {
            let spelled = sas.filter(\.isNumber).map(String.init).joined(separator: " ")
            return String(localized: "ui_pair_sas_a11y", defaultValue: "Code \(spelled)")
        }
    }
}
