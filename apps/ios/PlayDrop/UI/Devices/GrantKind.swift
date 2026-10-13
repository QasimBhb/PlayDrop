import Foundation
import PlayDropCore

// The per-device permissions a user can change on the Device screen (threat model SR-14).
// The `input` grant would let the device control this iPhone, which iOS does not allow any
// app to do, and media control is not offered on iOS (no system API lets an app control
// another app's playback), so both are hidden instead of shown as switches that do nothing
// (anti-pattern 18); the grants section says so in a footer (`L10n.Detail.inputNotPossible`).
// Controlling the computer from this iPhone needs the grant on the computer, not here.
// Port of Android `GrantKind` and `GrantEdit`.

/// One permission row.
enum GrantKind: CaseIterable, Identifiable, Sendable {
    /// Receive files without asking.
    case files
    /// Let the device put text on this iPhone's clipboard.
    case clipboardIn
    /// Send this iPhone's clipboard to the device.
    case clipboardOut
    /// Let the device ring this iPhone.
    case find

    /// Stable identity for `ForEach`.
    var id: Self { self }

    /// The row title.
    var title: String {
        switch self {
        case .files: return String(localized: "ui_grant_files", defaultValue: "Send files without asking")
        case .clipboardIn:
            return String(localized: "ui_grant_clip_in", defaultValue: "Put text on this iPhone's clipboard")
        case .clipboardOut:
            return String(localized: "ui_grant_clip_out", defaultValue: "Receive this iPhone's clipboard")
        case .find: return String(localized: "ui_grant_find", defaultValue: "Ring this iPhone")
        }
    }

    /// The explanation under the title.
    var detail: String {
        switch self {
        case .files:
            return String(
                localized: "ui_grant_files_body",
                defaultValue: "When off, each transfer waits for you to accept it."
            )
        case .clipboardIn:
            return String(
                localized: "ui_grant_clip_in_body",
                defaultValue: "Text copied on the device appears here while PlayDrop is open, ready to paste."
            )
        case .clipboardOut:
            return String(
                localized: "ui_grant_clip_out_body",
                defaultValue: "Text you send with Send clipboard goes to the device."
            )
        case .find:
            return String(
                localized: "ui_grant_find_body",
                defaultValue: "Plays a sound while PlayDrop is open, so you can find this iPhone."
            )
        }
    }

    /// The current value in `grants`.
    func value(in grants: Grants) -> Bool {
        switch self {
        case .files: return grants.files
        case .clipboardIn: return grants.clipboardIn
        case .clipboardOut: return grants.clipboardOut
        case .find: return grants.find
        }
    }

    /// A copy of `grants` with this permission set to `value`. Hidden grants keep their value.
    func applying(_ value: Bool, to grants: Grants) -> Grants {
        var g = grants
        switch self {
        case .files: g.files = value
        case .clipboardIn: g.clipboardIn = value
        case .clipboardOut: g.clipboardOut = value
        case .find: g.find = value
        }
        return g
    }
}
