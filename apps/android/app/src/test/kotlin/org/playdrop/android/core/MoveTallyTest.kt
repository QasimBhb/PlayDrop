package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** Bookkeeping of a mover pass and its progress throttle. */
class MoveTallyTest {
    @Test
    fun countsEveryOutcome() {
        val t = MoveTally(5)
        t.record(MoveOutcome.COPIED)
        t.record(MoveOutcome.COPIED, renumbered = true)
        t.record(MoveOutcome.GONE)
        t.record(MoveOutcome.FAILED, renumbered = true) // a failed file is not counted as renumbered
        assertEquals(4, t.done)
        assertEquals(2, t.moved)
        assertEquals(1, t.failed)
        assertEquals(1, t.gone)
        assertEquals(1, t.renumbered)
        assertEquals("4 of 5 files: 2 copied, 1 already gone, 1 failed, 1 renumbered", t.line())
    }

    @Test
    fun stoppedPassCountsTheRestAsNotMoved() {
        val t = MoveTally(5)
        t.record(MoveResult(MoveOutcome.COPIED))
        t.record(MoveResult(MoveOutcome.FAILED, stopPass = true))
        t.skipRest()
        assertEquals(5, t.done)
        assertEquals(1, t.moved)
        assertEquals(4, t.failed)
        t.skipRest() // nothing left to count
        assertEquals(4, t.failed)
    }

    @Test
    fun retriesBackOffThenStop() {
        assertEquals(60_000L, MoveRetry.delayMs(0))
        assertEquals(5 * 60_000L, MoveRetry.delayMs(1))
        assertEquals(6 * 3_600_000L, MoveRetry.delayMs(4))
        assertEquals(null, MoveRetry.delayMs(5))
        var prev = 0L
        for (i in 0..4) {
            val d = MoveRetry.delayMs(i)!!
            assertTrue(d > prev)
            prev = d
        }
    }

    @Test
    fun secondPassOverAMovedFolderMovesNothing() {
        val t = MoveTally(3)
        repeat(3) { t.record(MoveOutcome.GONE) }
        assertEquals(0, t.moved)
        assertEquals(3, t.done)
    }

    @Test
    fun throttleLetsEdgesThroughAndLimitsTheRest() {
        val p = ProgressThrottle(intervalNanos = 100)
        assertTrue(p.due(1, 10, 0))
        assertFalse(p.due(2, 10, 50))
        assertFalse(p.due(3, 10, 99))
        assertTrue(p.due(4, 10, 100))
        assertFalse(p.due(5, 10, 150))
        assertTrue(p.due(10, 10, 151)) // the last file always
    }
}
