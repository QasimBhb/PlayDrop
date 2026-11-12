package org.playdrop.android.service

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Opening the app brings back a swiped-away service notification; other callers start only a stopped service. */
class ServiceStartTest {
    @Test
    fun aStoppedServiceIsAlwaysStarted() {
        assertTrue(ServiceStart.needed(running = false, repost = false))
        assertTrue(ServiceStart.needed(running = false, repost = true))
    }

    @Test
    fun aRunningServiceIsStartedAgainOnlyToRepost() {
        assertFalse(ServiceStart.needed(running = true, repost = false))
        assertTrue(ServiceStart.needed(running = true, repost = true))
    }
}
