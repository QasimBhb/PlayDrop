package org.playdrop.android.core

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.core.TransferStatus

/** When a transfer lands first, and what a failed rename does to its route. */
class LandingPolicyTest {
    private val mib = 1024L * 1024

    @Test
    fun manySmallFilesLand() {
        // The Phase 6 bench set: 5,000 files of 4 to 64 KiB.
        assertTrue(LandingPolicy.shouldLand(5_000, 5_000L * 34 * 1024))
        assertTrue(LandingPolicy.shouldLand(LandingPolicy.MIN_FILES, LandingPolicy.MIN_FILES))
    }

    @Test
    fun fewFilesGoStraightToDownloads() {
        assertFalse(LandingPolicy.shouldLand(1, 1))
        assertFalse(LandingPolicy.shouldLand(LandingPolicy.MIN_FILES - 1, 1024))
        assertFalse(LandingPolicy.shouldLand(0, 0))
    }

    @Test
    fun bigMeanGoesStraightToDownloads() {
        // 100 medium files at 5 MiB, and one 1 GiB file among 63 tiny ones.
        assertFalse(LandingPolicy.shouldLand(100, 100 * 5 * mib))
        assertFalse(LandingPolicy.shouldLand(64, 1024 * mib + 63))
    }

    @Test
    fun meanThresholdIsStrict() {
        val n = 100L
        assertFalse(LandingPolicy.shouldLand(n, n * LandingPolicy.MAX_MEAN_BYTES))
        assertTrue(LandingPolicy.shouldLand(n, n * LandingPolicy.MAX_MEAN_BYTES - 1))
        assertFalse(LandingPolicy.shouldLand(n, -1))
    }

    @Test
    fun hugeCountsDoNotOverflow() {
        assertTrue(LandingPolicy.shouldLand(1_000_000, 1_000_000L * 1024))
    }

    /** A single file's integrity failure leaves the transfer active: no end-of-transfer move. */
    @Test
    fun onlyFinalStatesEndATransfer() {
        assertFalse(TransferEnds.isTerminal(TransferStatus.ACTIVE))
        assertFalse(TransferEnds.isTerminal(TransferStatus.OFFERED))
        assertTrue(TransferEnds.isTerminal(TransferStatus.FAILED))
        assertTrue(TransferEnds.isTerminal(TransferStatus.CANCELLED))
        assertTrue(TransferEnds.isTerminal(TransferStatus.COMPLETE))
        assertTrue(TransferEnds.isTerminal(null))
    }

    @Test
    fun refusalsCloseTheRouteOtherErrorsDoNot() {
        assertEquals(RenameDecision.COPY_FROM_NOW_ON, RenameGate.decide(ErrnoKind.CROSS_DEVICE))
        assertEquals(RenameDecision.COPY_FROM_NOW_ON, RenameGate.decide(ErrnoKind.ACCESS))
        assertEquals(RenameDecision.COPY_FROM_NOW_ON, RenameGate.decide(ErrnoKind.PERMISSION))
        assertEquals(RenameDecision.COPY_THIS_FILE, RenameGate.decide(ErrnoKind.NOT_FOUND))
        assertEquals(RenameDecision.COPY_THIS_FILE, RenameGate.decide(ErrnoKind.OTHER))
    }

    @Test
    fun gateClosesOnceAndSaysSoOnce() {
        val g = RenameGate("test")
        assertTrue(g.open)
        assertEquals(RenameDecision.COPY_THIS_FILE to false, g.failed(ErrnoKind.NOT_FOUND))
        assertTrue(g.open)
        assertEquals(RenameDecision.COPY_FROM_NOW_ON to true, g.failed(ErrnoKind.CROSS_DEVICE))
        assertFalse(g.open)
        assertEquals(RenameDecision.COPY_FROM_NOW_ON to false, g.failed(ErrnoKind.ACCESS))
        assertFalse(g.open)
    }

    @Test
    fun firstSuccessIsReportedOnce() {
        val g = RenameGate("test")
        assertTrue(g.succeeded())
        assertFalse(g.succeeded())
        assertEquals(2L, g.renamed)
    }
}
