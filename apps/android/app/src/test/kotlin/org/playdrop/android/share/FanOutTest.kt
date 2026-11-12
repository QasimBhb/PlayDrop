package org.playdrop.android.share

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.Timeout

/** One send per device, and the Cancel after "Send" that stops all of them (SR-29). */
class FanOutTest {
    /** A locking mistake would hang the run; fail fast instead. */
    @get:Rule
    val timeout: Timeout = Timeout.seconds(10)

    private val startedFor = mutableListOf<String>()
    private val stopped = mutableListOf<String>()
    private val released = mutableListOf<String>()

    private fun fan(targets: List<String>, start: suspend (String) -> String) = FanOut(
        targets,
        start = { t -> startedFor += t; start(t) },
        stop = { id -> stopped += id },
        release = { t -> released += t },
    )

    @Test
    fun startsEveryTargetInOrder() = runBlocking {
        val f = fan(listOf("A", "B", "C")) { "id-$it" }
        val out = f.run()
        assertEquals(listOf("A", "B", "C"), startedFor)
        assertEquals(listOf("id-A", "id-B", "id-C"), out.map { (it as FanOut.Outcome.Started).id })
        assertTrue(stopped.isEmpty())
        assertTrue(released.isEmpty())
        assertFalse(f.isCancelled())
    }

    @Test
    fun aFailedStartDoesNotStopTheOthers() = runBlocking {
        val f = fan(listOf("A", "B")) { if (it == "A") throw IllegalStateException("no") else "id-$it" }
        val out = f.run()
        assertTrue(out[0] is FanOut.Outcome.Failed)
        assertEquals(FanOut.Outcome.Started("B", "id-B"), out[1])
    }

    @Test
    fun cancelStopsStartedSendsAndStartsNoMore() = runBlocking {
        val bStarting = CompletableDeferred<Unit>()
        val bMayFinish = CompletableDeferred<Unit>()
        lateinit var f: FanOut<String>
        f = fan(listOf("A", "B", "C")) { t ->
            if (t == "B") {
                bStarting.complete(Unit)
                bMayFinish.await()
            }
            "id-$t"
        }
        val run = async { f.run() }
        bStarting.await()
        // Cancel while B's start is in flight: A is stopped at once.
        f.cancelAll()
        assertEquals(listOf("id-A"), stopped)
        bMayFinish.complete(Unit)
        val out = run.await()
        // B returned its ID after Cancel and is stopped as soon as it does; C never starts.
        assertEquals(listOf("id-A", "id-B"), stopped)
        assertEquals(listOf("A", "B"), startedFor)
        assertEquals(listOf("C"), released)
        assertEquals(FanOut.Outcome.Stopped("B", "id-B"), out[1])
        assertEquals(FanOut.Outcome.Skipped("C"), out[2])
        assertTrue(f.isCancelled())
    }

    @Test
    fun cancelBeforeRunStartsNothing() = runBlocking {
        val f = fan(listOf("A", "B")) { "id-$it" }
        f.cancelAll()
        val out = f.run()
        assertTrue(startedFor.isEmpty())
        assertEquals(listOf("A", "B"), released)
        assertTrue(out.all { it is FanOut.Outcome.Skipped })
    }

    @Test
    fun cancelAfterEverythingStartedStopsEachOnce() = runBlocking {
        val f = fan(listOf("A", "B")) { "id-$it" }
        f.run()
        f.cancelAll()
        f.cancelAll()
        assertEquals(listOf("id-A", "id-B"), stopped)
    }
}
