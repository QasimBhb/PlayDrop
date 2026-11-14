package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.ui.common.Samples
import org.playdrop.core.MediaAction

/** The mute button's hold-and-expire rules and when it is offered. */
class MuteToggleTest {
    @Test
    fun tapShowsTheNewValueAtOnceAndSendsIt() {
        val step = MuteToggle().tap(incoming = false, nowMs = 100)
        assertEquals(true, step.send)
        assertTrue(step.state.shown(incoming = false))
    }

    @Test
    fun lateEchoDoesNotFlicker() {
        val s = MuteToggle().tap(false, 0).state
        // An older state arrives before the echo: still shows muted.
        assertTrue(s.incoming(false).shown(false))
        // The echo ends the hold.
        val done = s.incoming(true)
        assertNull(done.held)
        assertTrue(done.shown(true))
    }

    @Test
    fun withoutAnEchoTheHoldExpiresToTheReportedValue() {
        val s = MuteToggle().tap(false, 1_000).state
        assertEquals(3_000L, s.holdUntilMs())
        assertTrue(s.expire(2_999).shown(false))
        assertFalse(s.expire(3_000).shown(false))
    }

    @Test
    fun secondTapUnmutes() {
        val first = MuteToggle().tap(false, 0).state
        val second = first.tap(false, 10)
        assertEquals(false, second.send)
        assertFalse(second.state.shown(false))
    }

    @Test
    fun volumeAboveZeroShowsUnmutedOnlyWhileMuted() {
        val muted = MuteToggle().unmutedByVolume(incoming = true, nowMs = 5)
        assertFalse(muted.shown(true))
        val already = MuteToggle()
        assertEquals(already, already.unmutedByVolume(incoming = false, nowMs = 5))
    }

    @Test
    fun muteIsOfferedOnlyWhenTheDesktopListsIt() {
        assertFalse(MuteToggle.canMute(Samples.computerVolume))
        val newer = Samples.computerVolume.copy(can = listOf(MediaAction.VOLUME, MediaAction.MUTE), muted = true)
        assertTrue(MuteToggle.canMute(newer))
        assertTrue(MuteToggle.reportedMuted(newer))
        // A host that leaves `muted` out cannot be unmuted from here, so no button.
        assertFalse(MuteToggle.canMute(newer.copy(muted = null)))
        assertFalse(MuteToggle.reportedMuted(Samples.computerVolume))
    }
}
