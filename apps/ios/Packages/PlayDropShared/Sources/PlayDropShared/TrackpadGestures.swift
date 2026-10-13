// Turns raw touches on the phone's trackpad surface into pointer frames (protocol.md 5.12).
// The view reports how many fingers are down, where their centre is and when; this type
// decides what that means, so the rules are tested on Linux without UIKit:
//
// - one finger moving: pointer moves, scaled by the sensitivity. Movement under `slop`
//   points is held back, so a tap does not nudge the pointer; once past it, the whole
//   distance is sent and every later change goes out as it happens.
// - one finger lifted quickly without moving: left click.
// - one finger held still for `longPress` seconds: left button down; moving then drags,
//   and lifting releases the button.
// - two fingers moving: scroll, one line per `pointsPerLine` points, with the remainder
//   kept for the next change. Natural direction, as on the iPhone: moving the fingers up
//   scrolls the content up, which is a positive (downward) scroll.
// - two fingers lifted quickly without moving: right click.
import Foundation

/// A pointer button on the trackpad screen.
public enum TrackpadButton: Equatable, Sendable {
    case left, middle, right
}

/// What happens to a button.
public enum TrackpadButtonAction: Equatable, Sendable {
    case down, up, click
}

/// One thing to send for a touch change.
public enum TrackpadOutput: Equatable, Sendable {
    /// Relative pointer move, already scaled by the sensitivity.
    case move(dx: Double, dy: Double)
    /// A button change.
    case button(TrackpadButton, TrackpadButtonAction)
    /// Scroll by whole lines (positive `dy` scrolls down, positive `dx` right).
    case scroll(dx: Double, dy: Double)
}

/// A point on the surface, in points.
public struct TrackpadPoint: Equatable, Sendable {
    public var x: Double
    public var y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }

    func distance(to other: TrackpadPoint) -> Double {
        let dx = other.x - x, dy = other.y - y
        return (dx * dx + dy * dy).squareRoot()
    }
}

/// The gesture state machine; see the file comment. Not thread-safe: feed it from one place.
public struct TrackpadGestures: Sendable {
    /// Default pointer sensitivity (pointer units per point).
    public static let defaultSensitivity = 1.5
    /// Range of the sensitivity slider.
    public static let sensitivityRange: ClosedRange<Double> = 0.5...4.0

    /// Pointer units per point of finger movement.
    public var sensitivity: Double
    /// Movement below this many points still counts as a tap.
    public var slop: Double = 8
    /// A tap must end within this many seconds.
    public var tapTime: TimeInterval = 0.3
    /// Holding one finger still this long presses the left button.
    public var longPress: TimeInterval = 0.5
    /// Points of two-finger movement per scroll line.
    public var pointsPerLine: Double = 48

    private enum Phase: Sendable {
        case idle
        /// One finger down. `tracking` once it moved past the slop; `dragging` once the
        /// long press pressed the left button.
        case one(start: TrackpadPoint, startTime: TimeInterval, last: TrackpadPoint, tracking: Bool, dragging: Bool)
        /// Two or more fingers were down at some point; ends when all are lifted.
        case two(start: TrackpadPoint, startTime: TimeInterval, last: TrackpadPoint, moved: Bool, restX: Double, restY: Double)
    }

    private var phase: Phase = .idle

    /// A machine with `sensitivity`, clamped to `sensitivityRange`.
    public init(sensitivity: Double = TrackpadGestures.defaultSensitivity) {
        self.sensitivity = Self.clampSensitivity(sensitivity)
    }

    /// `value` clamped to `sensitivityRange`; the default for a value that is not finite.
    public static func clampSensitivity(_ value: Double) -> Double {
        guard value.isFinite else { return defaultSensitivity }
        return min(max(value, sensitivityRange.lowerBound), sensitivityRange.upperBound)
    }

    /// True while the long press holds the left button down.
    public var isDragging: Bool {
        if case .one(_, _, _, _, true) = phase { return true }
        return false
    }

    /// The touches changed: `fingers` are down now (at least one), centred on `centroid`.
    /// Call on every begin and move.
    public mutating func touches(_ fingers: Int, at centroid: TrackpadPoint, time: TimeInterval) -> [TrackpadOutput] {
        guard fingers > 0 else { return ended(time: time) }
        switch phase {
        case .idle:
            if fingers == 1 {
                phase = .one(start: centroid, startTime: time, last: centroid, tracking: false, dragging: false)
            } else {
                phase = .two(start: centroid, startTime: time, last: centroid, moved: false, restX: 0, restY: 0)
            }
            return []

        case let .one(start, startTime, last, tracking, dragging):
            if fingers >= 2 && !dragging {
                // A second finger joined: this is a two-finger gesture from here on.
                phase = .two(start: centroid, startTime: startTime, last: centroid, moved: tracking, restX: 0, restY: 0)
                return []
            }
            if !tracking && centroid.distance(to: start) < slop {
                phase = .one(start: start, startTime: startTime, last: last, tracking: false, dragging: dragging)
                return []
            }
            // Past the slop (or already tracking): send everything since the last sent point.
            let from = tracking ? last : start
            phase = .one(start: start, startTime: startTime, last: centroid, tracking: true, dragging: dragging)
            let dx = (centroid.x - from.x) * sensitivity
            let dy = (centroid.y - from.y) * sensitivity
            return dx == 0 && dy == 0 ? [] : [.move(dx: dx, dy: dy)]

        case let .two(start, startTime, last, moved, restX, restY):
            if fingers < 2 {
                // One finger lifted: the centre jumps, so ignore it until all are lifted.
                phase = .two(start: start, startTime: startTime, last: centroid, moved: moved, restX: restX, restY: restY)
                return []
            }
            let nowMoved = moved || centroid.distance(to: start) >= slop
            // Natural scrolling: fingers moving up (negative y) scroll down (positive).
            let totalX = restX - (centroid.x - last.x)
            let totalY = restY - (centroid.y - last.y)
            guard nowMoved else {
                phase = .two(start: start, startTime: startTime, last: last, moved: false, restX: restX, restY: restY)
                return []
            }
            let linesX = (totalX / pointsPerLine).rounded(.towardZero)
            let linesY = (totalY / pointsPerLine).rounded(.towardZero)
            phase = .two(
                start: start, startTime: startTime, last: centroid, moved: true,
                restX: totalX - linesX * pointsPerLine, restY: totalY - linesY * pointsPerLine
            )
            return linesX == 0 && linesY == 0 ? [] : [.scroll(dx: linesX, dy: linesY)]
        }
    }

    /// Every finger was lifted (or the touches were cancelled).
    public mutating func ended(time: TimeInterval) -> [TrackpadOutput] {
        defer { phase = .idle }
        switch phase {
        case .idle:
            return []
        case let .one(_, startTime, _, tracking, dragging):
            if dragging { return [.button(.left, .up)] }
            if !tracking && time - startTime <= tapTime { return [.button(.left, .click)] }
            return []
        case let .two(_, startTime, _, moved, _, _):
            if !moved && time - startTime <= tapTime { return [.button(.right, .click)] }
            return []
        }
    }

    /// The touches were cancelled by the system: release a held button, click nothing.
    public mutating func cancelled() -> [TrackpadOutput] {
        defer { phase = .idle }
        return isDragging ? [.button(.left, .up)] : []
    }

    /// Time passed without a touch change. Starts the long-press drag when one finger has
    /// rested within the slop for `longPress` seconds. The view calls this from a timer.
    public mutating func tick(time: TimeInterval) -> [TrackpadOutput] {
        guard case let .one(start, startTime, last, tracking, dragging) = phase,
              !tracking, !dragging, time - startTime >= longPress
        else { return [] }
        phase = .one(start: start, startTime: startTime, last: last, tracking: false, dragging: true)
        return [.button(.left, .down)]
    }
}
