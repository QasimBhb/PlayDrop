// Socket address helpers on plain strings. They never trigger a DNS lookup, so typing an
// address can never leak a name to the network, and they run in Linux unit tests. The core
// reports addresses as `ip:port`, with IPv6 as `[ip]:port` (protocol.md section 1). Ported
// from the Android app's `ui/common/Addresses.kt`; only IP literals are accepted, as there.

/// Parsing and comparison of `ip:port` strings.
public enum Addresses {
    /// Splits `1.2.3.4:42187`, `[fd00::1]:42187`, a bare IPv4 literal or a bare IPv6 literal
    /// (with or without brackets) into host and port. A missing port becomes
    /// `PlayDropIdentifiers.defaultPort`. The host comes back without brackets. Returns nil
    /// for anything that is not an IP literal with an optional port in 1...65535; host
    /// names are refused because the core dials literals only.
    public static func parseHostPort(_ s: String) -> (host: String, port: UInt16)? {
        let t = s.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.isEmpty { return nil }

        if t.hasPrefix("[") {
            guard let close = t.firstIndex(of: "]") else { return nil }
            let ip = String(t[t.index(after: t.startIndex)..<close])
            guard isIpv6Literal(ip) else { return nil }
            let rest = t[t.index(after: close)...]
            if rest.isEmpty { return (ip, PlayDropIdentifiers.defaultPort) }
            guard rest.hasPrefix(":"), let port = parsePort(rest.dropFirst()) else { return nil }
            return (ip, port)
        }

        let colons = t.reduce(0) { $1 == ":" ? $0 + 1 : $0 }
        switch colons {
        case 0:
            return parseIpv4(t) != nil ? (t, PlayDropIdentifiers.defaultPort) : nil
        case 1:
            guard let colon = t.firstIndex(of: ":") else { return nil }
            let ip = String(t[..<colon])
            guard parseIpv4(ip) != nil, let port = parsePort(t[t.index(after: colon)...]) else {
                return nil
            }
            return (ip, port)
        default:
            return isIpv6Literal(t) ? (t, PlayDropIdentifiers.defaultPort) : nil
        }
    }

    /// True when `parseHostPort` accepts `s`.
    public static func isValidHostPort(_ s: String) -> Bool {
        parseHostPort(s) != nil
    }

    /// What the core's `pairWith` expects: `1.2.3.4:42187` or `[fd00::1]:42187`, with the
    /// default port added when none was typed. Nil when `s` is not an address.
    public static func normalized(_ s: String) -> String? {
        guard let (host, port) = parseHostPort(s) else { return nil }
        return host.contains(":") ? "[\(host)]:\(port)" : "\(host):\(port)"
    }

    /// The host part of `ip:port` or `[ip]:port`, without brackets; nil when the text is not
    /// an address.
    public static func host(of addrPort: String) -> String? {
        parseHostPort(addrPort)?.host
    }

    /// True when `addr` (`ip:port` or a bare IPv4) is IPv4 and on the same subnet as the
    /// interface `ifaceAddr` with mask `netmask`. IPv6 or unparsable input is never a match.
    public static func sameSubnet(_ addr: String, _ ifaceAddr: String, _ netmask: String) -> Bool {
        guard let host = host(of: addr),
              let a = parseIpv4(host),
              let i = parseIpv4(ifaceAddr),
              let m = parseIpv4(netmask)
        else { return false }
        return (a & m) == (i & m)
    }

    /// A dotted IPv4 literal as a 32-bit number, or nil when `text` is not exactly four
    /// decimal octets of at most three digits each.
    public static func parseIpv4(_ text: String) -> UInt32? {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return nil }
        var out: UInt32 = 0
        for p in parts {
            guard !p.isEmpty, p.count <= 3, p.allSatisfy({ $0.isASCII && $0.isNumber }),
                  let v = UInt32(p), v <= 255
            else { return nil }
            out = (out << 8) | v
        }
        return out
    }

    /// A TCP port in 1...65535, or nil.
    public static func parsePort<S: StringProtocol>(_ text: S) -> UInt16? {
        guard !text.isEmpty, text.count <= 5, text.allSatisfy({ $0.isASCII && $0.isNumber }),
              let v = Int(text), (1...65535).contains(v)
        else { return nil }
        return UInt16(v)
    }

    /// True for an IPv6 literal in the RFC 4291 text forms: up to eight groups of one to
    /// four hex digits, at most one `::`, and an optional dotted IPv4 tail. Zone ids
    /// (`%en0`) are refused because the core's address parser does not take them.
    public static func isIpv6Literal(_ text: String) -> Bool {
        guard text.contains(":"), text.count <= 45 else { return false }
        let halves = text.components(separatedBy: "::")
        guard halves.count <= 2 else { return false }
        let compressed = halves.count == 2

        var groups = 0
        for (h, half) in halves.enumerated() {
            if half.isEmpty { continue }
            let parts = half.split(separator: ":", omittingEmptySubsequences: false)
            for (k, part) in parts.enumerated() {
                let isLast = h == halves.count - 1 && k == parts.count - 1
                if isLast && part.contains(".") {
                    guard parseIpv4(String(part)) != nil else { return false }
                    groups += 2
                } else {
                    guard (1...4).contains(part.count), part.allSatisfy({ $0.isASCII && $0.isHexDigit }) else {
                        return false
                    }
                    groups += 1
                }
            }
        }
        return compressed ? groups <= 7 : groups == 8
    }
}
