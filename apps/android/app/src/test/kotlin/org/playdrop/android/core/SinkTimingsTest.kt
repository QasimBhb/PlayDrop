package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Per-transfer sink timing summary. */
class SinkTimingsTest {
    @Test
    fun summarisesStepsFilesAndSpan() {
        val t = SinkTimings()
        t.add("a", "create", 1_000_000)
        t.add("a", "copy", 3_000_000)
        t.fileDone("a", 100, start = 10, end = 5_000_010)
        t.add("a", "create", 1_000_000)
        t.fileDone("a", 50, start = 6_000_000, end = 8_000_000)
        val s = t.finish("a")!!
        assertEquals(2, s.files)
        assertEquals(150L, s.bytes)
        assertEquals(7_999_990L, s.spanNanos)
        assertEquals(mapOf("create" to 2_000_000L, "copy" to 3_000_000L), s.stepNanos)
        assertEquals(5_000_000L, s.totalNanos)
        val line = s.line()
        assertTrue(line, line.startsWith("2 files, 150 bytes, first to last delivery 8.0 ms, sink 5.0 ms (2.500 ms/file)"))
        assertTrue(line, line.contains("create 2.0 ms (1.000 ms/file)"))
    }

    @Test
    fun finishForgetsTheTransfer() {
        val t = SinkTimings()
        t.fileDone("a", 1, 0, 1)
        t.finish("a")
        assertNull(t.finish("a"))
        assertNull(t.finish("never"))
    }

    @Test
    fun transfersAreKeptApart() {
        val t = SinkTimings()
        t.add("a", "copy", 5)
        t.add("b", "copy", 7)
        assertEquals(5L, t.finish("a")!!.stepNanos["copy"])
        assertEquals(7L, t.finish("b")!!.stepNanos["copy"])
    }
}
