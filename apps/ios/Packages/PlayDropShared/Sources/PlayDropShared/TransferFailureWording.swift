// What a failed transfer says. The core describes a failure in two ways: a machine-readable
// code (`TransferFailure`, `playdrop-ffi` 0.6.0 and later) and an English text that, on older
// paths, may still carry the wire's own words ("507 Insufficient Storage no_space"). A person
// should never read an HTTP status or a snake_case wire code, so every place that shows a
// failure goes through `resolve` and gets either a known code (the app words it), the core's
// own text when it is already plain, or nothing technical at all.
//
// The code is mirrored here (this package cannot import the generated bindings). Until the
// bindings carry it, callers pass `nil` and the text is classified instead: the same statuses
// and wire codes the core would have mapped.
import Foundation

/// Why a transfer did not complete; one case per core `TransferFailure`.
public enum TransferFailureCode: Equatable, Sendable, CaseIterable {
    /// The receiving device is out of room.
    case noSpace
    /// The receiving user declined the offer.
    case declined
    /// The offer is over the receiver's limits.
    case tooLarge
    /// The receiver is busy with its limit of transfers from this device.
    case rateLimited
    /// The other device cancelled it.
    case cancelledByPeer
    /// This device's user cancelled it.
    case cancelledByYou
    /// Data failed its integrity check.
    case integrity
    /// The other device could not be reached.
    case connectionLost
    /// Nobody answered in time.
    case timedOut
    /// Anything else.
    case other
}

/// What to show for a failure.
public enum TransferFailureText: Equatable, Sendable {
    /// A known reason; the app words it for the direction.
    case known(TransferFailureCode)
    /// The core's own text, which carries no wire status or code.
    case plain(String)
}

/// See the file comment.
public enum TransferFailureWording {
    /// The text to show for a failure with `code` (when the core sent one) and the core's
    /// `text`, or nil when there is nothing to say.
    public static func resolve(code: TransferFailureCode?, text: String?) -> TransferFailureText? {
        if let code, code != .other { return .known(code) }
        let trimmed = text?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        if let classified = classify(trimmed) { return .known(classified) }
        if trimmed.isEmpty { return code == nil ? nil : .known(.other) }
        if carriesWireWords(trimmed) { return .known(.other) }
        return .plain(trimmed)
    }

    /// The reason a wire-flavoured text stands for, if it names one.
    static func classify(_ text: String) -> TransferFailureCode? {
        let lower = text.lowercased()
        let table: [(needles: [String], code: TransferFailureCode)] = [
            (["no_space", "insufficient storage", "507"], .noSpace),
            (["too_large", "payload too large", "413"], .tooLarge),
            (["too many requests", "rate_limited", "429"], .rateLimited),
            (["declined"], .declined),
        ]
        for entry in table where entry.needles.contains(where: { lower.contains($0) }) {
            return entry.code
        }
        return nil
    }

    /// True when the text holds an HTTP error status or a snake_case wire code.
    static func carriesWireWords(_ text: String) -> Bool {
        text.range(of: #"(?<![0-9])[45][0-9]{2}(?![0-9])"#, options: .regularExpression) != nil
            || text.range(of: #"\b[a-z]+(_[a-z]+)+\b"#, options: .regularExpression) != nil
    }
}
