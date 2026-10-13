package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.core.MediaPlayers
import org.playdrop.android.ui.common.Samples
import org.playdrop.core.MediaAction

/** Media card position, buttons, the "gone" rule, and the remote input button. */
class NowPlayingTest {
    private val song = Samples.song
    private val at = song.positionAtMs!!.toLong()

    @Test
    fun positionIsExtrapolatedWhilePlaying() {
        assertEquals(61_200L, NowPlaying.positionMs(song, at))
        assertEquals(66_200L, NowPlaying.positionMs(song, at + 5_000))
        // A phone clock behind the desktop never moves it backwards.
        assertEquals(61_200L, NowPlaying.positionMs(song, at - 3_000))
        // Clamped to the track.
        assertEquals(215_000L, NowPlaying.positionMs(song, at + 1_000_000))
    }

    @Test
    fun pausedPositionStaysPut() {
        val paused = song.copy(playing = false)
        assertEquals(61_200L, NowPlaying.positionMs(paused, at + 60_000))
        assertEquals(61_200f / 215_000f, NowPlaying.fraction(paused, at)!!, 1e-6f)
        assertNull(NowPlaying.positionMs(song.copy(positionMs = null), at))
        assertNull(NowPlaying.fraction(song.copy(durationMs = null), at))
    }

    @Test
    fun toggleUsesWhatThePlayerOffers() {
        assertEquals(MediaAction.PLAY_PAUSE, NowPlaying.toggleAction(song))
        val separate = song.copy(can = listOf(MediaAction.PLAY, MediaAction.PAUSE))
        assertEquals(MediaAction.PAUSE, NowPlaying.toggleAction(separate))
        assertEquals(MediaAction.PLAY, NowPlaying.toggleAction(separate.copy(playing = false)))
        assertNull(NowPlaying.toggleAction(song.copy(can = listOf(MediaAction.NEXT))))
        assertEquals("1:01", NowPlaying.clock(61_200))
        assertEquals("1:00:05", NowPlaying.clock(3_605_000))
    }

    @Test
    fun goneRuleRemovesAPlayerAndThenThePeer() {
        val peer = "a1".repeat(32)
        val other = song.copy(player = "vlc", name = "VLC")
        var all = MediaPlayers.apply(emptyMap(), peer, song)
        all = MediaPlayers.apply(all, peer, other)
        assertEquals(listOf("spotify", "vlc"), all[peer]!!.map { it.player })
        // An update replaces in place and keeps the order.
        all = MediaPlayers.apply(all, peer, song.copy(title = "Next song"))
        assertEquals("Next song", all[peer]!![0].title)
        val gone = song.copy(playing = false, can = emptyList())
        assertTrue(MediaPlayers.isGone(gone))
        assertFalse(MediaPlayers.isGone(song.copy(playing = false)))
        all = MediaPlayers.apply(all, peer, gone)
        assertEquals(listOf("vlc"), all[peer]!!.map { it.player })
        all = MediaPlayers.apply(all, peer, other.copy(playing = false, can = emptyList()))
        assertFalse(peer in all)
        // A gone state for an unknown player adds nothing.
        assertTrue(MediaPlayers.apply(emptyMap(), peer, gone).isEmpty())
        assertTrue(MediaPlayers.forget(MediaPlayers.apply(emptyMap(), peer, song), peer).isEmpty())
    }

    @Test
    fun remoteInputButtonNeedsOnlineAndCapability() {
        val state = Samples.detailState
        assertTrue(state.canControl)
        assertFalse(state.copy(capabilities = listOf("media.control")).canControl)
        assertFalse(state.copy(row = state.row!!.copy(online = false)).canControl)
        assertTrue(state.copy(capabilities = listOf("input.key")).canControl)
    }
}
