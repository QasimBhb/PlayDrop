package org.playdrop.android.share

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.Timeout

/** The send card's waits: countdown, chooser, Cancel, and leaving after "Send". */
class SendSheetTest {
    @get:Rule
    val timeout: Timeout = Timeout.seconds(10)

    private val laptop = Candidate("aa", "calliope", "AAAA-1111", online = true, clipboardOut = true)
    private val desk = Candidate("cc", "desk", "CCCC-3333", online = false, clipboardOut = true)
    private val selection = Selection.of(listOf(laptop, desk), "aa", SendKind.FILES)
    private fun sheet() = SendSheet(onMakeDefault = {}, windowMs = 50)

    @Test
    fun theCountdownSendsWhenItRunsOut() = runBlocking {
        val s = sheet()
        assertTrue(s.countdown("Sending 1 item to calliope"))
        assertEquals(SheetState.Countdown("Sending 1 item to calliope"), s.state)
    }

    @Test
    fun cancelOrLeavingDuringTheCountdownSendsNothing() = runBlocking {
        val s = sheet()
        val running = async { s.countdown("t") }
        yield()
        s.cancel()
        assertFalse(running.await())
        val left = sheet()
        val running2 = async { left.countdown("t") }
        yield()
        left.leave()
        assertFalse(running2.await())
    }

    @Test
    fun sendTurnsTheChooserIntoTheSendingCardAtOnce() = runBlocking {
        val s = sheet()
        val chosen = async { s.choose(selection, SendKind.FILES, 3) { ids -> "Sending 3 items to ${ids.joinToString()}" } }
        yield()
        s.toggle("cc")
        s.send()
        // No chooser left to tap while the sends are prepared.
        assertEquals(SheetState.Sending("Sending 3 items to aa, cc"), s.state)
        s.toggle("aa")
        assertEquals(SheetState.Sending("Sending 3 items to aa, cc"), s.state)
        assertEquals(listOf("aa", "cc"), chosen.await())
    }

    @Test
    fun sendWithoutASendingCardHidesTheCard() = runBlocking {
        val s = sheet()
        val chosen = async { s.choose(selection, SendKind.SHARED_TEXT, 1) }
        yield()
        s.send()
        assertEquals(SheetState.Hidden, s.state)
        assertEquals(listOf("aa"), chosen.await())
    }

    @Test
    fun cancelInTheChooserChoosesNothing() = runBlocking {
        val s = sheet()
        val chosen = async { s.choose(selection, SendKind.FILES, 3) }
        yield()
        s.cancel()
        assertNull(chosen.await())
        // A second chooser after Cancel is not offered.
        assertNull(s.choose(selection, SendKind.FILES, 3))
    }

    @Test
    fun leavingBeforeSendCancels() = runBlocking {
        val s = sheet()
        val chosen = async { s.choose(selection, SendKind.FILES, 3) }
        yield()
        s.leave()
        assertNull(chosen.await())
    }

    @Test
    fun leavingAfterSendKeepsTheSends() = runBlocking {
        val s = sheet()
        val ready = CompletableDeferred<Unit>()
        val held = async { s.holdSending("Sending to A and B", ready) }
        yield()
        s.leave()
        assertFalse("leaving is not a Cancel", held.await())
    }

    @Test
    fun cancelOnTheSendingCardIsReported() = runBlocking {
        val s = sheet()
        val held = async { s.holdSending("Sending to A and B", CompletableDeferred()) }
        yield()
        s.cancel()
        assertTrue(held.await())
    }

    @Test
    fun theSendingCardStaysUntilEverySendStarted() = runBlocking {
        val s = sheet()
        val ready = CompletableDeferred<Unit>()
        val held = async { s.holdSending("Sending to A and B", ready) }
        delay(120)
        // The window is over but a send is still starting: Cancel stays on offer.
        assertFalse(held.isCompleted)
        ready.complete(Unit)
        assertFalse(held.await())
    }

    @Test
    fun theSendingCardGoesOnceEverySendEnded() = runBlocking {
        val s = SendSheet(onMakeDefault = {}, windowMs = 10_000)
        val finished = CompletableDeferred<Unit>()
        val held = async { s.holdSending("Sending to A and B", CompletableDeferred(), finished) }
        yield()
        assertFalse(held.isCompleted)
        // Nothing is left to cancel: the card closes before its window, without a Cancel.
        finished.complete(Unit)
        assertFalse(withTimeout(1_000) { held.await() })
    }
}
