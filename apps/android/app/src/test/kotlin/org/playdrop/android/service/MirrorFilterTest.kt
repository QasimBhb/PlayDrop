package org.playdrop.android.service

import android.app.Notification
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Test

/** What the phone may mirror (SR-41). */
class MirrorFilterTest {
    private val own = "org.playdrop.android"
    private val chat = "org.example.chat"
    private val allowed = setOf(chat)

    private fun candidate(
        pkg: String = chat,
        visibility: Int = MirrorFilter.VISIBILITY_PUBLIC,
        title: String? = "Alex",
        text: String? = "On my way",
    ) = MirrorFilter.Candidate(
        packageName = pkg,
        otherUser = false,
        ongoing = false,
        foregroundService = false,
        groupSummary = false,
        visibility = visibility,
        title = title,
        text = text,
    )

    private fun decide(c: MirrorFilter.Candidate) = MirrorFilter.decide(c, own, allowed, "Chat")

    @Test
    fun visibilityConstantsAreThePlatformOnes() {
        assertEquals(Notification.VISIBILITY_PUBLIC, MirrorFilter.VISIBILITY_PUBLIC)
        assertEquals(Notification.VISIBILITY_PRIVATE, MirrorFilter.VISIBILITY_PRIVATE)
        assertEquals(Notification.VISIBILITY_SECRET, MirrorFilter.VISIBILITY_SECRET)
    }

    @Test
    fun anAllowedPublicNotificationGoesWithTitleAndText() {
        assertEquals(MirrorFilter.Outgoing("Chat", "Alex", "On my way"), decide(candidate()))
    }

    @Test
    fun neverPlayDropItselfEvenWhenListed() {
        val c = candidate(pkg = own)
        assertNull(MirrorFilter.decide(c, own, setOf(own, chat), "PlayDrop"))
    }

    @Test
    fun onlyAppsOnTheAllowList() {
        assertNull(decide(candidate(pkg = "org.example.other")))
        assertNull(MirrorFilter.decide(candidate(), own, emptySet(), "Chat"))
    }

    @Test
    fun ongoingForegroundServiceSummariesAndOtherUsersStay() {
        assertNull(decide(candidate().copy(ongoing = true)))
        assertNull(decide(candidate().copy(foregroundService = true)))
        assertNull(decide(candidate().copy(groupSummary = true)))
        assertNull(decide(candidate().copy(otherUser = true)))
    }

    @Test
    fun secretNeverLeaves() {
        assertNull(decide(candidate(visibility = MirrorFilter.VISIBILITY_SECRET)))
    }

    @Test
    fun privateGoesWithItsRealTitleAndText() {
        assertEquals(
            MirrorFilter.Outgoing("Chat", "Alex", "On my way"),
            decide(candidate(visibility = MirrorFilter.VISIBILITY_PRIVATE)),
        )
    }

    @Test
    fun emptyTitleAndTextIsSkipped() {
        assertNull(decide(candidate(title = null, text = null)))
        assertNull(decide(candidate(title = "  ", text = "")))
        assertEquals(MirrorFilter.Outgoing("Chat", "", "Hi"), decide(candidate(title = null, text = "Hi")))
    }

    @Test
    fun aMissingLabelFallsBackToThePackage() {
        assertEquals(chat, MirrorFilter.decide(candidate(), own, allowed, " ")?.app)
    }

    @Test
    fun resyncTakesTheNewestAndSendsThemOldestFirst() {
        val times = (1L..30L).toList().shuffled()
        val picked = MirrorFilter.forResync(times, { it })
        assertEquals((11L..30L).toList(), picked)
        assertEquals(listOf(1L, 2L), MirrorFilter.forResync(listOf(2L, 1L), { it }))
    }

    @Test
    fun contentHashFollowsTheContent() {
        val a = MirrorFilter.Outgoing("Chat", "Alex", "On my way")
        assertEquals(a.contentHash(), a.copy().contentHash())
        assertNotEquals(a.contentHash(), a.copy(text = "On my way!").contentHash())
        assertNotEquals(a.contentHash(), a.copy(title = "Alex", text = "").contentHash())
        assertNotEquals(
            "fields do not run together",
            MirrorFilter.Outgoing("Chat", "ab", "c").contentHash(),
            MirrorFilter.Outgoing("Chat", "a", "bc").contentHash(),
        )
    }
}
