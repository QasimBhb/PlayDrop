// Batch ids for the outgoing queue: 64 random bits as 16 lowercase hex characters. They are
// directory names, so they are validated before any path is built from one.

/// Creation and validation of outgoing batch ids.
public enum OutgoingBatchId {
    /// Length of an id in characters.
    public static let length = 16

    /// A fresh random id from the system's secure generator.
    public static func random() -> String {
        var rng = SystemRandomNumberGenerator()
        let value = rng.next() as UInt64
        let hex = String(value, radix: 16, uppercase: false)
        return String(repeating: "0", count: length - hex.count) + hex
    }

    /// True when `s` is exactly 16 lowercase hex characters.
    public static func isValid(_ s: String) -> Bool {
        s.utf8.count == length && s.utf8.allSatisfy { (0x30...0x39).contains($0) || (0x61...0x66).contains($0) }
    }
}
