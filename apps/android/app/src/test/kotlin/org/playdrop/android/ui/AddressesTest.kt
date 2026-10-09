package org.playdrop.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.ui.adddevice.Candidates
import org.playdrop.android.ui.common.Addresses
import org.playdrop.core.AddDeviceCandidate

/** Typed addresses, address ranking and the candidate collapse. */
class AddressesTest {
    @Test
    fun normalizeAddsTheDefaultPort() {
        assertEquals("192.168.1.20:42187", Addresses.normalize("192.168.1.20"))
        assertEquals("192.168.1.20:5000", Addresses.normalize(" 192.168.1.20:5000 "))
        assertEquals("[fe80::1]:42187", Addresses.normalize("fe80::1"))
        assertEquals("[fe80::1]:9", Addresses.normalize("[fe80::1]:9"))
        assertEquals("[fe80::1]:42187", Addresses.normalize("[fe80::1]"))
    }

    @Test
    fun normalizeRejectsNonsense() {
        assertNull(Addresses.normalize(""))
        assertNull(Addresses.normalize("laptop.local"))
        assertNull(Addresses.normalize("300.1.1.1"))
        assertNull(Addresses.normalize("1.2.3.4:0"))
        assertNull(Addresses.normalize("1.2.3.4:70000"))
        assertNull(Addresses.normalize("[::1]x"))
    }

    @Test
    fun privateIpv4RanksFirst() {
        assertEquals(0, Addresses.rank("192.168.1.20:42187"))
        assertEquals(0, Addresses.rank("10.0.0.5:42187"))
        assertEquals(0, Addresses.rank("172.20.0.1:42187"))
        assertEquals(1, Addresses.rank("8.8.8.8:42187"))
        assertEquals(1, Addresses.rank("[::ffff:192.168.1.2]:42187"))
        assertEquals(2, Addresses.rank("[2001:db8::1]:42187"))
        assertEquals(3, Addresses.rank("[fe80::1]:42187"))
    }

    @Test
    fun sameSubnet() {
        assertTrue(Addresses.sameSubnet("192.168.1.31:42187", "192.168.1.40", "255.255.255.0"))
        assertFalse(Addresses.sameSubnet("192.168.2.31:42187", "192.168.1.40", "255.255.255.0"))
        assertFalse(Addresses.sameSubnet("[fe80::1]:42187", "192.168.1.40", "255.255.255.0"))
    }

    @Test
    fun candidatesCollapseToOneRowPerDeviceWithTheBestAddress() {
        val list = listOf(
            AddDeviceCandidate("laptop", "laptop", "[fe80::1]:42187"),
            AddDeviceCandidate("laptop", "laptop", "172.17.0.1:42187"),
            AddDeviceCandidate("pc", "desktop", "192.168.1.31:42187"),
            AddDeviceCandidate("laptop", "laptop", "[2001:db8::2]:42187"),
            AddDeviceCandidate("laptop", "laptop", "192.168.1.20:42187"),
            AddDeviceCandidate("laptop", "phone", "192.168.1.50:42187"),
        )
        val out = Candidates.collapse(list)
        assertEquals(3, out.size)
        assertEquals("laptop", out[0].name)
        // Both are private IPv4; the first one seen stays.
        assertEquals("172.17.0.1:42187", out[0].addr)
        assertEquals("pc", out[1].name)
        assertEquals("phone", out[2].deviceType)
    }

    @Test
    fun ipv4ReplacesLinkLocal() {
        val out = Candidates.collapse(
            listOf(
                AddDeviceCandidate("a", "laptop", "[fe80::1]:42187"),
                AddDeviceCandidate("a", "laptop", "[2001:db8::1]:42187"),
                AddDeviceCandidate("a", "laptop", "192.168.0.9:42187"),
            ),
        )
        assertEquals(listOf("192.168.0.9:42187"), out.map { it.addr })
    }
}
