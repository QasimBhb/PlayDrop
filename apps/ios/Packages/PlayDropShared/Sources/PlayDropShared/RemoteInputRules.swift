// What the remote input screen offers for a peer, and the keys it sends. A feature the
// computer cannot do is hidden or explained, never shown as a button that does nothing
// (PLAN.md anti-pattern 18): the peer lists what it accepts in `capabilities` while online
// (`input.pointer`, `input.key`, `input.presenter`, `media.control`).
import Foundation

/// Capability names the phone looks for (protocol.md section 4).
public enum PeerCapability {
    /// Pointer moves, buttons and scrolling.
    public static let inputPointer = "input.pointer"
    /// Keys and text.
    public static let inputKey = "input.key"
    /// Presenter remote.
    public static let inputPresenter = "input.presenter"
    /// Media commands.
    public static let mediaControl = "media.control"
}

/// Whether the "Remote input" entry is shown for a peer.
public enum RemoteInputAvailability: Equatable, Sendable {
    /// Online and accepts pointer or key input: show the link.
    case available
    /// Online but accepts no pointer or key input: show a disabled row saying so.
    case unsupported
    /// Offline: capabilities are unknown, show nothing.
    case offline

    /// The entry for a peer that is `online` with `capabilities`.
    public static func of(online: Bool, capabilities: [String]) -> RemoteInputAvailability {
        guard online else { return .offline }
        let caps = Set(capabilities)
        if caps.contains(PeerCapability.inputPointer) || caps.contains(PeerCapability.inputKey) {
            return .available
        }
        return .unsupported
    }
}

/// The tabs of the remote input screen.
public enum RemoteInputTab: String, CaseIterable, Sendable {
    case trackpad, keyboard, presenter

    /// The tabs a peer with `capabilities` accepts, in display order.
    public static func available(for capabilities: [String]) -> [RemoteInputTab] {
        let caps = Set(capabilities)
        return allCases.filter { tab in
            switch tab {
            case .trackpad: return caps.contains(PeerCapability.inputPointer)
            case .keyboard: return caps.contains(PeerCapability.inputKey)
            case .presenter: return caps.contains(PeerCapability.inputPresenter)
            }
        }
    }
}

/// True when the peer is online and accepts media commands.
public func peerAcceptsMediaControl(online: Bool, capabilities: [String]) -> Bool {
    online && capabilities.contains(PeerCapability.mediaControl)
}

/// W3C UI Events `key` values the keyboard tab sends (protocol.md 5.12).
public enum RemoteKey {
    public static let escape = "Escape"
    public static let tab = "Tab"
    public static let enter = "Enter"
    public static let backspace = "Backspace"
    public static let delete = "Delete"
    public static let arrowUp = "ArrowUp"
    public static let arrowDown = "ArrowDown"
    public static let arrowLeft = "ArrowLeft"
    public static let arrowRight = "ArrowRight"
    public static let home = "Home"
    public static let end = "End"
    public static let pageUp = "PageUp"
    public static let pageDown = "PageDown"
    public static let volumeUp = "AudioVolumeUp"
    public static let volumeDown = "AudioVolumeDown"
    public static let volumeMute = "AudioVolumeMute"
    public static let playPause = "MediaPlayPause"

    /// The row of special keys, in display order.
    public static let special: [String] = [
        escape, tab, enter, backspace, arrowLeft, arrowUp, arrowDown, arrowRight,
        home, end, pageUp, pageDown, delete,
    ]

    /// The media keys, in display order.
    public static let media: [String] = [volumeDown, volumeMute, volumeUp, playPause]
}

/// A modifier chip on the keyboard tab.
public enum KeyModifier: String, CaseIterable, Sendable {
    case ctrl, alt, shift, meta
}

/// One frame the keyboard tab sends.
public enum KeyboardOutput: Equatable, Sendable {
    /// Literal text.
    case text(String)
    /// A named key (or one character) pressed with `mods` held.
    case key(String, mods: [KeyModifier])
}

/// The keyboard tab's typing logic: the modifier chips apply to the next key only, and the
/// text field's edits become text or Backspace frames.
///
/// The field always holds `sentinel` plus what was typed since the last edit, so deleting
/// with the field "empty" is still seen (the sentinel goes) and becomes a Backspace. The
/// view puts `fieldReset` back into the field after each edit.
public struct KeyboardComposer: Sendable {
    /// An invisible character that keeps the field non-empty (zero width space).
    public static let sentinel = "\u{200B}"
    /// What the field holds between edits.
    public static let fieldReset = sentinel

    /// Modifiers latched for the next key, in chip order.
    public private(set) var latched: [KeyModifier] = []

    public init() {}

    /// Turns a chip on or off.
    public mutating func toggle(_ mod: KeyModifier) {
        if let i = latched.firstIndex(of: mod) {
            latched.remove(at: i)
        } else {
            latched.append(mod)
            latched.sort { KeyModifier.allCases.firstIndex(of: $0)! < KeyModifier.allCases.firstIndex(of: $1)! }
        }
    }

    /// True when `mod` is latched.
    public func isLatched(_ mod: KeyModifier) -> Bool { latched.contains(mod) }

    /// A special key was tapped: send it with the latched modifiers, then clear them.
    public mutating func press(_ key: String) -> KeyboardOutput {
        let mods = latched
        latched = []
        return .key(key, mods: mods)
    }

    /// The field changed to `new`. Returns the frames to send. Typed text goes out as
    /// `.text`, except while modifiers are latched: then each character is a key with them
    /// (Ctrl+C), and the latch clears after the first.
    public mutating func fieldChanged(to new: String) -> [KeyboardOutput] {
        guard new != Self.fieldReset else { return [] }
        // The sentinel is still there (wherever the cursor was): only text was added. It is
        // gone: Backspace deleted it, or typing replaced it.
        let removed = new.contains(Self.sentinel) ? 0 : 1
        let inserted = new.replacingOccurrences(of: Self.sentinel, with: "")
        var out: [KeyboardOutput] = Array(repeating: .key(RemoteKey.backspace, mods: []), count: removed)
        if removed > 0, !latched.isEmpty {
            // A modified Backspace: the first one carries the latched modifiers.
            out[0] = press(RemoteKey.backspace)
        }
        guard !inserted.isEmpty else { return out }
        if latched.isEmpty {
            out.append(.text(inserted))
        } else {
            var rest = Substring(inserted)
            let first = rest.removeFirst()
            out.append(press(String(first)))
            if !rest.isEmpty { out.append(.text(String(rest))) }
        }
        return out
    }
}
