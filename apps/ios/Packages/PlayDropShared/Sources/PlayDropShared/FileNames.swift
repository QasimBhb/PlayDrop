// Name rules for files the iPhone sends. A name that leaves this phone must pass every
// receiver's manifest check (threat model SR-17), because one bad component rejects the
// whole transfer there; so names are cleaned here, at the moment the user shares, and the
// cleaned name is what lands in the outgoing queue and on the wire. Ported from the Android
// app's `core/FileNames.kt` and tightened to the full SR-17 rule set (Windows-reserved names
// and characters too, so a send to a Windows desktop never fails on a name).
import Foundation

/// Cleaning, de-duplicating and typing of single file names.
public enum FileNames {
    /// Used when nothing usable is left of a name.
    public static let fallbackName = "file"

    /// Longest name in UTF-8 bytes (ext4, APFS and NTFS limits; SR-17).
    public static let maxNameBytes = 255

    /// Longest extension (with its dot) kept intact when a long name is shortened.
    private static let maxKeptExtensionBytes = 32

    /// Characters Windows refuses in a name (SR-17 asks every platform to refuse them).
    private static let windowsReserved: Set<Character> = ["<", ">", ":", "\"", "|", "?", "*"]

    /// Device names Windows reserves, with or without an extension (SR-17).
    private static let reservedStems: Set<String> = ["CON", "PRN", "AUX", "NUL"]

    /// Digits that make `COM<d>` and `LPT<d>` reserved, superscripts included (SR-17).
    private static let reservedPortDigits: Set<Character> = [
        "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "\u{00B9}", "\u{00B2}", "\u{00B3}",
    ]

    /// One plain path component that passes SR-17 on every platform. Separators (`/`, `\`),
    /// NUL, C0 and C1 controls, bidi overrides and isolates (U+202A-202E, U+2066-2069),
    /// marks U+200E and U+200F, and `<>:"|?*` become `_`; the result is NFC-normalised;
    /// leading spaces and trailing dots and spaces are trimmed; a Windows device name
    /// (`CON`, `COM1.txt`, ...) gets a leading `_`; the name is cut to 255 UTF-8 bytes
    /// on a character boundary, keeping a short extension. Never returns an empty string,
    /// `.` or `..`: those become `fallbackName`.
    public static func sanitized(_ name: String) -> String {
        var cleaned = ""
        cleaned.unicodeScalars.reserveCapacity(name.unicodeScalars.count)
        for scalar in name.precomposedStringWithCanonicalMapping.unicodeScalars {
            cleaned.unicodeScalars.append(isForbidden(scalar) ? "_" : scalar)
        }
        cleaned = trimmed(cleaned)
        if cleaned.isEmpty || cleaned == "." || cleaned == ".." { return fallbackName }
        if isReservedDeviceName(cleaned) { cleaned = "_" + cleaned }
        cleaned = truncated(cleaned, toBytes: maxNameBytes)
        // Cutting can expose a trailing dot or space again.
        cleaned = trimmed(cleaned)
        return cleaned.isEmpty || cleaned == "." || cleaned == ".." ? fallbackName : cleaned
    }

    /// `name` itself when `exists` says it is free, else the first free of "a (2).txt",
    /// "a (3).txt", ... (the same " (n)" rule receivers use, SR-18). The stem is shortened
    /// when the suffix would push the name past 255 bytes. `name` should already be
    /// sanitised; the result then is too.
    public static func unique(_ name: String, exists: (String) -> Bool) -> String {
        if !exists(name) { return name }
        let (stem, ext) = splitExtension(name)
        var n = 2
        while true {
            let suffix = " (\(n))" + ext
            let room = maxNameBytes - suffix.utf8.count
            let candidate = truncated(stem, toBytes: max(room, 1)) + suffix
            if !exists(candidate) { return candidate }
            n += 1
        }
    }

    /// The MIME type for a lower- or upper-case file extension without the dot, from a small
    /// table of what people share from an iPhone; nil when unknown (the receiver then
    /// guesses from the name).
    public static func mimeType(forExtension ext: String) -> String? {
        switch ext.lowercased() {
        case "jpg", "jpeg": return "image/jpeg"
        case "png": return "image/png"
        case "heic": return "image/heic"
        case "gif": return "image/gif"
        case "mp4": return "video/mp4"
        case "mov": return "video/quicktime"
        case "m4v": return "video/x-m4v"
        case "pdf": return "application/pdf"
        case "txt": return "text/plain"
        case "zip": return "application/zip"
        case "json": return "application/json"
        default: return nil
        }
    }

    /// Lower-case extension of `name` without the dot, or nil when it has none (a leading
    /// dot, as in ".bashrc", is not an extension).
    public static func fileExtension(of name: String) -> String? {
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex,
              name.index(after: dot) != name.endIndex
        else { return nil }
        return String(name[name.index(after: dot)...]).lowercased()
    }

    /// True for the scalars SR-17 forbids inside a component.
    private static func isForbidden(_ s: Unicode.Scalar) -> Bool {
        switch s.value {
        case 0x00...0x1F, 0x7F...0x9F: return true          // C0, DEL, C1
        case 0x202A...0x202E, 0x2066...0x2069: return true   // bidi overrides, isolates
        case 0x200E, 0x200F: return true                     // LRM, RLM
        case 0x2F, 0x5C: return true                         // "/" and "\"
        default: return windowsReserved.contains(Character(s))
        }
    }

    /// Leading whitespace and trailing dots and whitespace removed.
    private static func trimmed(_ s: String) -> String {
        let scalars = Array(s.unicodeScalars)
        var start = 0
        var end = scalars.count
        while start < end, scalars[start].properties.isWhitespace { start += 1 }
        while end > start, scalars[end - 1] == "." || scalars[end - 1].properties.isWhitespace {
            end -= 1
        }
        var out = String.UnicodeScalarView()
        out.append(contentsOf: scalars[start..<end])
        return String(out)
    }

    /// True for `CON`, `nul.txt`, `COM1`, `lpt\u{00B9}.tar.gz` and the like.
    private static func isReservedDeviceName(_ name: String) -> Bool {
        let stem = name.split(separator: ".", maxSplits: 1, omittingEmptySubsequences: false)
            .first.map(String.init) ?? name
        let upper = stem.trimmingCharacters(in: .whitespaces).uppercased()
        if reservedStems.contains(upper) { return true }
        if upper.count == 4, upper.hasPrefix("COM") || upper.hasPrefix("LPT"),
           let last = upper.last, reservedPortDigits.contains(last) {
            return true
        }
        return false
    }

    /// `name` split before its last dot: ("a.tar", ".gz"); ("a", "") with no extension.
    /// An extension longer than `maxKeptExtensionBytes` is treated as part of the stem.
    private static func splitExtension(_ name: String) -> (stem: String, ext: String) {
        guard let dot = name.lastIndex(of: "."), dot != name.startIndex else { return (name, "") }
        let ext = String(name[dot...])
        if ext.utf8.count > maxKeptExtensionBytes || ext.count == 1 { return (name, "") }
        return (String(name[..<dot]), ext)
    }

    /// `name` cut to at most `limit` UTF-8 bytes on a character boundary, keeping its
    /// extension when it has a short one.
    private static func truncated(_ name: String, toBytes limit: Int) -> String {
        if name.utf8.count <= limit { return name }
        let (stem, ext) = splitExtension(name)
        let extBytes = ext.utf8.count
        if extBytes > 0 && extBytes < limit {
            return prefix(stem, bytes: limit - extBytes) + ext
        }
        return prefix(name, bytes: limit)
    }

    /// The longest run of whole characters from the start of `s` within `bytes` UTF-8 bytes.
    private static func prefix(_ s: String, bytes: Int) -> String {
        var used = 0
        var out = ""
        for ch in s {
            let n = ch.utf8.count
            if used + n > bytes { break }
            used += n
            out.append(ch)
        }
        return out
    }
}
