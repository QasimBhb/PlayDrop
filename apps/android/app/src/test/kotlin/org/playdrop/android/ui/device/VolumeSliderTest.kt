package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.core.MediaPlayers
import org.playdrop.android.ui.common.Samples
import org.playdrop.core.MediaAction

/** The computer volume slider's throttle and "what to show" rule, and the reserved entry. */
class VolumeSliderTest {
    @Test
    fun dragSendsAtMostOncePerInterval() {
        var s = VolumeSlider()
        val sent = mutableListOf<Int>()
        // A drag reporting a new value every 20 ms for one second.
        for (i in 0..50) {
            val step = s.drag(i, 1_000L + i * 20)
            s = step.state
            step.send?.let(sent::add)
        }
        // First value at once, then one per 200 ms: 0, 200, 400, 600, 800, 1000 ms.
        assertEquals(listOf(0, 10, 20, 30, 40, 50), sent)
    }

    @Test
    fun heldBackValueIsFlushedWhenTheIntervalEnds() {
        var s = VolumeSlider().drag(10, 0).state
        val held = s.drag(30, 50)
        assertNull(held.send)
        s = held.state
        assertEquals(200L, s.flushAtMs())
        assertNull(s.flush(150).send)
        val flushed = s.flush(200)
        assertEquals(30, flushed.send)
        assertNull(flushed.state.flushAtMs())
    }

    @Test
    fun releaseAlwaysSendsTheFinalValue() {
        var s = VolumeSlider().drag(10, 0).state
        s = s.drag(55, 30).state
        val released = s.release(60)
        // Within the throttle interval, but the final value is never dropped.
        assertEquals(55, released.send)
        assertNull(released.state.dragging)
        assertNull(released.state.flushAtMs())
    }

    @Test
    fun releaseDoesNotRepeatAValueAlreadySent() {
        val s = VolumeSlider().drag(40, 0).state
        assertNull(s.release(500).send)
        assertNull(VolumeSlider().release(0).send)
    }

    @Test
    fun fingerValueWinsWhileHeld() {
        var s = VolumeSlider().drag(80, 0).state
        s = s.incoming(20)
        assertEquals(80, s.shown(20))
        assertEquals(80, s.shown(35))
    }

    @Test
    fun releasedValueHoldsUntilTheEcho() {
        var s = VolumeSlider().drag(30, 0).state.drag(70, 300).state.release(400).state
        // A state sent before the command arrived must not pull the slider back.
        s = s.incoming(30)
        assertEquals(70, s.shown(30))
        s = s.incoming(70)
        assertEquals(70, s.shown(70))
        // After the echo the slider follows the computer again.
        assertEquals(15, s.shown(15))
    }

    @Test
    fun releasedValueGivesUpWithoutAnEcho() {
        var s = VolumeSlider().drag(90, 0).state.release(100).state
        assertEquals(2_100L, s.holdUntilMs())
        s = s.expire(2_000)
        assertEquals(90, s.shown(25))
        s = s.expire(2_100)
        assertEquals(25, s.shown(25))
        assertNull(s.holdUntilMs())
    }

    @Test
    fun valuesAreClampedAndIdleShowsIncoming() {
        assertEquals(42, VolumeSlider().shown(42))
        assertEquals(100, VolumeSlider().drag(130, 0).send)
        assertEquals(0, VolumeSlider().drag(-5, 0).send)
    }

    @Test
    fun systemEntryIsSplitFromRealPlayers() {
        val sys = Samples.computerVolume
        val players = listOf(Samples.song, sys)
        assertEquals(MediaPlayers.SYSTEM_VOLUME_ID, sys.player)
        assertTrue(MediaPlayers.isSystemVolume(sys))
        assertFalse(MediaPlayers.isSystemVolume(Samples.song))
        assertEquals(sys, MediaPlayers.systemVolume(players))
        assertEquals(listOf(Samples.song), MediaPlayers.realPlayers(players))
        assertNull(MediaPlayers.systemVolume(listOf(Samples.song)))
        assertFalse(MediaPlayers.isGone(sys))
        // The desktop drops the entry the same way as a closed player.
        val all = MediaPlayers.apply(MediaPlayers.apply(emptyMap(), "p", sys), "p", sys.copy(can = emptyList<MediaAction>()))
        assertTrue(all.isEmpty())
        assertTrue(Samples.detailState.media.none(MediaPlayers::isSystemVolume))
    }
}
