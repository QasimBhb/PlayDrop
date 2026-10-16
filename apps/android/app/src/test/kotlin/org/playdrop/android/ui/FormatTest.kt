package org.playdrop.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.ui.common.Ago
import org.playdrop.android.ui.common.Format

/** Number and time formatting matches the desktop window. */
class FormatTest {
    @Test
    fun bytesUseBinaryUnitsWithOneDecimal() {
        assertEquals("0 B", Format.bytes(0))
        assertEquals("512 B", Format.bytes(512))
        assertEquals("1.5 KiB", Format.bytes(1536))
        assertEquals("12.0 MiB", Format.bytes(12L * 1024 * 1024))
        assertEquals("2.0 GiB", Format.bytes(2L * 1024 * 1024 * 1024))
        assertEquals("30.0 MiB/s", Format.rate(30L * 1024 * 1024))
    }

    @Test
    fun durationsAndCountdown() {
        assertEquals("<1 s", Format.duration(0))
        assertEquals("45 s", Format.duration(45))
        assertEquals("2 min", Format.duration(120))
        assertEquals("2 min 5 s", Format.duration(125))
        assertEquals("1 h 3 min", Format.duration(3780))
        assertEquals("1:45", Format.countdown(105))
        assertEquals("0:00", Format.countdown(-3))
    }

    @Test
    fun etaRoundsUpAndNeedsARate() {
        assertNull(Format.etaSeconds(100, 0))
        assertEquals(3L, Format.etaSeconds(250, 100))
        assertEquals(0L, Format.etaSeconds(-5, 100))
    }

    @Test
    fun fractionIsClamped() {
        assertEquals(0f, Format.fraction(10, 0))
        assertEquals(0.5f, Format.fraction(50, 100))
        assertEquals(1f, Format.fraction(150, 100))
    }

    @Test
    fun sasIsGroupedForComparison() {
        assertEquals("482 913", Format.sas("482913"))
        assertEquals("12", Format.sas("12"))
    }

    @Test
    fun agoBuckets() {
        assertEquals(Ago.JustNow, Ago.of(100, 130))
        assertEquals(Ago.Minutes(5), Ago.of(0, 300))
        assertEquals(Ago.Hours(2), Ago.of(0, 7200))
        assertEquals(Ago.Days(1), Ago.of(0, 90_000))
        assertEquals(Ago.Days(3), Ago.of(0, 3 * 86_400))
        assertEquals(Ago.Never, Ago.of(null, 10))
        assertEquals(Ago.JustNow, Ago.of(200, 100))
    }

    @Test
    fun countsAreGroupedForTheRow() {
        assertEquals("1,204", Format.count(1204, java.util.Locale.US))
        assertEquals("5,000", Format.count(5000, java.util.Locale.US))
        assertEquals("0", Format.count(-3, java.util.Locale.US))
    }

    @Test
    fun filesDoneNeedsSeveralFilesAndAReportedCount() {
        assertTrue(Format.showFilesDone(1204, 5000))
        assertFalse(Format.showFilesDone(0, 5000))
        assertFalse(Format.showFilesDone(1, 1))
        assertFalse(Format.showFilesDone(0, 0))
    }
}
