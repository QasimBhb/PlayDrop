package org.playdrop.android.share

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The chooser's selection model and when it is shown at all. */
class SelectionTest {
    private val laptop = Candidate("aa", "calliope", "AAAA-1111", online = true, clipboardOut = true)
    private val pixel = Candidate("bb", "Pixel 9 Pro XL", "BBBB-2222", online = true, clipboardOut = false)
    private val desk = Candidate("cc", "desk", "CCCC-3333", online = false, clipboardOut = true)
    private val attic = Candidate("dd", "Attic", "DDDD-4444", online = false, clipboardOut = true)

    @Test
    fun rowsAreOnlineFirstThenByLabelIgnoringCase() {
        val s = Selection.of(listOf(desk, pixel, attic, laptop), null, SendKind.FILES)
        assertEquals(listOf("calliope", "Pixel 9 Pro XL", "Attic", "desk"), s.rows.map { it.label })
    }

    @Test
    fun defaultIsPreTickedAndMarked() {
        val s = Selection.of(listOf(laptop, pixel), "bb", SendKind.FILES)
        assertEquals(setOf("bb"), s.ticked)
        assertEquals("bb", s.defaultId)
        assertTrue(s.canSend)
        assertEquals(1, s.count)
    }

    @Test
    fun anOfflineDefaultIsStillPreTickedForFiles() {
        val s = Selection.of(listOf(laptop, desk), "cc", SendKind.FILES)
        assertEquals(setOf("cc"), s.ticked)
    }

    @Test
    fun noDefaultMeansNothingTickedAndSendDisabled() {
        val s = Selection.of(listOf(laptop, pixel), null, SendKind.FILES)
        assertTrue(s.ticked.isEmpty())
        assertFalse(s.canSend)
        assertEquals(0, s.count)
    }

    @Test
    fun aDefaultThatCannotReceiveIsNotTicked() {
        // Pixel has the clipboard switch off: the row is greyed out and nothing is ticked.
        val s = Selection.of(listOf(laptop, pixel, desk), "bb", SendKind.CLIPBOARD_TEXT)
        assertTrue(s.ticked.isEmpty())
        assertFalse(s.canSend)
        assertEquals("bb", s.defaultId)
        assertEquals(Blocked.CLIPBOARD_OFF, s.rows.single { it.id == "bb" }.blocked)
        assertEquals(Blocked.OFFLINE, s.rows.single { it.id == "cc" }.blocked)
    }

    @Test
    fun toggleTicksAndUnticksButIgnoresBlockedRows() {
        val s = Selection.of(listOf(laptop, pixel), null, SendKind.CLIPBOARD_TEXT)
        val ticked = s.toggle("aa")
        assertEquals(setOf("aa"), ticked.ticked)
        assertEquals(emptySet<String>(), ticked.toggle("aa").ticked)
        assertEquals(s, s.toggle("bb"))
        assertEquals(s, s.toggle("unknown"))
    }

    @Test
    fun selectAllTicksEveryTickableRowThenClears() {
        val s = Selection.of(listOf(laptop, pixel, desk, attic), null, SendKind.SHARED_TEXT)
        assertFalse(s.allTicked)
        val all = s.toggleAll()
        // Pixel's switch is off, so it stays unticked; offline rows can be ticked for shared text.
        assertEquals(setOf("aa", "cc", "dd"), all.ticked)
        assertTrue(all.allTicked)
        assertEquals(emptySet<String>(), all.toggleAll().ticked)
    }

    @Test
    fun selectAllAfterSomeTicksTicksTheRest() {
        val s = Selection.of(listOf(laptop, pixel, desk), "aa", SendKind.FILES)
        assertFalse(s.allTicked)
        assertEquals(setOf("aa", "bb", "cc"), s.toggleAll().ticked)
    }

    @Test
    fun selectAllHasNothingToDoWhenNoRowCanBeTicked() {
        val s = Selection.of(listOf(desk, attic), null, SendKind.CLIPBOARD_TEXT)
        assertFalse(s.canToggleAll)
        assertFalse(s.allTicked)
        assertEquals(s, s.toggleAll())
    }

    @Test
    fun chosenIsInRowOrder() {
        val s = Selection.of(listOf(desk, laptop, pixel), null, SendKind.FILES).toggle("cc").toggle("aa")
        assertEquals(listOf("aa", "cc"), s.chosen)
    }

    @Test
    fun makeDefaultMovesTheMarkerOnly() {
        val s = Selection.of(listOf(laptop, pixel), "aa", SendKind.FILES).withDefault("bb")
        assertEquals("bb", s.defaultId)
        assertEquals(setOf("aa"), s.ticked)
        assertEquals(1, s.rows.count { it.isDefault })
    }

    @Test
    fun shareWithOneDeviceSkipsTheChooser() {
        assertEquals(Launch.Direct("cc"), Launch.forShare(listOf(desk), null, SendKind.FILES))
        assertEquals(Launch.Direct("aa"), Launch.forShare(listOf(laptop), "aa", SendKind.SHARED_TEXT))
    }

    @Test
    fun shareWithTwoDevicesOpensTheChooser() {
        val launch = Launch.forShare(listOf(laptop, desk), "aa", SendKind.FILES)
        assertTrue(launch is Launch.Choose)
        assertEquals(setOf("aa"), (launch as Launch.Choose).selection.ticked)
    }

    @Test
    fun shareWithNoDevice() {
        assertEquals(Launch.NoDevices, Launch.forShare(emptyList(), null, SendKind.FILES))
    }

    @Test
    fun aDeviceThatRemovedThisPhoneIsNeverATarget() {
        val gone = laptop.copy(id = "ee", label = "gone", removed = true)
        // Never the one device: the other device is.
        assertEquals(Launch.Direct("cc"), Launch.forShare(listOf(gone, desk), null, SendKind.FILES))
        // Never pre-ticked, even as the default, and blocked for every kind.
        val launch = Launch.forShare(listOf(gone, desk, attic), "ee", SendKind.FILES) as Launch.Choose
        assertTrue(launch.selection.ticked.isEmpty())
        assertEquals("ee", launch.selection.defaultId)
        for (kind in SendKind.entries) assertEquals(Blocked.REMOVED, kind.blocked(gone))
        assertFalse(launch.selection.toggle("ee").ticked.contains("ee"))
        assertEquals(listOf("cc", "dd"), launch.selection.toggleAll().chosen.sorted())
        // When it is all there is, the chooser lists it greyed out with nothing to send.
        val alone = Launch.forShare(listOf(gone), "ee", SendKind.FILES) as Launch.Choose
        assertFalse(alone.selection.canSend)
        assertFalse(alone.selection.canToggleAll)
        // The clipboard action counts it as not paired.
        assertEquals(Launch.NoneEligible(ClipboardOutcome.NoPairedDevice), Launch.forClipboard(listOf(gone), null, SendKind.CLIPBOARD_TEXT))
        assertEquals(Launch.Direct("aa"), Launch.forClipboard(listOf(gone, laptop), null, SendKind.CLIPBOARD_TEXT))
    }

    @Test
    fun theFirstSinglePickBecomesTheDefault() {
        assertEquals("bb", DefaultRule.after(null, listOf("bb")))
        assertNull(DefaultRule.after(null, listOf("aa", "bb")))
        assertNull(DefaultRule.after("aa", listOf("bb")))
    }
}
