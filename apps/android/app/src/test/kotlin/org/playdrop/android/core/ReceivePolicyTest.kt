package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** Sync threshold and media-scan batching of the receive sink. */
class ReceivePolicyTest {
    @Test
    fun onlyBigFilesAreSyncedOneByOne() {
        assertFalse(ReceivePolicy.syncAfterCopy(0))
        assertFalse(ReceivePolicy.syncAfterCopy(100L * 1024))
        assertFalse(ReceivePolicy.syncAfterCopy(ReceivePolicy.SYNC_MIN_BYTES - 1))
        assertTrue(ReceivePolicy.syncAfterCopy(ReceivePolicy.SYNC_MIN_BYTES))
        assertTrue(ReceivePolicy.syncAfterCopy(1L shl 30))
    }

    @Test
    fun fullBatchComesOutAtTheLimit() {
        val b = ScanBatcher(limit = 3)
        assertNull(b.add("t", "a"))
        assertNull(b.add("t", "b"))
        assertEquals(listOf("a", "b", "c"), b.add("t", "c"))
        assertNull(b.add("t", "d"))
        assertEquals(listOf("d"), b.drain("t"))
        assertEquals(emptyList<String>(), b.drain("t"))
    }

    @Test
    fun transfersAreBatchedSeparately() {
        val b = ScanBatcher(limit = 2)
        assertNull(b.add("t1", "a"))
        assertNull(b.add("t2", "x"))
        assertEquals(listOf("a", "b"), b.add("t1", "b"))
        assertEquals(listOf("x"), b.drain("t2"))
        assertEquals(emptyList<String>(), b.drain("t1"))
    }

    @Test
    fun drainOfUnknownTransferIsEmpty() {
        assertEquals(emptyList<String>(), ScanBatcher().drain("nope"))
    }

    @Test(expected = IllegalArgumentException::class)
    fun zeroLimitIsRejected() {
        ScanBatcher(limit = 0)
    }

    @Test
    fun defaultBatchIsFiveHundred() {
        val b = ScanBatcher()
        repeat(ReceivePolicy.SCAN_BATCH - 1) { assertNull(b.add("t", "p$it")) }
        assertEquals(ReceivePolicy.SCAN_BATCH, b.add("t", "last")!!.size)
    }
}
