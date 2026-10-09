package org.playdrop.android.ui.diagnostics

import org.playdrop.android.ui.common.Addresses
import org.playdrop.core.Diagnostics
import org.playdrop.core.DiscoveryPath

/**
 * One plain-language finding, most important first, built like the desktop daemon's
 * `probe::summary`: it names the layer that failed. The screen words each one from
 * resources. A phone cannot probe its own port from the LAN, so the port line comes only
 * from the listener state.
 */
sealed class DiagIssue {
    /** The listener is down: the port is held or the bind failed. */
    data class NotListening(val port: Int) : DiagIssue()

    /** No usable network interface (Wi-Fi off). */
    data object NoInterface : DiagIssue()

    /** Joining the multicast group failed on an interface. */
    data class MulticastJoinFailed(val addr: String, val error: String) : DiagIssue()

    /** No beacon heard although a peer is connected on that network: multicast is filtered. */
    data class MulticastBlocked(val ifaceAddr: String, val peerLabel: String) : DiagIssue()

    /** Both discovery paths are off. */
    data object DiscoveryOff : DiagIssue()

    /** mDNS registration or browsing failed. */
    data class MdnsFailed(val error: String) : DiagIssue()

    /** An offline peer was last seen on a network this phone is not on. */
    data class OtherNetwork(val peerLabel: String) : DiagIssue()

    /** An offline peer whose last dial failed: the path and the reason. */
    data class PeerNotReached(val peerLabel: String, val path: DiscoveryPath, val failure: String) : DiagIssue()

    /** Nothing is wrong. */
    data object AllFine : DiagIssue()
}

/** Builds the summary list from a `diagnose()` snapshot. */
object DiagnosticsSummary {
    /** The findings; exactly `[AllFine]` when nothing is wrong. */
    fun of(d: Diagnostics): List<DiagIssue> {
        val out = ArrayList<DiagIssue>()
        if (!d.bindOk) out += DiagIssue.NotListening(d.port.toInt())
        if (d.interfaces.isEmpty()) out += DiagIssue.NoInterface

        if (d.multicast.enabled) {
            d.multicast.joinFailed.forEach { out += splitJoinFailure(it) }
            if (!d.multicast.beaconRecently) {
                for (i in d.interfaces) {
                    val p = d.peers.firstOrNull { p ->
                        p.online && !p.beaconSeen &&
                            p.lastKnown.any { Addresses.sameSubnet(it, i.addr, i.netmask) }
                    }
                    if (p != null) out += DiagIssue.MulticastBlocked(i.addr, p.label)
                }
            }
        } else if (!d.mdns.enabled) {
            out += DiagIssue.DiscoveryOff
        }
        if (d.mdns.enabled) d.mdns.error?.let { out += DiagIssue.MdnsFailed(it) }

        for (p in d.peers.filter { !it.online }) {
            val reachableNet = p.lastKnown.isEmpty() || p.lastKnown.any { a ->
                d.interfaces.any { Addresses.sameSubnet(a, it.addr, it.netmask) }
            }
            if (!reachableNet && d.interfaces.isNotEmpty()) {
                out += DiagIssue.OtherNetwork(p.label)
                continue
            }
            p.lastResult?.let { out += DiagIssue.PeerNotReached(p.label, it.path, it.failure) }
        }
        if (out.isEmpty()) out += DiagIssue.AllFine
        return out
    }

    /** The core reports a failed join as `addr: error` text. */
    private fun splitJoinFailure(text: String): DiagIssue.MulticastJoinFailed {
        val i = text.indexOf(": ")
        return if (i > 0) {
            DiagIssue.MulticastJoinFailed(text.substring(0, i), text.substring(i + 2))
        } else {
            DiagIssue.MulticastJoinFailed(text, "")
        }
    }
}
