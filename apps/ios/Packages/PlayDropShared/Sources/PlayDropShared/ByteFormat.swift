// Number and time formatting shared by every screen, the share extension and the
// notifications. Units follow the desktop window and the Android app (binary units with one
// decimal) so both ends of a transfer show the same figures. Formatting is done by hand
// rather than through `String(format:)` or a formatter so the output never depends on the
// user's locale and is identical on Linux, where the package is tested.

/// Byte counts, rates, durations and progress as short display strings.
public enum ByteFormat {
    /// Binary units above bytes, in order.
    private static let units = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]

    /// A byte count in binary units with one decimal: "0 B", "512 B", "1.5 KiB", "12.0 MiB".
    public static func bytes(_ n: UInt64) -> String {
        if n < 1024 { return "\(n) B" }
        var value = Double(n) / 1024.0
        var unit = 0
        while value >= 1024.0 && unit + 1 < units.count {
            value /= 1024.0
            unit += 1
        }
        return "\(oneDecimal(value)) \(units[unit])"
    }

    /// A transfer rate: "30.0 MiB/s".
    public static func rate(_ bytesPerSec: UInt64) -> String {
        "\(bytes(bytesPerSec))/s"
    }

    /// A duration given in milliseconds: "<1 s", "45 s", "2 min", "2 min 5 s", "1 h 3 min".
    /// Partial seconds are dropped, matching the Android and desktop wording.
    public static func duration(ms: UInt64) -> String {
        let secs = ms / 1000
        switch secs {
        case 0:
            return "<1 s"
        case ..<60:
            return "\(secs) s"
        case ..<3600:
            let m = secs / 60
            let s = secs % 60
            return s == 0 ? "\(m) min" : "\(m) min \(s) s"
        default:
            return "\(secs / 3600) h \((secs % 3600) / 60) min"
        }
    }

    /// Whole percent done, clamped to 0...100; 0 when the total is unknown (0).
    public static func percent(_ bytes: UInt64, of total: UInt64) -> Int {
        guard total > 0 else { return 0 }
        if bytes >= total { return 100 }
        // Double keeps this exact enough for display without overflowing `bytes * 100`.
        return min(100, Int((Double(bytes) / Double(total)) * 100.0))
    }

    /// `value` rounded half-up to one decimal place, always printed with that decimal.
    private static func oneDecimal(_ value: Double) -> String {
        let tenths = UInt64((value * 10.0).rounded(.toNearestOrAwayFromZero))
        return "\(tenths / 10).\(tenths % 10)"
    }
}
