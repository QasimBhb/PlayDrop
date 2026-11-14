package org.playdrop.android.core

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.data.SendHistory
import org.playdrop.android.data.SendHistoryEntry
import org.playdrop.android.data.SendOutcome
import org.playdrop.core.TransferFailure

/** The history recorder's rules, against an in-memory store. */
class SendHistoryRecorderTest {
    private class Memory : SendHistoryBackend {
        var entries = emptyList<SendHistoryEntry>()
        override suspend fun upsert(entry: SendHistoryEntry, allowed: suspend () -> Boolean) {
            if (allowed()) entries = SendHistory.upsert(entries, entry)
        }

        override suspend fun enrich(transferId: String, peerLabel: String, itemCount: Long, totalBytes: Long, names: List<String>) {
            entries = SendHistory.enriched(entries, transferId, peerLabel, itemCount, totalBytes, names)
        }

        override suspend fun forgotten(transferId: String): Boolean {
            val next = SendHistory.forgotten(entries, transferId)
            val changed = next != entries
            entries = next
            return changed
        }
    }

    private var on = true
    private val mem = Memory()
    private val rec = SendHistoryRecorder(mem, { on }, { 1_000L }, maxPending = 3)
    private val label = { _: String -> "Laptop" }
    private val noShape = { _: String -> null as TransferShape? }

    private fun done(id: String) = SendEndInfo.Done(id, "peer", 10, listOf("x.txt"))
    private fun stopped(id: String, code: TransferFailure) = SendEndInfo.Stopped(id, "peer", code, "")

    @Test
    fun nothingIsWrittenWhileTheSwitchIsOff() = runBlocking {
        on = false
        rec.noteSend("t1", "Laptop", listOf("a.txt"), 1, 5)
        rec.onEnd(done("t1"), label, noShape) { false }
        assertTrue(mem.entries.isEmpty())
        on = true
        rec.noteSend("t2", "Laptop", listOf("a.txt"), 1, 5)
        rec.onEnd(done("t2"), label, noShape) { false }
        assertEquals(1, mem.entries.size)
    }

    @Test
    fun resumableThenCompleteIsOneEntry() = runBlocking {
        rec.noteSend("t1", "Laptop", listOf("a.txt", "b.txt"), 2, 50)
        rec.onEnd(stopped("t1", TransferFailure.CONNECTION_LOST), label, noShape) { true }
        assertEquals(1, mem.entries.size)
        assertTrue(mem.entries[0].resumable)
        assertEquals(listOf("a.txt", "b.txt"), mem.entries[0].names)
        rec.onEnd(done("t1"), label, noShape) { false }
        assertEquals(1, mem.entries.size)
        val e = mem.entries[0]
        assertEquals(SendOutcome.SENT, e.outcome)
        assertFalse(e.resumable)
        // The details noted at the start survive the failed stop in between.
        assertEquals(listOf("a.txt", "b.txt"), e.names)
        assertEquals(50L, e.totalBytes)
    }

    @Test
    fun forgottenEndsTheOneEntryAsCancelled() = runBlocking {
        rec.noteSend("t1", "Laptop", listOf("a.txt"), 1, 5)
        rec.onEnd(stopped("t1", TransferFailure.TIMED_OUT), label, noShape) { true }
        rec.forgotten("t1")
        assertEquals(1, mem.entries.size)
        assertEquals(SendOutcome.CANCELLED, mem.entries[0].outcome)
        rec.onEnd(stopped("t1", TransferFailure.CANCELLED_BY_YOU), label, noShape) { false }
        assertEquals(1, mem.entries.size)
    }

    @Test
    fun cancellingARunningSendKeepsItsNamesAndSize() = runBlocking {
        rec.noteSend("t1", "Laptop", listOf("a.txt"), 4, 400)
        // The Transfers tab's cancel calls forgotten, which must not drop the note of a running send.
        rec.forgotten("t1")
        rec.onEnd(stopped("t1", TransferFailure.CANCELLED_BY_YOU), label, noShape) { false }
        val e = mem.entries.single()
        assertEquals(SendOutcome.CANCELLED, e.outcome)
        assertEquals(listOf("a.txt"), e.names)
        assertEquals(400L, e.totalBytes)
        assertEquals(4L, e.itemCount)
    }

    @Test
    fun anEndBeforeNoteSendIsFilledInAndNothingLeaks() = runBlocking {
        rec.onEnd(stopped("t1", TransferFailure.NO_SPACE), label, noShape) { false }
        assertTrue(mem.entries.single().names.isEmpty())
        rec.noteSend("t1", "Laptop", listOf("a.txt"), 1, 5)
        val e = mem.entries.single()
        assertEquals(listOf("a.txt"), e.names)
        assertEquals(5L, e.totalBytes)
        assertEquals(0, rec.pendingCount)
    }

    @Test
    fun theOldestNoteLeavesAtTheCapAndRunningSendsStay() = runBlocking {
        for (i in 1..4) rec.noteSend("t$i", "Laptop", listOf("f$i"), 1, i.toLong())
        assertEquals(3, rec.pendingCount)
        rec.onEnd(done("t4"), label, noShape) { false }
        assertEquals(listOf("f4"), mem.entries.single().names)
        // t1 was evicted: its end falls back to the event's own file list.
        rec.onEnd(done("t1"), label, noShape) { false }
        assertEquals(2, mem.entries.size)
    }
}
