package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

/** Prefix length to dotted netmask. */
class NetmaskTest {
    @Test
    fun commonPrefixes() {
        assertEquals("0.0.0.0", AndroidInterfaces.prefixToNetmask(0))
        assertEquals("255.0.0.0", AndroidInterfaces.prefixToNetmask(8))
        assertEquals("255.255.0.0", AndroidInterfaces.prefixToNetmask(16))
        assertEquals("255.255.255.0", AndroidInterfaces.prefixToNetmask(24))
        assertEquals("255.255.252.0", AndroidInterfaces.prefixToNetmask(22))
        assertEquals("255.255.255.255", AndroidInterfaces.prefixToNetmask(32))
    }

    @Test
    fun outOfRangeIsRefused() {
        assertThrows(IllegalArgumentException::class.java) { AndroidInterfaces.prefixToNetmask(33) }
        assertThrows(IllegalArgumentException::class.java) { AndroidInterfaces.prefixToNetmask(-1) }
    }
}
