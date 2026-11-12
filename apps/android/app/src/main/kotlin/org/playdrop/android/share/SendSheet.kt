package org.playdrop.android.share

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withTimeoutOrNull

/** What the bottom card of a send activity shows. */
internal sealed class SheetState {
    /** Nothing (a picker or a dialog is on top). */
    data object Hidden : SheetState()

    /** Opening the shared items. */
    data object Preparing : SheetState()

    /** One device: "Sending 3 items to A" with Cancel for 2 s before anything goes (SR-29). */
    data class Countdown(val title: String) : SheetState()

    /** The device chooser. */
    data class Choose(val selection: Selection, val kind: SendKind, val items: Int) : SheetState() {
        /** "Send" is enabled: any ticked set goes, clips included (one push to the chosen devices). */
        val canSend: Boolean get() = selection.canSend
    }

    /** After "Send": "Sending 3 items to A and B" with a Cancel that stops all of them. */
    data class Sending(val title: String) : SheetState()
}

/**
 * The state and the waits behind the send card shared by the share target, the "Send to"
 * and the "Send clipboard" notification actions. The activity drives the flow with the
 * suspend functions; the card ([SendSheetContent]) calls the action functions.
 *
 * @param onMakeDefault stores a new default device; the sheet moves its marker.
 * @param windowMs how long a card offers Cancel; [CANCEL_WINDOW_MS] except in tests.
 */
internal class SendSheet(
    private val onMakeDefault: (String) -> Unit,
    private val windowMs: Long = CANCEL_WINDOW_MS,
) {
    var state by mutableStateOf<SheetState>(SheetState.Hidden)
        private set

    private val cancelled = CompletableDeferred<Unit>()
    private val left = CompletableDeferred<Unit>()
    private var choice: CompletableDeferred<List<String>?>? = null
    private var sendingTitle: ((List<String>) -> String)? = null

    /** Shows [s] without waiting (Hidden, Preparing). */
    fun show(s: SheetState) {
        state = s
    }

    /**
     * Shows the countdown card for the cancel window. True when it ran out, false when the
     * person cancelled or the card left the screen.
     */
    suspend fun countdown(title: String): Boolean {
        state = SheetState.Countdown(title)
        return withTimeoutOrNull(windowMs) { cancelled.await() } == null && !cancelled.isCompleted
    }

    /**
     * Shows the chooser; the ticked device IDs in row order once "Send" is tapped, null on
     * Cancel. With [sendingTitle] the tap on "Send" turns the card into the sending card
     * ("Sending 3 items to A and B", with Cancel) at once, so the chooser takes no further
     * taps while the sends are being prepared; without it the card goes away.
     */
    suspend fun choose(
        selection: Selection,
        kind: SendKind,
        items: Int,
        sendingTitle: ((List<String>) -> String)? = null,
    ): List<String>? {
        if (cancelled.isCompleted) return null
        val d = CompletableDeferred<List<String>?>()
        choice = d
        this.sendingTitle = sendingTitle
        state = SheetState.Choose(selection, kind, items)
        return d.await().also { choice = null }
    }

    /**
     * Shows the sending card until it has been up for [CANCEL_WINDOW_MS] and [ready] is done
     * (every send started), or until Cancel. True when the person cancelled. Leaving the
     * screen ends the wait without cancelling: the person already tapped "Send". Once
     * [finished] is done (every send ended) Cancel has nothing left to stop, so the card
     * goes at once and the result is reported.
     */
    suspend fun holdSending(title: String, ready: Deferred<Unit>, finished: Deferred<Unit>? = null): Boolean = coroutineScope {
        state = SheetState.Sending(title)
        val window = async {
            delay(windowMs)
            ready.await()
        }
        val wasCancelled = select {
            cancelled.onAwait { true }
            left.onAwait { false }
            window.onAwait { false }
            finished?.onAwait { false }
        }
        window.cancel()
        wasCancelled
    }

    /** Cancel, back or a tap on the scrim. */
    fun cancel() {
        cancelled.complete(Unit)
        choice?.complete(null)
    }

    /**
     * The card left the screen. Before "Send" that is a Cancel (SR-29); once sending, the
     * sends go on.
     */
    fun leave() {
        if (state is SheetState.Sending) left.complete(Unit) else cancel()
    }

    /** "Send" in the chooser: leaves the chooser at once (see [choose]). */
    fun send() {
        val s = state as? SheetState.Choose ?: return
        if (!s.canSend) return
        val chosen = s.selection.chosen
        state = sendingTitle?.let { SheetState.Sending(it(chosen)) } ?: SheetState.Hidden
        choice?.complete(chosen)
    }

    /** A tap on a chooser row. */
    fun toggle(id: String) = updateChoice { it.toggle(id) }

    /** "Select all" or "Clear". */
    fun toggleAll() = updateChoice { it.toggleAll() }

    /** "Make default" on a chooser row. */
    fun makeDefault(id: String) {
        onMakeDefault(id)
        updateChoice { it.withDefault(id) }
    }

    private fun updateChoice(change: (Selection) -> Selection) {
        val s = state as? SheetState.Choose ?: return
        state = s.copy(selection = change(s.selection))
    }

    companion object {
        /** How long a card offers Cancel (SR-29). */
        const val CANCEL_WINDOW_MS = 2_000L
    }
}
