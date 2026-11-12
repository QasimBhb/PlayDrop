package org.playdrop.android.share

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.Timeout
import java.util.Collections

/** A send to several devices that outlives its activity: preparing, Cancel, keep, report. */
class ManySendTest {
    @get:Rule
    val timeout: Timeout = Timeout.seconds(10)

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val started = Collections.synchronizedList(mutableListOf<String>())
    private val stopped = Collections.synchronizedList(mutableListOf<String>())
    private val released = Collections.synchronizedList(mutableListOf<String>())
    private val reports = Collections.synchronizedList(mutableListOf<ManySend.Report>())
    private var preparedFor: Int? = null
    private val gate = CompletableDeferred<Unit>()

    @After
    fun tearDown() = scope.cancel()

    private fun run(peers: List<String>, waitInPrepare: Boolean = false, failPrepare: Exception? = null) = ManySend(
        title = "Sending",
        peers = peers,
        scope = scope,
        prepare = { n ->
            if (waitInPrepare) gate.await()
            failPrepare?.let { throw it }
            preparedFor = n
            List(n) { "share-$it" }
        },
        start = { peer, share ->
            started += "$peer:$share"
            "id-$peer"
        },
        stop = { id -> stopped += id },
        release = { share -> released += share },
        report = { reports += it },
        decideWithinMs = 2_000,
    )

    @Test
    fun keptSendsAllStartAndAreReported() = runBlocking {
        val r = run(listOf("a", "b"))
        val job = r.launch()
        r.allStarted.await()
        r.keep()
        job.join()
        assertEquals(listOf("a:share-0", "b:share-1"), started)
        val done = reports.single() as ManySend.Report.Done
        assertEquals(listOf(FanOut.Outcome.Started(0, "id-a"), FanOut.Outcome.Started(1, "id-b")), done.outcomes)
        assertTrue(stopped.isEmpty())
        // Ended: a recreated activity finds nothing to show.
        assertNull(ManySends.find(r.token))
    }

    @Test
    fun aRecreatedActivityFindsTheRunWhileItIsGoing() = runBlocking {
        val r = run(listOf("a", "b"), waitInPrepare = true)
        val job = r.launch()
        assertSame(r, ManySends.find(r.token))
        gate.complete(Unit)
        r.allStarted.await()
        r.keep()
        job.join()
        assertNull(ManySends.find(r.token))
    }

    @Test
    fun cancelWhilePreparingStartsNothingAndLetsGoOfEverything() = runBlocking {
        val r = run(listOf("a", "b"), waitInPrepare = true)
        val job = r.launch()
        r.cancel()
        gate.complete(Unit)
        job.join()
        assertTrue(started.isEmpty())
        assertEquals(ManySend.Report.Cancelled, reports.single())
        // Either nothing was prepared, or every prepared share was let go.
        assertEquals(List(preparedFor ?: 0) { "share-$it" }.toSet(), released.toSet())
    }

    @Test
    fun cancelAfterTheStartsStopsEveryStartedSend() = runBlocking {
        val r = run(listOf("a", "b", "c"))
        val job = r.launch()
        r.allStarted.await()
        r.cancel()
        job.join()
        assertEquals(setOf("id-a", "id-b", "id-c"), stopped.toSet())
        assertEquals(ManySend.Report.Cancelled, reports.single())
    }

    @Test
    fun aFailedPreparationIsReportedAndStartsNothing() = runBlocking {
        val r = run(listOf("a", "b"), failPrepare = IllegalStateException("cannot read"))
        val job = r.launch()
        r.allStarted.await()
        job.join()
        assertTrue(started.isEmpty())
        assertTrue(reports.single() is ManySend.Report.PrepareFailed)
    }

    @Test
    fun withoutADecisionTheSendsGoOn() = runBlocking {
        val r = ManySend(
            title = "Sending",
            peers = listOf("a"),
            scope = scope,
            prepare = { n -> List(n) { "share-$it" } },
            start = { peer, _ -> "id-$peer" },
            stop = { id -> stopped += id },
            release = { released += it },
            report = { reports += it },
            decideWithinMs = 50,
        )
        r.launch().join()
        assertTrue(reports.single() is ManySend.Report.Done)
        assertTrue(stopped.isEmpty())
    }
}
