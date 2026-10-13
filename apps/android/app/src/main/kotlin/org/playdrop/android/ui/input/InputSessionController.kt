package org.playdrop.android.ui.input

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import org.playdrop.core.InputEvent
import org.playdrop.core.PlayDropException

/**
 * Opens and closes one remote input session with a peer (protocol.md 5.12) and gates every
 * frame on it. Plain Kotlin so the rules are unit-tested; the view model feeds it the screen's
 * lifecycle and the peer's online state, and [transport] is the core's `sendInput`.
 *
 * Rules:
 * - [start] opens the session once; a second [start] while open sends nothing.
 * - [stop] closes it once; a second [stop] sends nothing.
 * - Going offline closes it (the frame may fail, which is fine: the desktop also ends the
 *   session on disconnect). Coming back online reopens it only while the screen still wants it.
 * - After a refusal (the desktop has remote input off for this phone) nothing reopens the
 *   session on its own; the screen tells the user and leaves.
 * - When the desktop ends the session itself ([remoteEnded]: it locked, saw no input for 10
 *   minutes, or its user hit Stop) nothing reopens it on its own either, and no stop frame
 *   goes out because the desktop already closed it.
 *
 * Every method is safe to call from any coroutine: one mutex keeps the frames in order.
 */
class InputSessionController(private val transport: suspend (InputEvent) -> Unit) {
    /** Where the session stands. */
    enum class State {
        /** No session open. */
        IDLE,

        /** Session open; frames go out. */
        ACTIVE,

        /** The peer refused; nothing opens the session again. */
        REFUSED,

        /** The peer ended the session; nothing opens it again on its own. */
        ENDED,
    }

    /** What a call did. */
    sealed interface Outcome {
        /** The frame went to the core. */
        data object Sent : Outcome

        /** Nothing was sent: no session, or nothing to do. */
        data object Skipped : Outcome

        /** The peer or the core refused remote input; the session is now [State.REFUSED]. */
        data object Refused : Outcome

        /** The core failed for another reason (offline, not running). */
        data class Failed(val error: Throwable) : Outcome
    }

    private val lock = Mutex()
    private var wanted = false
    private var online = true

    /** The current state. */
    @Volatile
    var state: State = State.IDLE
        private set

    /** The screen is visible: open the session if the peer is online. */
    suspend fun start(): Outcome = locked {
        when (state) {
            State.REFUSED -> Outcome.Refused
            State.ENDED -> Outcome.Skipped
            else -> {
                wanted = true
                if (online) openLocked() else Outcome.Skipped
            }
        }
    }

    /** The screen is gone or in the background: close the session. */
    suspend fun stop(): Outcome = locked {
        wanted = false
        closeLocked()
    }

    /** The peer's online state changed. */
    suspend fun setOnline(isOnline: Boolean): Outcome = locked {
        when {
            online == isOnline -> Outcome.Skipped
            !isOnline -> {
                online = false
                closeLocked()
            }
            else -> {
                online = true
                if (wanted && state == State.IDLE) openLocked() else Outcome.Skipped
            }
        }
    }

    /** The peer answered an input frame with `forbidden`: stop for good. */
    suspend fun refused() = locked {
        state = State.REFUSED
        wanted = false
    }

    /** The peer ended our session (`InputSessionEnded`): stop sending, never reopen on our own. */
    suspend fun remoteEnded() = locked {
        if (state != State.REFUSED) state = State.ENDED
        wanted = false
    }

    /** Sends one frame inside the open session; skipped when there is none. */
    suspend fun send(event: InputEvent): Outcome = locked {
        if (state == State.ACTIVE) deliver(event) else Outcome.Skipped
    }

    /** Runs [block] holding the mutex; a plain function so no branch can skip the unlock. */
    private suspend fun <T> locked(block: suspend () -> T): T {
        lock.lock()
        try {
            return block()
        } finally {
            lock.unlock()
        }
    }

    private suspend fun openLocked(): Outcome {
        if (state != State.IDLE) return Outcome.Skipped
        val outcome = deliver(InputEvent.Session(start = true))
        if (outcome == Outcome.Sent) state = State.ACTIVE
        return outcome
    }

    private suspend fun closeLocked(): Outcome {
        if (state != State.ACTIVE) return Outcome.Skipped
        state = State.IDLE
        return deliver(InputEvent.Session(start = false))
    }

    private suspend fun deliver(event: InputEvent): Outcome = try {
        transport(event)
        Outcome.Sent
    } catch (e: CancellationException) {
        throw e
    } catch (e: PlayDropException.Forbidden) {
        state = State.REFUSED
        wanted = false
        Outcome.Refused
    } catch (e: Exception) {
        Outcome.Failed(e)
    }
}
