package org.playdrop.android.share

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.core.ClipboardPush

/** Which devices a clip can go to, and what one push to the chosen ones reports. */
class ClipPlanTest {
    private val images = setOf(SendKind.CAP_CLIPBOARD_IMAGE, SendKind.CAP_BLOBS)
    private val laptop = Candidate("aa", "calliope", "AAAA", online = true, clipboardOut = true, capabilities = images)
    private val pixel = Candidate("bb", "Pixel 9 Pro XL", "BBBB", online = true, clipboardOut = true)
    private val off = Candidate("cc", "desk", "CCCC", online = true, clipboardOut = false, capabilities = images)
    private val away = Candidate("dd", "attic", "DDDD", online = false, clipboardOut = true)
    private val awayAndOff = Candidate("ee", "garage", "EEEE", online = false, clipboardOut = false)

    @Test
    fun clipboardEligibility() {
        assertNull(SendKind.CLIPBOARD_TEXT.blocked(laptop))
        assertNull(SendKind.CLIPBOARD_TEXT.blocked(pixel))
        assertEquals(Blocked.CLIPBOARD_OFF, SendKind.CLIPBOARD_TEXT.blocked(off))
        assertEquals(Blocked.OFFLINE, SendKind.CLIPBOARD_TEXT.blocked(away))
        // The switch is the reason that coming online would not fix.
        assertEquals(Blocked.CLIPBOARD_OFF, SendKind.CLIPBOARD_TEXT.blocked(awayAndOff))
        assertNull(SendKind.CLIPBOARD_IMAGE.blocked(laptop))
        assertEquals(Blocked.NO_IMAGES, SendKind.CLIPBOARD_IMAGE.blocked(pixel))
    }

    @Test
    fun filesAndSharedTextEligibility() {
        listOf(laptop, pixel, off, away, awayAndOff).forEach { assertNull(SendKind.FILES.blocked(it)) }
        assertNull(SendKind.SHARED_TEXT.blocked(away))
        assertEquals(Blocked.CLIPBOARD_OFF, SendKind.SHARED_TEXT.blocked(off))
    }

    @Test
    fun oneEligibleDeviceIsSentToAtOnce() {
        assertEquals(Launch.Direct("aa"), Launch.forClipboard(listOf(laptop, off, away), null, SendKind.CLIPBOARD_TEXT))
    }

    @Test
    fun twoEligibleDevicesOpenTheChooser() {
        val launch = Launch.forClipboard(listOf(laptop, pixel, away), "bb", SendKind.CLIPBOARD_TEXT)
        assertTrue(launch is Launch.Choose)
        val s = (launch as Launch.Choose).selection
        assertEquals(setOf("bb"), s.ticked)
        assertEquals(listOf("aa", "bb"), s.eligible)
        assertEquals(Blocked.OFFLINE, s.rows.single { it.id == "dd" }.blocked)
    }

    @Test
    fun noEligibleDeviceGivesThePlainReason() {
        assertEquals(Launch.NoneEligible(ClipboardOutcome.NoPairedDevice), Launch.forClipboard(emptyList(), null, SendKind.CLIPBOARD_TEXT))
        assertEquals(Launch.NoneEligible(ClipboardOutcome.NoGrant), Launch.forClipboard(listOf(off), null, SendKind.CLIPBOARD_TEXT))
        assertEquals(Launch.NoneEligible(ClipboardOutcome.NoDeviceOnline), Launch.forClipboard(listOf(off, away), null, SendKind.CLIPBOARD_TEXT))
        assertEquals(Launch.NoneEligible(ClipboardOutcome.NoImageSupport), Launch.forClipboard(listOf(pixel), null, SendKind.CLIPBOARD_IMAGE))
    }

    @Test
    fun anyTickedSetIsOnePushToTheDevicesThatCanTakeIt() {
        val eligible = listOf("aa", "bb", "cc")
        assertEquals(listOf("aa", "cc"), ClipPlan.targets(listOf("aa", "cc"), eligible))
        assertEquals(listOf("bb"), ClipPlan.targets(listOf("bb", "dd"), eligible))
        assertEquals(emptyList<String>(), ClipPlan.targets(listOf("dd"), eligible))
        // Chosen order is kept, so the result toast names devices as the chooser listed them.
        assertEquals(listOf("cc", "aa"), ClipPlan.targets(listOf("cc", "aa"), eligible))
    }

    @Test
    fun theChooserAllowsAnySubset() {
        val third = laptop.copy(id = "ff", label = "den")
        val text = SheetState.Choose(Selection.of(listOf(laptop, pixel, third), null, SendKind.CLIPBOARD_TEXT).toggle("aa").toggle("bb"), SendKind.CLIPBOARD_TEXT, 1)
        assertTrue(text.canSend)
        val pics = SheetState.Choose(Selection.of(listOf(laptop, third), null, SendKind.CLIPBOARD_IMAGE).toggle("aa"), SendKind.CLIPBOARD_IMAGE, 1)
        assertTrue(pics.canSend)
        val none = SheetState.Choose(Selection.of(listOf(laptop, third), null, SendKind.CLIPBOARD_IMAGE), SendKind.CLIPBOARD_IMAGE, 1)
        assertFalse(none.canSend)
    }

    @Test
    fun resultsNameWhatHappenedPerDevice() {
        val labels = mapOf("aa" to "calliope", "bb" to "Pixel", "cc" to "desk", "dd" to "attic")
        val r = ClipPlan.results(
            chosen = listOf("aa", "bb", "cc", "dd"),
            label = { labels.getValue(it) },
            blockedNow = mapOf("cc" to Blocked.CLIPBOARD_OFF, "dd" to Blocked.OFFLINE),
            pushed = Pushed(sentTo = setOf("aa"), echo = false, skippedOffline = 1),
            error = null,
        )
        assertEquals(
            listOf(
                DeviceResult.Sent("calliope"),
                DeviceResult.Offline("Pixel"),
                DeviceResult.ClipboardOff("desk"),
                DeviceResult.Offline("attic"),
            ),
            r,
        )
    }

    @Test
    fun echoErrorAndNotPairedResults() {
        val echo = ClipPlan.results(listOf("aa"), { it }, emptyMap(), Pushed(emptySet(), echo = true), null)
        assertEquals(listOf(DeviceResult.AlreadySent("aa")), echo)
        val failed = ClipPlan.results(listOf("aa", "dd"), { it }, mapOf("dd" to Blocked.OFFLINE), null, "too large")
        assertEquals(listOf(DeviceResult.Failed("aa", "too large"), DeviceResult.Offline("dd")), failed)
        val gone = ClipPlan.results(listOf("aa", "bb"), { it }, emptyMap(), Pushed(setOf("aa"), echo = false, notPaired = setOf("bb")), null)
        assertEquals(listOf(DeviceResult.Sent("aa"), DeviceResult.NotPaired("bb")), gone)
        val nobody = ClipPlan.results(listOf("dd"), { it }, mapOf("dd" to Blocked.OFFLINE), null, null)
        assertEquals(listOf(DeviceResult.Offline("dd")), nobody)
    }

    @Test
    fun skippedDevicesGetAReasonOnlyWhenItIsTheOnlyOneCounted() {
        val chosen = listOf("aa", "bb", "cc")
        fun run(p: Pushed) = ClipPlan.results(chosen, { it }, emptyMap(), p, null)
        // One reason counted: it belongs to every skipped device.
        assertEquals(
            listOf(DeviceResult.Sent("aa"), DeviceResult.ClipboardOff("bb"), DeviceResult.ClipboardOff("cc")),
            run(Pushed(setOf("aa"), echo = false, skippedNoGrant = 2)),
        )
        assertEquals(DeviceResult.NoImages("bb"), run(Pushed(setOf("aa", "cc"), echo = false, skippedUnsupported = 1))[1])
        // Two reasons counted, or none: no device is called offline, the sentence stays neutral.
        assertEquals(
            listOf(DeviceResult.Sent("aa"), DeviceResult.NotTaken("bb"), DeviceResult.NotTaken("cc")),
            run(Pushed(setOf("aa"), echo = false, skippedOffline = 1, skippedNoGrant = 1)),
        )
        assertEquals(DeviceResult.NotTaken("bb"), run(Pushed(setOf("aa", "cc"), echo = false))[1])
    }

    @Test
    fun aDeviceThatRemovedThisPhoneIsReportedAsNotPaired() {
        val r = ClipPlan.results(listOf("aa"), { it }, mapOf("aa" to Blocked.REMOVED), null, null)
        assertEquals(listOf(DeviceResult.NotPaired("aa")), r)
    }

    /** The phone run: "Send clipboard" with two devices ticked; both got it. */
    @Test
    fun aClipDeliveredToBothIsReportedAsSentToBoth() {
        val one = "3f0c9a7e5b2d4c18a6e1f0b7d9c3a5e2" + "7b4d1c8e6a2f9b0d3c5e7a1f4b6d8c0e"
        val two = "a1b2c3d4e5f60718293a4b5c6d7e8f90" + "0f9e8d7c6b5a49382716f5e4d3c2b1a0"
        val candidates = listOf(
            Candidate(one, "test-one", "AAAA-1111", online = true, clipboardOut = true),
            Candidate(two, "test-two", "BBBB-2222", online = true, clipboardOut = true),
        )
        val labels = mapOf(one to "test-one", two to "test-two")
        fun send(chosen: List<String>): String {
            val blocked = ClipPlan.blockedNow(chosen, candidates, SendKind.CLIPBOARD_TEXT)
            val targets = ClipPlan.targets(chosen, blocked.filterValues { it == null }.keys)
            assertEquals(chosen, targets)
            // Exactly what push_clipboard_to returned: every target in sentTo, nothing skipped.
            val real = ClipboardPush(
                sentTo = targets,
                skippedEcho = false,
                skippedOffline = 0u,
                skippedNoGrant = 0u,
                skippedUnsupported = 0u,
                skippedNotPaired = emptyList(),
            )
            val results = ClipPlan.results(chosen, labels::getValue, blocked, Pushed.of(real), null)
            return SendReport.sentence(results, EnglishPhrases())
        }
        assertEquals("Sent to test-one and test-two.", send(listOf(one, two)))
        assertEquals("Sent to test-one.", send(listOf(one)))
    }

    @Test
    fun blockedNowKeepsEligibleDevicesApartFromMissingOnes() {
        val chosen = listOf("aa", "dd", "zz")
        val blocked = ClipPlan.blockedNow(chosen, listOf(laptop, away), SendKind.CLIPBOARD_TEXT)
        // Eligible is null, not offline; a device no longer listed is not paired.
        assertEquals(mapOf("aa" to null, "dd" to Blocked.OFFLINE, "zz" to Blocked.REMOVED), blocked)
    }

    @Test
    fun theCoresAnswerIsReadFieldByField() {
        val p = Pushed.of(ClipboardPush(listOf("aa"), true, 1u, 2u, 3u, listOf("zz")))
        assertEquals(Pushed(setOf("aa"), echo = true, notPaired = setOf("zz"), skippedOffline = 1, skippedNoGrant = 2, skippedUnsupported = 3), p)
    }
}
