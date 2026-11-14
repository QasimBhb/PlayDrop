package org.playdrop.android.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** The "Sent" history's bounding, entry building, file format and damage tolerance. */
class SendHistoryTest {
    private fun entry(at: Long, outcome: SendOutcome = SendOutcome.SENT, id: String = "t$at") =
        SendHistory.entry(id, at, "Laptop", 3, 2048, listOf("a.txt", "b.txt", "c.txt"), outcome)

    @Test
    fun keepsOnlyTheNewestFifty() {
        var h = emptyList<SendHistoryEntry>()
        for (i in 1..60) h = SendHistory.upsert(h, entry(i.toLong()))
        assertEquals(50, h.size)
        assertEquals(60L, h.first().atMs)
        assertEquals(11L, h.last().atMs)
    }

    @Test
    fun anEntryKeepsTheLastPartOfThePathAndAtMostThreeNames() {
        val e = SendHistory.entry("t", 1, "PC", 5, 10, listOf("Photos/2026/a.jpg", " ", "b.jpg", "c.jpg", "d.jpg"), SendOutcome.SENT)
        assertEquals(listOf("a.jpg", "b.jpg", "c.jpg"), e.names)
    }

    @Test
    fun anEntryCutsLongTextAndNeverHasNegativeCounts() {
        val e = SendHistory.entry("t", 1, "PC", -4, -9, listOf("x".repeat(500)), SendOutcome.FAILED, "OTHER", "y".repeat(500))
        assertEquals(SendHistory.MAX_NAME_CHARS, e.names.single().length)
        assertEquals(SendHistory.MAX_DETAIL_CHARS, e.detail!!.length)
        assertEquals(0L, e.itemCount)
        assertEquals(0L, e.totalBytes)
    }

    @Test
    fun roundTripKeepsEveryFieldIncludingAwkwardText() {
        val list = listOf(
            SendHistory.entry("t10", 10, "Tab\there \\ back", 2, 99, listOf("line\nbreak.txt", "ok.txt"), SendOutcome.SENT),
            SendHistory.entry("t9", 9, "Desk", 1, 5, emptyList(), SendOutcome.FAILED, "NO_SPACE", "Not enough room."),
            SendHistory.entry("t8", 8, "Desk", 0, 0, emptyList(), SendOutcome.CANCELLED),
        )
        assertEquals(list, SendHistory.decode(SendHistory.encode(list)))
    }

    @Test
    fun damagedLinesAreSkippedAndTheRestIsKept() {
        val good = SendHistory.encode(listOf(entry(2), entry(1)))
        val lines = good.lines().filter { it.isNotEmpty() }
        val text = listOf(
            lines[0],
            "garbage",
            "2\tt\tnot-a-number\tSENT\tPC\t1\t1\t\t\t0",
            "2\tt\t5\tNOPE\tPC\t1\t1\t\t\t0",
            "2\tt\t5\tSENT\tPC\t-1\t1\t\t\t0",
            "1\t5\tSENT\tPC\t1\t1\t\t",
            "2\t\t5\tSENT\tPC\t1\t1\t\t\t0",
            "",
            lines[1],
        ).joinToString("\n")
        assertEquals(listOf(2L, 1L), SendHistory.decode(text).map { it.atMs })
    }

    @Test
    fun emptyOrBinaryFilesReadAsEmpty() {
        assertTrue(SendHistory.decode("").isEmpty())
        assertTrue(SendHistory.decode("\u0000\u0001\u0002\n\n").isEmpty())
    }

    @Test
    fun aHugeFileIsReadAsAtMostFiftyEntries() {
        val text = (1..80).joinToString("") { SendHistory.encode(listOf(entry(it.toLong()))) }
        assertEquals(50, SendHistory.decode(text).size)
    }

    @Test
    fun emptyFailureFieldsReadBackAsNull() {
        val e = SendHistory.decode(SendHistory.encode(listOf(entry(1)))).single()
        assertNull(e.failure)
        assertNull(e.detail)
    }

    @Test
    fun aResumedSendEndsAsOneEntry() {
        val failed = SendHistory.entry("t1", 10, "Laptop", 1, 5, listOf("a.txt"), SendOutcome.FAILED, "CONNECTION_LOST", resumable = true)
        val other = entry(5)
        var h = SendHistory.upsert(listOf(other), failed)
        assertEquals(listOf("t1", "t5"), h.map { it.transferId })
        assertTrue(h.first().resumable)
        val sent = SendHistory.entry("t1", 20, "Laptop", 1, 5, listOf("a.txt"), SendOutcome.SENT)
        h = SendHistory.upsert(h, sent)
        assertEquals(listOf("t1", "t5"), h.map { it.transferId })
        assertEquals(SendOutcome.SENT, h.first().outcome)
        assertEquals(false, h.first().resumable)
        assertNull(h.first().failure)
    }

    @Test
    fun onlyAFailedEntryCanBeResumable() {
        assertEquals(false, SendHistory.entry("t", 1, "PC", 1, 1, emptyList(), SendOutcome.SENT, resumable = true).resumable)
        val e = SendHistory.entry("t", 1, "PC", 1, 1, emptyList(), SendOutcome.FAILED, "TIMED_OUT", resumable = true)
        assertEquals(listOf(e), SendHistory.decode(SendHistory.encode(listOf(e))))
    }

    @Test
    fun forgettingAResumableFailedSendLeavesOneCancelledEntry() {
        val failed = SendHistory.entry("t1", 10, "Laptop", 1, 5, listOf("a.txt"), SendOutcome.FAILED, "TIMED_OUT", "x", resumable = true)
        val sent = entry(3)
        val h = SendHistory.forgotten(listOf(failed, sent), "t1")
        assertEquals(2, h.size)
        assertEquals(SendOutcome.CANCELLED, h[0].outcome)
        assertNull(h[0].failure)
        assertEquals(false, h[0].resumable)
        // The cancelled event that may follow replaces the same entry, never adds one.
        val after = SendHistory.upsert(h, SendHistory.entry("t1", 11, "Laptop", 1, 5, listOf("a.txt"), SendOutcome.CANCELLED))
        assertEquals(2, after.size)
        // Sent entries and unknown IDs are left alone.
        assertEquals(h, SendHistory.forgotten(h, "t3").let { SendHistory.forgotten(it, "nope") })
        assertEquals(SendOutcome.SENT, SendHistory.forgotten(h, "t3")[1].outcome)
    }

    @Test
    fun anUpdatedEntryMovesToTheTop() {
        val h = listOf(entry(3), entry(2), entry(1))
        val next = SendHistory.upsert(h, SendHistory.entry("t1", 9, "Laptop", 3, 2048, emptyList(), SendOutcome.SENT))
        assertEquals(listOf("t1", "t3", "t2"), next.map { it.transferId })
    }

    @Test
    fun enrichingFillsInOnlyThatEntry() {
        val h = listOf(SendHistory.entry("t1", 5, "?", 0, 0, emptyList(), SendOutcome.FAILED), entry(2))
        val next = SendHistory.enriched(h, "t1", "Laptop", 4, 400, listOf("a/b.txt"))
        assertEquals(listOf("b.txt"), next[0].names)
        assertEquals(4L, next[0].itemCount)
        assertEquals("Laptop", next[0].peerLabel)
        assertEquals(h[1], next[1])
    }

    @Test
    fun aResumableClaimEndsWhenTheCoreNoLongerListsTheSend() {
        val waiting = SendHistory.entry("t1", 1_000, "L", 1, 1, emptyList(), SendOutcome.FAILED, "TIMED_OUT", resumable = true)
        val other = SendHistory.entry("t2", 1_000, "L", 1, 1, emptyList(), SendOutcome.FAILED, "TIMED_OUT", resumable = true)
        val now = 2_000L
        // Still listed: kept. Not listed: the claim is dropped, the failure stays.
        val r = SendHistory.reconcile(listOf(waiting, other), setOf("t1"), now)
        assertTrue(r[0].resumable)
        assertFalse(r[1].resumable)
        assertEquals(SendOutcome.FAILED, r[1].outcome)
        assertEquals("TIMED_OUT", r[1].failure)
        // The core cannot be asked: only age decides.
        assertTrue(SendHistory.reconcile(listOf(waiting), null, now)[0].resumable)
        assertFalse(SendHistory.reconcile(listOf(waiting), null, 1_000 + SendHistory.MAX_RESUMABLE_AGE_MS + 1)[0].resumable)
    }
}
