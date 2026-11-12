package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The one held descriptor set shared by every send of the items, and the descriptor budget. */
class SharedHoldTest {
    @Test
    fun closesOnceWhenTheLastSendLetsGo() {
        var closed = 0
        val hold = SharedHold { closed++ }
        // Three sends to three devices, then the preparer lets go.
        assertTrue(hold.retain(3))
        hold.release()
        assertEquals(0, closed)
        hold.release()
        hold.release()
        assertEquals(0, closed)
        assertFalse(hold.isClosed)
        hold.release()
        assertEquals(1, closed)
        assertTrue(hold.isClosed)
        // Extra releases and late retains change nothing.
        hold.release()
        assertFalse(hold.retain(1))
        assertEquals(1, closed)
    }

    @Test
    fun thePreparerAloneClosesItWhenNoSendTookIt() {
        var closed = 0
        val hold = SharedHold { closed++ }
        hold.release()
        assertEquals(1, closed)
    }

    @Test
    fun onlyItemsThatCannotBeReopenedCostMoreDescriptorsPerDevice() {
        assertEquals(0L, FdBudget.extraFor(notReopenable = 0, sends = 5))
        assertEquals(0L, FdBudget.extraFor(notReopenable = 5_000, sends = 1))
        assertEquals(5_000L, FdBudget.extraFor(notReopenable = 5_000, sends = 2))
        assertEquals(30_000L, FdBudget.extraFor(notReopenable = 10_000, sends = 4))
    }

    @Test
    fun theBudgetKeepsAReserveFree() {
        assertEquals(32_768 - 100 - FdBudget.RESERVE, FdBudget.free(32_768, 100))
        assertEquals(0, FdBudget.free(1_000, 100))
    }

    @Test
    fun readsTheSoftLimit() {
        val limits = """
            Limit                     Soft Limit           Hard Limit           Units
            Max processes             23005                23005                processes
            Max open files            32768                32768                files
        """.trimIndent()
        assertEquals(32_768, FdBudget.parseSoftLimit(limits))
        assertNull(FdBudget.parseSoftLimit("nothing here"))
    }
}
