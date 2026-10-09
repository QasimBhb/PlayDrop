package org.playdrop.android.ui.common

/**
 * Socket address helpers on plain strings, so they never trigger a DNS lookup and run in
 * JVM unit tests. The core reports addresses as `ip:port` (IPv6 as `[ip]:port`).
 */
object Addresses {
    /** The port PlayDrop listens on unless the user changed it (protocol.md section 1). */
    const val DEFAULT_PORT = 42187

    /** The host part of `ip:port` or `[ip]:port`, without brackets. */
    fun host(addr: String): String {
        val a = addr.trim()
        if (a.startsWith("[")) {
            val end = a.indexOf(']')
            return if (end > 0) a.substring(1, end) else a.removePrefix("[")
        }
        val colons = a.count { it == ':' }
        return if (colons == 1) a.substringBefore(':') else a
    }

    /** Four octets of a dotted IPv4 literal, or null when [text] is not one. */
    fun parseIpv4(text: String): IntArray? {
        val parts = text.split('.')
        if (parts.size != 4) return null
        val out = IntArray(4)
        for ((i, p) in parts.withIndex()) {
            if (p.isEmpty() || p.length > 3 || !p.all { it.isDigit() }) return null
            val v = p.toInt()
            if (v > 255) return null
            out[i] = v
        }
        return out
    }

    /** True for 10/8, 172.16/12 and 192.168/16. */
    fun isPrivateV4(o: IntArray): Boolean =
        o[0] == 10 || (o[0] == 172 && o[1] in 16..31) || (o[0] == 192 && o[1] == 168)

    /**
     * Lower is better for dialling by hand, as on the desktop: private IPv4, then other
     * IPv4 (or IPv4-mapped IPv6), then global IPv6, then link-local IPv6.
     */
    fun rank(addr: String): Int {
        val h = host(addr)
        parseIpv4(h)?.let { return if (isPrivateV4(it)) 0 else 1 }
        val lower = h.lowercase()
        return when {
            lower.startsWith("::ffff:") && parseIpv4(lower.removePrefix("::ffff:")) != null -> 1
            lower.startsWith("fe8") || lower.startsWith("fe9") ||
                lower.startsWith("fea") || lower.startsWith("feb") -> 3
            else -> 2
        }
    }

    /**
     * Turns what the user typed into `ip:port` for `pairWith`, adding the default port when
     * none is given. Accepts `1.2.3.4`, `1.2.3.4:42187`, `[fe80::1]:42187` and a bare IPv6
     * literal. Returns null when the text is not an address.
     */
    fun normalize(input: String): String? {
        val t = input.trim()
        if (t.isEmpty()) return null
        if (t.startsWith("[")) {
            val end = t.indexOf(']')
            if (end <= 1) return null
            val ip = t.substring(1, end)
            if (!ip.contains(':')) return null
            val rest = t.substring(end + 1)
            val port = when {
                rest.isEmpty() -> DEFAULT_PORT
                rest.startsWith(":") -> parsePort(rest.substring(1)) ?: return null
                else -> return null
            }
            return "[$ip]:$port"
        }
        val colons = t.count { it == ':' }
        return when {
            colons == 0 -> parseIpv4(t)?.let { "$t:$DEFAULT_PORT" }
            colons == 1 -> {
                val ip = t.substringBefore(':')
                val port = parsePort(t.substringAfter(':')) ?: return null
                parseIpv4(ip)?.let { "$ip:$port" }
            }
            t.all { it.isLetterOrDigit() || it == ':' || it == '.' } -> "[$t]:$DEFAULT_PORT"
            else -> null
        }
    }

    /** A TCP port in 1..65535, or null. */
    fun parsePort(text: String): Int? {
        if (text.isEmpty() || text.length > 5 || !text.all { it.isDigit() }) return null
        return text.toInt().takeIf { it in 1..65535 }
    }

    /** True when [addr] (`ip:port`) is IPv4 and on the same subnet as [ifaceAddr]/[netmask]. */
    fun sameSubnet(addr: String, ifaceAddr: String, netmask: String): Boolean {
        val a = parseIpv4(host(addr)) ?: return false
        val i = parseIpv4(ifaceAddr) ?: return false
        val m = parseIpv4(netmask) ?: return false
        return (0 until 4).all { (a[it] and m[it]) == (i[it] and m[it]) }
    }
}
