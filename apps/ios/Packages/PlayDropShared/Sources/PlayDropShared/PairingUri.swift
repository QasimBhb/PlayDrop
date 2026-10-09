// Recognises a PlayDrop pairing QR code (protocol.md section 6.3): `playdrop://pair?...`. The
// check is deliberately narrow so a scanned web link or any other `playdrop://` URL is never
// handed to the core's pairing call; the core itself validates the parameters. The URI holds
// a one-time secret, so it is never logged (SR-32).
import Foundation

/// Pairing URI recognition.
public enum PairingUri {
    /// The required prefix; scheme and host are compared without regard to case.
    public static let prefix = "\(PlayDropIdentifiers.urlScheme)://pair?"

    /// True when `s` (surrounding whitespace ignored) starts with `playdrop://pair?` and
    /// has a non-empty query.
    public static func isPairingUri(_ s: String) -> Bool {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard t.utf8.count > prefix.utf8.count else { return false }
        let head = String(t.prefix(prefix.count))
        return head.lowercased() == prefix
    }
}
