import Foundation
import PlayDropShared

// Number and time formatting for the screens. Byte sizes, rates and durations delegate to
// `PlayDropShared.ByteFormat` so the app, the share extension and the tests agree on the
// figures; this file adds only what is UI-specific (countdowns, SAS grouping, relative
// times). Port of Android `ui/common/Format.kt`.

/// Formatting helpers for views.
enum UIFormat {
    /// "1.2 GiB" (binary units, from `ByteFormat`).
    static func bytes(_ n: UInt64) -> String { ByteFormat.bytes(n) }

    /// "32.1 MiB/s".
    static func rate(_ bytesPerSec: UInt64) -> String { ByteFormat.rate(bytesPerSec) }

    /// "13 s", "2 min 4 s" for a number of seconds.
    static func duration(seconds: UInt64) -> String { ByteFormat.duration(ms: seconds &* 1000) }

    /// Whole percent done, 0 when the total is unknown.
    static func percent(_ bytes: UInt64, of total: UInt64) -> Int { ByteFormat.percent(bytes, of: total) }

    /// Fraction done in 0...1 for a `ProgressView`; 0 when the total is unknown.
    static func fraction(_ bytes: UInt64, of total: UInt64) -> Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, Double(bytes) / Double(total)))
    }

    /// Seconds left at the current rate, or nil when it cannot be estimated.
    static func etaSeconds(remaining: UInt64, bytesPerSec: UInt64) -> UInt64? {
        guard bytesPerSec > 0 else { return nil }
        return remaining / bytesPerSec + (remaining % bytesPerSec == 0 ? 0 : 1)
    }

    /// "m:ss" for a countdown.
    static func countdown(_ seconds: Int) -> String {
        let s = max(0, seconds)
        return String(format: "%d:%02d", s / 60, s % 60)
    }

    /// Groups a six digit SAS as "123 456" so it is easy to compare across two screens.
    static func sas(_ sas: String) -> String {
        guard sas.count == 6 else { return sas }
        let mid = sas.index(sas.startIndex, offsetBy: 3)
        return "\(sas[..<mid]) \(sas[mid...])"
    }

    /// Current Unix time in seconds.
    static func nowSecs(_ now: Date = Date()) -> UInt64 {
        UInt64(max(0, now.timeIntervalSince1970))
    }

    /// "just now", "5 min ago", "3 hours ago", "never" for a Unix-seconds timestamp.
    static func ago(_ then: UInt64?, now: UInt64) -> String {
        guard let then else { return L10n.Ago.never }
        let d = now > then ? now - then : 0
        switch d {
        case ..<60: return L10n.Ago.justNow
        case ..<3600: return L10n.Ago.minutes(Int(d / 60))
        case ..<86_400: return L10n.Ago.hours(Int(d / 3600))
        default: return L10n.Ago.days(Int(d / 86_400))
        }
    }

    /// A medium date ("12 Oct 2026") for a Unix-seconds timestamp.
    static func date(unixSecs: UInt64) -> String {
        Date(timeIntervalSince1970: TimeInterval(unixSecs)).formatted(date: .abbreviated, time: .omitted)
    }
}
