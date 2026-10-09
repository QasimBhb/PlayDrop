import Foundation

// Checks a typed port. Ports below 1024 need privileges, and the KDE Connect (1714 to 1764)
// and LocalSend (53317) ports are off limits so PlayDrop never collides with them (CLAUDE.md
// hard rules). Port of Android `PortRules`; plain logic for unit tests.

/// Validation of the port setting.
enum PortRules {
    /// Outcome of `check`.
    enum Result: Equatable {
        /// Usable.
        case ok(UInt16)
        /// Not a number in 1024...65535.
        case invalid
        /// Belongs to KDE Connect or LocalSend.
        case reserved
    }

    /// Validates `text`.
    static func check(_ text: String) -> Result {
        let t = text.trimmingCharacters(in: .whitespaces)
        guard !t.isEmpty, t.count <= 5, t.allSatisfy(\.isASCII), t.allSatisfy(\.isNumber), let n = Int(t) else {
            return .invalid
        }
        guard (1024...65535).contains(n) else { return .invalid }
        if (1714...1764).contains(n) || n == 53317 { return .reserved }
        return .ok(UInt16(n))
    }
}
