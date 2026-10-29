package org.playdrop.android.service

import android.content.Intent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** When the service starts on its own: after boot and after an update, once onboarding is done. */
class AutoStartTest {
    @Test
    fun theActionsAreThePlatformOnes() {
        assertEquals(Intent.ACTION_BOOT_COMPLETED, AutoStart.BOOT)
        assertEquals(Intent.ACTION_MY_PACKAGE_REPLACED, AutoStart.UPDATED)
    }

    @Test
    fun startsOnlyForTheExpectedActionAfterOnboarding() {
        for (expected in listOf(AutoStart.BOOT, AutoStart.UPDATED)) {
            assertTrue(AutoStart.shouldStart(expected, expected, onboarded = true))
            assertFalse("not onboarded", AutoStart.shouldStart(expected, expected, onboarded = false))
        }
        assertFalse("another action", AutoStart.shouldStart(AutoStart.UPDATED, AutoStart.BOOT, onboarded = true))
        assertFalse("another action", AutoStart.shouldStart(Intent.ACTION_PACKAGE_REPLACED, AutoStart.UPDATED, onboarded = true))
        assertFalse("no action", AutoStart.shouldStart(null, AutoStart.UPDATED, onboarded = true))
    }
}
