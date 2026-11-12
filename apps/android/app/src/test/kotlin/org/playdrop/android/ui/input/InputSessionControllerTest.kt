package org.playdrop.android.ui.input

import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.Timeout
import org.playdrop.core.InputEvent
import org.playdrop.core.PlayDropException

/** Session open and close rules. */
class InputSessionControllerTest {
    /** A locking mistake would hang the run; fail fast instead. */
    @get:Rule
    val timeout: Timeout = Timeout.seconds(10)

    private val sent = mutableListOf<InputEvent>()
    private var fail: Exception? = null
    private val controller = InputSessionController { ev ->
        fail?.let { throw it }
        sent += ev
    }

    private val open = InputEvent.Session(start = true)
    private val close = InputEvent.Session(start = false)
    private val move = InputEvent.PointerMove(dx = 1.0, dy = 1.0)

    @Test
    fun startsOnceAndStopsOnce() = runBlocking {
        assertEquals(InputSessionController.Outcome.Sent, controller.start())
        assertEquals(InputSessionController.Outcome.Skipped, controller.start())
        assertEquals(InputSessionController.Outcome.Sent, controller.stop())
        assertEquals(InputSessionController.Outcome.Skipped, controller.stop())
        assertEquals(listOf(open, close), sent)
    }

    @Test
    fun framesGoOutOnlyInsideASession() = runBlocking {
        assertEquals(InputSessionController.Outcome.Skipped, controller.send(move))
        controller.start()
        assertEquals(InputSessionController.Outcome.Sent, controller.send(move))
        controller.stop()
        assertEquals(InputSessionController.Outcome.Skipped, controller.send(move))
        assertEquals(listOf(open, move, close), sent)
    }

    @Test
    fun goingOfflineStopsAndComingBackRestarts() = runBlocking {
        controller.start()
        assertEquals(InputSessionController.Outcome.Sent, controller.setOnline(false))
        // The core reports the drop too; that must not turn the session into a final end.
        controller.connectionLost()
        assertEquals(InputSessionController.State.IDLE, controller.state)
        // A start while offline waits for the peer.
        assertEquals(InputSessionController.Outcome.Skipped, controller.start())
        assertEquals(InputSessionController.Outcome.Sent, controller.setOnline(true))
        assertEquals(listOf(open, close, open), sent)
    }

    @Test
    fun aLostConnectionIsResumedOnReconnect() = runBlocking {
        controller.start()
        // The core's event can come before the peer list shows the device offline.
        controller.connectionLost()
        assertEquals(InputSessionController.State.IDLE, controller.state)
        assertEquals(InputSessionController.Outcome.Skipped, controller.send(move))
        controller.setOnline(false)
        assertEquals(InputSessionController.Outcome.Sent, controller.setOnline(true))
        assertEquals(InputSessionController.State.ACTIVE, controller.state)
        // No stop frame went out for the lost session: the desktop had already closed it.
        assertEquals(listOf(open, open), sent)
    }

    @Test
    fun aLostConnectionIsResumedWhenThePeerNeverLookedOffline() = runBlocking {
        controller.start()
        controller.connectionLost()
        assertEquals(InputSessionController.Outcome.Sent, controller.resume())
        assertEquals(InputSessionController.Outcome.Skipped, controller.resume())
        assertEquals(listOf(open, open), sent)
    }

    @Test
    fun aLostConnectionKeepsARefusalOrAnEndAndAClosedScreen() = runBlocking {
        controller.start()
        controller.remoteEnded()
        controller.connectionLost()
        assertEquals(InputSessionController.State.ENDED, controller.state)
        assertEquals(InputSessionController.Outcome.Skipped, controller.resume())
        controller.retry()
        controller.stop()
        controller.connectionLost()
        // The screen left: nothing reopens.
        assertEquals(InputSessionController.Outcome.Skipped, controller.resume())
    }

    @Test
    fun comingOnlineAfterTheScreenLeftDoesNotRestart() = runBlocking {
        controller.start()
        controller.setOnline(false)
        controller.stop()
        controller.setOnline(true)
        assertEquals(listOf(open, close), sent)
    }

    @Test
    fun aRefusalIsNotRetriedAutomatically() = runBlocking {
        controller.start()
        controller.refused()
        assertEquals(InputSessionController.State.REFUSED, controller.state)
        assertEquals(InputSessionController.Outcome.Refused, controller.start())
        controller.setOnline(false)
        controller.setOnline(true)
        assertEquals(InputSessionController.Outcome.Skipped, controller.send(move))
        assertEquals(listOf(open), sent)
    }

    @Test
    fun aSessionEndedByTheDesktopStaysEnded() = runBlocking {
        controller.start()
        controller.remoteEnded()
        assertEquals(InputSessionController.State.ENDED, controller.state)
        // No stop frame (the desktop closed it), no frames, and no reopening on start or reconnect.
        assertEquals(InputSessionController.Outcome.Skipped, controller.stop())
        assertEquals(InputSessionController.Outcome.Skipped, controller.send(move))
        assertEquals(InputSessionController.Outcome.Skipped, controller.start())
        controller.setOnline(false)
        controller.setOnline(true)
        assertEquals(listOf(open), sent)
    }

    @Test
    fun forbiddenFromTheCoreCountsAsARefusal() = runBlocking {
        fail = PlayDropException.Forbidden("input.pointer")
        assertEquals(InputSessionController.Outcome.Refused, controller.start())
        assertEquals(InputSessionController.State.REFUSED, controller.state)
        fail = null
        assertEquals(InputSessionController.Outcome.Refused, controller.start())
        assertTrue(sent.isEmpty())
    }

    @Test
    fun otherFailuresLeaveTheSessionClosedAndRetryable() = runBlocking {
        fail = IllegalStateException("core not running")
        assertTrue(controller.start() is InputSessionController.Outcome.Failed)
        assertEquals(InputSessionController.State.IDLE, controller.state)
        fail = null
        assertEquals(InputSessionController.Outcome.Sent, controller.start())
    }

    @Test
    fun tryAgainReopensAfterAnEndOrARefusal() = runBlocking {
        controller.start()
        controller.remoteEnded()
        assertEquals(InputSessionController.Outcome.Sent, controller.retry())
        assertEquals(InputSessionController.State.ACTIVE, controller.state)
        assertEquals(InputSessionController.Outcome.Skipped, controller.retry())
        controller.refused()
        assertEquals(InputSessionController.Outcome.Sent, controller.retry())
        assertEquals(listOf(open, open, open), sent)
    }

    @Test
    fun tryAgainWhileOfflineOpensOnReconnect() = runBlocking {
        controller.start()
        controller.remoteEnded()
        controller.setOnline(false)
        assertEquals(InputSessionController.Outcome.Skipped, controller.retry())
        assertEquals(InputSessionController.Outcome.Sent, controller.setOnline(true))
        assertEquals(InputSessionController.State.ACTIVE, controller.state)
    }
}
