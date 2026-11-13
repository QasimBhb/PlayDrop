import Foundation
import PlayDropShared

// Strings of the remote input screen and the "Now playing" rows. See `L10n.swift` for the
// scheme; keys follow the Android `strings_ui.xml` naming.

extension L10n {
    /// Remote input screen.
    enum Input {
        /// "Remote input".
        static var title: String { String(localized: "ui_input_title", defaultValue: "Remote input") }
        /// Disabled row for a computer without remote input.
        static var unsupported: String {
            String(localized: "ui_input_unsupported", defaultValue: "This device cannot be controlled remotely")
        }
        /// Why the session is over, as a sentence that stays until the user goes back. `label` is
        /// the computer's name. The first two tell what to do on the computer, as the user
        /// reported the screen closing with no reason.
        static func endNotice(_ notice: InputEndNotice, label: String) -> String {
            switch notice {
            case .grantOff:
                return String(
                    localized: "ui_input_end_grant_off",
                    defaultValue: "Remote control is turned off for this iPhone on \(label). Turn it on for this iPhone in PlayDrop on \(label)."
                )
            case .locked:
                return String(
                    localized: "ui_input_end_locked",
                    defaultValue: "\(label) is locked. Unlock it and try again."
                )
            case .notAccepted:
                return String(
                    localized: "ui_input_end_not_accepted",
                    defaultValue: "\(label) cannot be controlled from this iPhone. It does not accept remote input."
                )
            case .stoppedByUser:
                return String(
                    localized: "ui_input_end_stopped",
                    defaultValue: "Remote input was stopped on \(label). Open this screen again to start it."
                )
            case .idleTimeout:
                return String(
                    localized: "ui_input_end_idle",
                    defaultValue: "Remote input ended after 10 minutes without use. Open this screen again to start it."
                )
            case .other(let reason?):
                return String(
                    localized: "ui_input_end_other_reason",
                    defaultValue: "\(label) ended remote input: \(reason)"
                )
            case .other(nil):
                return String(
                    localized: "ui_input_end_other",
                    defaultValue: "\(label) ended remote input. Open this screen again to try again."
                )
            }
        }
        /// "Remote input ended".
        static var endedTitle: String { String(localized: "ui_input_ended_title", defaultValue: "Remote input ended") }
        /// Shown while the session is not open (connecting, or the device went offline).
        static var connecting: String {
            String(localized: "ui_input_connecting", defaultValue: "Connecting to the device...")
        }
        /// "Trackpad".
        static var trackpad: String { String(localized: "ui_input_trackpad", defaultValue: "Trackpad") }
        /// "Keyboard".
        static var keyboard: String { String(localized: "ui_input_keyboard", defaultValue: "Keyboard") }
        /// "Presenter".
        static var presenter: String { String(localized: "ui_input_presenter", defaultValue: "Presenter") }
        /// Trackpad hint under the surface.
        static var trackpadHint: String {
            String(
                localized: "ui_input_trackpad_hint",
                defaultValue: "Tap to click, two fingers to right-click or scroll, hold to drag."
            )
        }
        /// "Pointer speed".
        static var sensitivity: String {
            String(localized: "ui_input_sensitivity", defaultValue: "Pointer speed")
        }
        /// "Options".
        static var options: String { String(localized: "ui_input_options", defaultValue: "Options") }
        /// "Left".
        static var left: String { String(localized: "ui_input_left", defaultValue: "Left") }
        /// "Middle".
        static var middle: String { String(localized: "ui_input_middle", defaultValue: "Middle") }
        /// "Right".
        static var right: String { String(localized: "ui_input_right", defaultValue: "Right") }
        /// Placeholder of the typing field.
        static var typeHere: String {
            String(localized: "ui_input_type_here", defaultValue: "Tap here and type")
        }
        /// "Previous".
        static var previous: String { String(localized: "ui_input_previous", defaultValue: "Previous") }
        /// "Next".
        static var next: String { String(localized: "ui_input_next", defaultValue: "Next") }
        /// "Blackout".
        static var blackout: String { String(localized: "ui_input_blackout", defaultValue: "Blackout") }
        /// "Laser pointer".
        static var pointer: String { String(localized: "ui_input_pointer", defaultValue: "Laser pointer") }
        /// "Volume down".
        static var volumeDown: String { String(localized: "ui_input_volume_down", defaultValue: "Volume down") }
        /// "Volume up".
        static var volumeUp: String { String(localized: "ui_input_volume_up", defaultValue: "Volume up") }
        /// "Mute".
        static var mute: String { String(localized: "ui_input_mute", defaultValue: "Mute") }
        /// "Play or pause".
        static var playPause: String { String(localized: "ui_input_play_pause", defaultValue: "Play or pause") }

        /// The label of a special key in the keyboard row (the W3C name stays on the wire).
        static func keyLabel(_ key: String) -> String {
            switch key {
            case "Escape": return String(localized: "ui_key_escape", defaultValue: "Esc")
            case "Tab": return String(localized: "ui_key_tab", defaultValue: "Tab")
            case "Enter": return String(localized: "ui_key_enter", defaultValue: "Enter")
            case "Backspace": return String(localized: "ui_key_backspace", defaultValue: "Bksp")
            case "Delete": return String(localized: "ui_key_delete", defaultValue: "Del")
            case "Home": return String(localized: "ui_key_home", defaultValue: "Home")
            case "End": return String(localized: "ui_key_end", defaultValue: "End")
            case "PageUp": return String(localized: "ui_key_page_up", defaultValue: "PgUp")
            case "PageDown": return String(localized: "ui_key_page_down", defaultValue: "PgDn")
            case "ArrowUp": return "\u{2191}"
            case "ArrowDown": return "\u{2193}"
            case "ArrowLeft": return "\u{2190}"
            case "ArrowRight": return "\u{2192}"
            default: return key
            }
        }

        /// The label of a modifier chip.
        static func modifierLabel(_ name: String) -> String {
            switch name {
            case "ctrl": return String(localized: "ui_key_ctrl", defaultValue: "Ctrl")
            case "alt": return String(localized: "ui_key_alt", defaultValue: "Alt")
            case "shift": return String(localized: "ui_key_shift", defaultValue: "Shift")
            default: return String(localized: "ui_key_meta", defaultValue: "Meta")
            }
        }
    }

    /// "Now playing" rows on the Device screen.
    enum Media {
        /// "Now playing".
        static var title: String { String(localized: "ui_media_title", defaultValue: "Now playing") }
        /// A player that reported no title.
        static var unknownTitle: String {
            String(localized: "ui_media_unknown_title", defaultValue: "Nothing playing")
        }
        /// "Play".
        static var play: String { String(localized: "ui_media_play", defaultValue: "Play") }
        /// "Pause".
        static var pause: String { String(localized: "ui_media_pause", defaultValue: "Pause") }
        /// "Previous track".
        static var previous: String { String(localized: "ui_media_previous", defaultValue: "Previous track") }
        /// "Next track".
        static var next: String { String(localized: "ui_media_next", defaultValue: "Next track") }
        /// "Volume".
        static var volume: String { String(localized: "ui_media_volume", defaultValue: "Volume") }
        /// "Position".
        static var position: String { String(localized: "ui_media_position", defaultValue: "Position") }
        /// "Computer volume": the computer's master volume slider.
        static var computerVolume: String {
            String(localized: "ui_media_computer_volume", defaultValue: "Computer volume")
        }
        /// Accessibility hint of the computer volume slider.
        static var computerVolumeHint: String {
            String(
                localized: "ui_media_computer_volume_hint",
                defaultValue: "Sets the volume of everything the computer plays."
            )
        }
    }
}
