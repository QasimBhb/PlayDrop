package org.playdrop.android.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/** Notification IDs, action specs and the "received" tap decision. */
class NotificationPlanTest {
    @Test
    fun idsAreStableAndKindsDoNotCollide() {
        val a = NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, "t1")
        assertEquals(a, NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, "t1"))
        val all = NotificationPlan.Kind.entries.map { NotificationPlan.notificationId(it, "t1") }
        assertEquals(all.size, all.toSet().size)
        assertTrue(all.all { it >= 1_000 })
    }

    @Test
    fun dynamicIdsNeverHitFixedIds() {
        val fixed = setOf(NotificationPlan.SERVICE_ID, NotificationPlan.FIND_ID, NotificationPlan.START_FAILED_ID)
        repeat(1_000) { i ->
            NotificationPlan.Kind.entries.forEach { k ->
                assertTrue(NotificationPlan.notificationId(k, "key$i") !in fixed)
            }
        }
    }

    @Test
    fun offerActionsCarryTheTransferAndDistinctRequestCodes() {
        val nid = NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, "abc")
        val (accept, decline) = NotificationPlan.offerActions("abc", nid)
        assertEquals(NotificationPlan.ACTION_ACCEPT_OFFER, accept.action)
        assertEquals(NotificationPlan.ACTION_DECLINE_OFFER, decline.action)
        assertEquals("abc", accept.extras[NotificationPlan.EXTRA_TRANSFER_ID])
        assertEquals(nid.toString(), decline.extras[NotificationPlan.EXTRA_NOTIFICATION_ID])
        assertNotEquals(accept.requestCode, decline.requestCode)
        assertTrue(accept.requestCode > 0 && decline.requestCode > 0)
        assertNotEquals(accept.requestCode, NotificationPlan.stopFindAction().requestCode)
    }

    @Test
    fun requestCodesDifferAcrossOffers() {
        val n1 = NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, "one")
        val n2 = NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, "two")
        assertNotEquals(
            NotificationPlan.offerActions("one", n1)[0].requestCode,
            NotificationPlan.offerActions("two", n2)[0].requestCode,
        )
    }

    @Test
    fun oneSafeFileOpensItself() {
        val r = NotificationPlan.received(listOf(NotificationPlan.Delivered("dir/a.jpg", "content://x/1"))) { "image/jpeg" }
        assertEquals(NotificationPlan.Received.One("a.jpg", "content://x/1", "image/jpeg"), r)
    }

    @Test
    fun riskyFileOpensDownloadsInstead() {
        val r = NotificationPlan.received(listOf(NotificationPlan.Delivered("a.apk", "content://x/2"))) { null }
        assertEquals(NotificationPlan.Received.OneRisky("a.apk"), r)
    }

    @Test
    fun severalFilesShowACount() {
        val list = listOf(NotificationPlan.Delivered("a", "u1"), NotificationPlan.Delivered("b", "u2"))
        assertEquals(NotificationPlan.Received.Many(2), NotificationPlan.received(list) { null })
    }

    @Test
    fun humanBytes() {
        assertEquals("512 B", NotificationPlan.humanBytes(512))
        assertEquals("1.5 KiB", NotificationPlan.humanBytes(1536))
        assertEquals("500.0 MiB", NotificationPlan.humanBytes(500L * 1024 * 1024))
    }
}
