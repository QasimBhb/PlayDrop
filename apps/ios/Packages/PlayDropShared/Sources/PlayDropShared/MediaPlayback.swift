// Rules for the "Now playing" rows of a computer's media players (protocol.md 5.9). The
// desktop sends a player's position once, with the Unix time it was read (`positionAt`),
// and never streams it, so the phone extrapolates while the player plays. A state that is
// not playing and allows no action means the player went away: its row is removed.
//
// Works on plain values so it builds without the generated core types; the app maps
// `MediaState` fields into these calls.
import Foundation

/// Position extrapolation and the player list rules; see the file comment.
public enum MediaPlayback {
    /// The player is gone: not playing and no action allowed.
    public static func isGone(playing: Bool, canIsEmpty: Bool) -> Bool {
        !playing && canIsEmpty
    }

    /// The position to show at `nowMs` (Unix ms): `positionMs` plus the time since
    /// `positionAtMs` while playing, the reported value while paused, never past
    /// `durationMs` and never before zero. Nil when the player reported no position.
    /// A clock that runs behind the desktop's (now before `positionAtMs`) adds nothing.
    public static func position(
        positionMs: UInt64?, positionAtMs: UInt64?, durationMs: UInt64?,
        playing: Bool, nowMs: UInt64
    ) -> UInt64? {
        guard let positionMs else { return nil }
        var position = positionMs
        if playing, let at = positionAtMs, nowMs > at {
            let (sum, overflow) = position.addingReportingOverflow(nowMs - at)
            position = overflow ? UInt64.max : sum
        }
        if let durationMs, durationMs > 0 {
            position = min(position, durationMs)
        }
        return position
    }

    /// The share of the track played at `positionMs`, from 0 to 1; nil without a duration.
    public static func fraction(positionMs: UInt64?, durationMs: UInt64?) -> Double? {
        guard let positionMs, let durationMs, durationMs > 0 else { return nil }
        return min(1, Double(positionMs) / Double(durationMs))
    }

    /// The seek target in ms for a tap or drag at `fraction` of the bar.
    public static func seekTarget(fraction: Double, durationMs: UInt64) -> UInt64 {
        guard fraction.isFinite else { return 0 }
        let clamped = min(max(fraction, 0), 1)
        return UInt64((clamped * Double(durationMs)).rounded())
    }

    /// "1:05", "12:00", "1:02:03" for a position or duration in ms.
    public static func clock(_ ms: UInt64) -> String {
        let total = ms / 1000
        let h = total / 3600, m = (total % 3600) / 60, s = total % 60
        let ss = s < 10 ? "0\(s)" : "\(s)"
        if h > 0 {
            let mm = m < 10 ? "0\(m)" : "\(m)"
            return "\(h):\(mm):\(ss)"
        }
        return "\(m):\(ss)"
    }

    /// `players` after a new state for one player: replaced in place when known, appended
    /// when new, removed when `gone`. Other players keep their order.
    public static func merge<Player>(
        _ players: [Player], with incoming: Player, gone: Bool, id: (Player) -> String
    ) -> [Player] {
        let key = id(incoming)
        var out = players
        if let i = out.firstIndex(where: { id($0) == key }) {
            if gone { out.remove(at: i) } else { out[i] = incoming }
        } else if !gone {
            out.append(incoming)
        }
        return out
    }
}
