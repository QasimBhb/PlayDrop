package org.playdrop.android.share

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
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

    /** The core's transfer-end events, as [CoreHost.events] would carry them. */
    private val ends = MutableSharedFlow<SendEnd>(extraBufferCapacity = 16)

    private fun run(
        peers: List<String>,
        waitInPrepare: Boolean = false,
        failPrepare: Exception? = null,
        onStart: suspend (String) -> Unit = {},
    ) = ManySend(
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
            onStart(peer)
            "id-$peer"
        },
        stop = { id -> stopped += id },
        release = { share -> released += share },
        report = { reports += it },
        ends = ends,
        decideWithinMs = 2_000,
        settleWithinMs = 300,
    )

    private fun completed(id: String) = SendEnd(id, completed = true, byCancel = false, reason = "")

    private fun cancelledHere(id: String) = SendEnd(id, completed = false, byCancel = true, reason = "cancelled")

    @Test
    fun keptSendsAllStartAndAreReported() = runBlocking {
        val r = run(listOf("a", "b"))
        val job = r.launch()
        r.allStarted.await()
        r.keep()
        job.join()
        assertEquals(listOf("a:share-0", "b:share-1"), started)
        val done = reports.single() as ManySend.Report.Done
        assertEquals(ManySend.Report.Done(listOf(ManySend.Device.Running(0, "id-a"), ManySend.Device.Running(1, "id-b")), cancelled = false), done)
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
        // No end was heard for any of them: each was stopped part way.
        assertEquals(
            ManySend.Report.Done(listOf(ManySend.Device.Stopped(0), ManySend.Device.Stopped(1), ManySend.Device.Stopped(2)), cancelled = true),
            reports.single(),
        )
    }

    @Test
    fun cancelAfterEverySendCompletedSaysTheyWereSent() = runBlocking {
        // The phone run: two small sends finished 0.1 s after Send, Cancel came a second later.
        val r = run(listOf("a", "b"))
        val job = r.launch()
        r.allStarted.await()
        ends.emit(completed("id-a"))
        ends.emit(completed("id-b"))
        r.allFinished.await()
        r.cancel()
        job.join()
        // Whether the Cancel tap counted or came too late, both devices are reported as sent.
        val done = reports.single() as ManySend.Report.Done
        assertEquals(listOf(ManySend.Device.Completed(0), ManySend.Device.Completed(1)), done.devices)
    }

    @Test
    fun everySendCompletingReportsAtOnceWithoutADecision() = runBlocking {
        val r = run(listOf("a", "b"))
        val job = r.launch()
        r.allStarted.await()
        ends.emit(completed("id-a"))
        ends.emit(completed("id-b"))
        // decideWithinMs is 2 s; the report must not wait for it.
        withTimeout(1_000) { job.join() }
        assertEquals(
            ManySend.Report.Done(listOf(ManySend.Device.Completed(0), ManySend.Device.Completed(1)), cancelled = false),
            reports.single(),
        )
        assertTrue(stopped.isEmpty())
    }

    @Test
    fun anEndHeardBeforeTheStartReturnedStillCounts() = runBlocking {
        // The core may report the end before send() has handed back the transfer ID.
        val r = run(listOf("a"), onStart = { peer -> ends.emit(completed("id-$peer")) })
        val job = r.launch()
        withTimeout(1_000) { r.allFinished.await() }
        job.join()
        assertEquals(ManySend.Report.Done(listOf(ManySend.Device.Completed(0)), cancelled = false), reports.single())
    }

    @Test
    fun cancelReportsEachDeviceTruthfully() = runBlocking {
        val r = run(listOf("a", "b", "c"), onStart = { peer -> if (peer == "c") gate.await() })
        val job = r.launch()
        // a finished, b is going, c is still starting when Cancel comes.
        while (started.size < 3) yield()
        ends.emit(completed("id-a"))
        r.cancel()
        gate.complete(Unit)
        // The core confirms b was cancelled here.
        ends.emit(cancelledHere("id-b"))
        job.join()
        val done = reports.single() as ManySend.Report.Done
        assertTrue(done.cancelled)
        assertEquals(ManySend.Device.Completed(0), done.devices[0])
        assertEquals(ManySend.Device.Stopped(1), done.devices[1])
        assertEquals(ManySend.Device.Stopped(2), done.devices[2])
        // The finished send needs no stop to be reported as sent; the late start is stopped.
        assertTrue("id-c" in stopped)
    }

    @Test
    fun cancelBeforeALaterDeviceStartsSaysItWasNotSent() = runBlocking {
        val r = run(listOf("a", "b"), onStart = { peer -> if (peer == "a") gate.await() })
        val job = r.launch()
        while (started.isEmpty()) yield()
        r.cancel()
        gate.complete(Unit)
        job.join()
        val done = reports.single() as ManySend.Report.Done
        // a started while Cancel was handled and was stopped at once; b never started.
        assertEquals(listOf(ManySend.Device.Stopped(0), ManySend.Device.NotStarted(1)), done.devices)
        assertEquals(listOf("share-1"), released)
    }

    @Test
    fun aSendThatFailedIsReportedWithTheCoresReason() = runBlocking {
        val r = run(listOf("a", "b"))
        val job = r.launch()
        r.allStarted.await()
        ends.emit(completed("id-a"))
        ends.emit(SendEnd("id-b", completed = false, byCancel = false, reason = "the receiving device does not have enough free space"))
        job.join()
        assertEquals(
            ManySend.Report.Done(
                listOf(ManySend.Device.Completed(0), ManySend.Device.Failed(1, "the receiving device does not have enough free space")),
                cancelled = false,
            ),
            reports.single(),
        )
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
        assertEquals(ManySend.Report.Done(listOf(ManySend.Device.Running(0, "id-a")), cancelled = false), reports.single())
        assertTrue(stopped.isEmpty())
    }
}
