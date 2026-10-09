package org.playdrop.android.core

import org.playdrop.core.Ipv4Interface
import org.playdrop.core.NetworkInterfaces
import java.net.Inet4Address
import java.net.NetworkInterface

/**
 * The phone's IPv4 interfaces for discovery. Android's `getifaddrs` is not reliable for apps,
 * so the core asks this list instead (docs/core-api.md section 7). Loopback, virtual and
 * point-to-point (VPN tunnel) interfaces are left out: discovery multicast does not cross them.
 */
internal class AndroidInterfaces : NetworkInterfaces {
    /** Every up, non-loopback, non-virtual interface's IPv4 addresses. */
    override fun ipv4Interfaces(): List<Ipv4Interface> {
        val out = mutableListOf<Ipv4Interface>()
        val all = try {
            NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
        } catch (e: Exception) {
            HostLog.w("net", "interface list failed", e)
            return emptyList()
        }
        for (ni in all) {
            val usable = try {
                ni.isUp && !ni.isLoopback && !ni.isVirtual && !ni.isPointToPoint && ni.index > 0
            } catch (e: Exception) {
                HostLog.w("net", "interface ${ni.name} unreadable", e)
                false
            }
            if (!usable) continue
            for (ia in ni.interfaceAddresses) {
                val addr = ia.address as? Inet4Address ?: continue
                val prefix = ia.networkPrefixLength.toInt()
                if (prefix !in 0..32) continue
                out += Ipv4Interface(
                    index = ni.index.toUInt(),
                    addr = addr.hostAddress ?: continue,
                    netmask = prefixToNetmask(prefix),
                )
            }
        }
        return out
    }

    companion object {
        /** Dotted netmask for an IPv4 prefix length (24 -> "255.255.255.0"). */
        fun prefixToNetmask(prefix: Int): String {
            require(prefix in 0..32) { "prefix $prefix out of range" }
            val mask = if (prefix == 0) 0L else (0xFFFFFFFFL shl (32 - prefix)) and 0xFFFFFFFFL
            return listOf(24, 16, 8, 0).joinToString(".") { ((mask shr it) and 0xFF).toString() }
        }
    }
}
