// The computer's master volume. The desktop reports it as a reserved pseudo-player in the
// same `MediaState` list as the real players: player ID "@system", never playing, no track,
// `volume` 0 to 100 (0 when muted) and `can` holding only `volume`. A `volume` command to
// that ID sets the master volume. It is never a "Now playing" row: the app splits it off
// with `SystemVolume.split` and shows it as its own slider.
//
// `VolumeSliderModel` holds the slider rules: while the finger drags, commands go out at
// most every `minInterval` seconds (about 5 per second), the slider follows the finger and
// ignores incoming states, and the release always sends the final value. After the release
// the slider keeps the released value until the computer echoes it (or `echoWait` passes),
// so it does not jump back to an older reported level.
//
// Works on plain values so it builds without the generated core types.
import Foundation

/// The reserved system volume entry; see the file comment.
public enum SystemVolume {
    /// The reserved player ID of the computer's master volume. No real player can have it.
    public static let playerId = "@system"

    /// True for the system volume entry.
    public static func isSystem(_ playerId: String) -> Bool {
        playerId == SystemVolume.playerId
    }

    /// `players` split into the system volume entry (the first one, if any) and the real
    /// players in their original order. Every system entry is left out of `players`.
    public static func split<Player>(
        _ players: [Player], id: (Player) -> String
    ) -> (system: Player?, players: [Player]) {
        var system: Player?
        var rest: [Player] = []
        rest.reserveCapacity(players.count)
        for player in players {
            if isSystem(id(player)) {
                if system == nil { system = player }
            } else {
                rest.append(player)
            }
        }
        return (system, rest)
    }

    /// The real players of `players`, without the system volume entry.
    public static func realPlayers<Player>(_ players: [Player], id: (Player) -> String) -> [Player] {
        players.filter { !isSystem(id($0)) }
    }

    /// A slider or reported value as a whole percent from 0 to 100. Not finite reads as 0.
    public static func percent(_ value: Double) -> Int {
        guard value.isFinite else { return 0 }
        return Int(min(max(value, 0), 100).rounded())
    }

    /// A reported volume as a whole percent from 0 to 100; nil (not reported) reads as 0.
    public static func percent(_ value: Int?) -> Int {
        min(max(value ?? 0, 0), 100)
    }
}

/// What the system volume slider shows and when it sends; see the file comment. Times are
/// seconds on any monotonic clock (the app uses `ProcessInfo.systemUptime`).
public struct VolumeSliderModel: Equatable, Sendable {
    /// Shortest gap between two commands while dragging (5 per second).
    public static let defaultMinInterval: TimeInterval = 0.2
    /// How long the released value is kept while waiting for the computer's echo.
    public static let defaultEchoWait: TimeInterval = 1.5

    /// Shortest gap between two commands while dragging.
    public let minInterval: TimeInterval
    /// How long the released value is kept without an echo.
    public let echoWait: TimeInterval

    /// The latest level the computer reported, 0 to 100.
    public private(set) var reported: Int = 0
    /// A finger is on the slider.
    public private(set) var dragging = false

    private var finger = 0
    /// The released value, kept until the echo arrives or `echoWait` passes.
    private var held: Int?
    private var heldSince: TimeInterval = 0
    private var lastSent: Int?
    private var lastSentAt: TimeInterval?
    /// A drag value held back by the throttle, sent by `flush` or the release.
    private var unsent: Int?

    /// A model with the given timings.
    public init(
        minInterval: TimeInterval = VolumeSliderModel.defaultMinInterval,
        echoWait: TimeInterval = VolumeSliderModel.defaultEchoWait
    ) {
        self.minInterval = minInterval
        self.echoWait = echoWait
    }

    /// The value the slider shows: the finger while dragging, the released value until its
    /// echo arrives, else the reported level.
    public var shown: Int {
        if dragging { return finger }
        return held ?? reported
    }

    /// A released value is waiting for its echo.
    public var awaitingEcho: Bool { held != nil }

    /// A new level from the computer (nil: not reported, read as 0). Recorded always; it
    /// shows only when no finger is on the slider and no released value waits for its echo.
    /// The echo of the released value ends the wait.
    public mutating func report(_ volume: Int?) {
        reported = SystemVolume.percent(volume)
        if let held, held == reported { self.held = nil }
    }

    /// The finger went down on the slider.
    public mutating func begin() {
        if !dragging { finger = shown }
        dragging = true
        held = nil
    }

    /// The finger moved to `value`. Returns the value to send now, or nil when it is the
    /// value last sent or the throttle holds it back (see `flushDelay`).
    public mutating func drag(to value: Double, now: TimeInterval) -> Int? {
        if !dragging { begin() }
        finger = SystemVolume.percent(value)
        if finger == lastSent {
            unsent = nil
            return nil
        }
        if let at = lastSentAt, !intervalPassed(since: at, now: now) {
            unsent = finger
            return nil
        }
        return markSent(finger, now: now)
    }

    /// Seconds until a held-back drag value may go out; nil when none waits.
    public func flushDelay(now: TimeInterval) -> TimeInterval? {
        guard dragging, unsent != nil else { return nil }
        guard let at = lastSentAt else { return 0 }
        return max(0, at + minInterval - now)
    }

    /// The held-back drag value when the throttle now lets it go, else nil.
    public mutating func flush(now: TimeInterval) -> Int? {
        guard dragging, let value = unsent else { return nil }
        if let at = lastSentAt, !intervalPassed(since: at, now: now) { return nil }
        return markSent(value, now: now)
    }

    /// The finger lifted. Returns the final value, which is always sent, and keeps showing
    /// it until the echo arrives.
    public mutating func release(now: TimeInterval) -> Int {
        let value = dragging ? finger : shown
        dragging = false
        held = value == reported ? nil : value
        heldSince = now
        return markSent(value, now: now)
    }

    /// A change without a drag (an accessibility increment, a tap). Returns the value to
    /// send, which is always sent, and holds it like a release.
    public mutating func set(to value: Double, now: TimeInterval) -> Int {
        dragging = true
        finger = SystemVolume.percent(value)
        return release(now: now)
    }

    /// Stops keeping the released value once `echoWait` has passed without an echo, so the
    /// slider follows the computer again.
    public mutating func expire(now: TimeInterval) {
        if held != nil, now - heldSince >= echoWait { held = nil }
    }

    /// `minInterval` has passed since `at`, with a millisecond of slack so a timer that
    /// fires on the dot is not turned away by rounding.
    private func intervalPassed(since at: TimeInterval, now: TimeInterval) -> Bool {
        now - at >= minInterval - 0.001
    }

    private mutating func markSent(_ value: Int, now: TimeInterval) -> Int {
        lastSent = value
        lastSentAt = now
        unsent = nil
        return value
    }
}
