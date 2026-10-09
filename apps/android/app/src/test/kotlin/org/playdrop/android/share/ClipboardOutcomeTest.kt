package org.playdrop.android.share

import org.junit.Assert.assertEquals
import org.junit.Test

/** The toast a manual clipboard push ends with. */
class ClipboardOutcomeTest {
    @Test
    fun sentWins() {
        assertEquals(ClipboardOutcome.Sent(2), ClipboardOutcome.of(2, false, 1, 1, 4))
    }

    @Test
    fun echo() {
        assertEquals(ClipboardOutcome.AlreadySent, ClipboardOutcome.of(0, true, 0, 0, 1))
    }

    @Test
    fun noPairedDevice() {
        assertEquals(ClipboardOutcome.NoPairedDevice, ClipboardOutcome.of(0, false, 0, 0, 0))
    }

    @Test
    fun onlineButNoGrant() {
        assertEquals(ClipboardOutcome.NoGrant, ClipboardOutcome.of(0, false, 0, 2, 2))
    }

    @Test
    fun offline() {
        assertEquals(ClipboardOutcome.NoDeviceOnline, ClipboardOutcome.of(0, false, 2, 0, 2))
        assertEquals(ClipboardOutcome.NoDeviceOnline, ClipboardOutcome.of(0, false, 1, 1, 2))
    }
}
