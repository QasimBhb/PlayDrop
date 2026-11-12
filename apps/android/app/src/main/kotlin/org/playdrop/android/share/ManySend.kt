package org.playdrop.android.share

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * A send to several devices after "Send" in the chooser, owned by a process-wide [scope] so
 * it does not depend on the activity that showed the chooser: pressing Home, rotating or
 * leaving keeps the sends going, and the card of a recreated activity finds it again by
 * [token] ([ManySends.find]). It prepares one share of the items per device ([prepare]),
 * then starts one send per device in order ([FanOut]).
 *
 * [cancel] (the card's Cancel) stops every started send and starts none of the rest, also
 * while the items are still being prepared; [keep] (the card closed without Cancel) lets the
 * result be reported. One [report] ends every run. Pure apart from the callbacks, so the
 * rules are unit-tested.
 *
 * @param prepare returns one share of the items per device for [peers]' count, or for 0 when
 *   Cancel came first (then it only lets go of what it holds). Called once.
 * @param start starts the send of one share to one device and returns its transfer ID; on an
 *   exception it must release that share itself.
 * @param stop cancels a started transfer by ID.
 * @param release lets go of a share whose send never started.
 */
internal class ManySend<V>(
    /** "Sending 3 items to A and B", for the card. */
    val title: String,
    private val peers: List<String>,
    private val scope: CoroutineScope,
    private val prepare: suspend (Int) -> List<V>,
    private val start: suspend (String, V) -> String,
    private val stop: suspend (String) -> Unit,
    private val release: (V) -> Unit,
    private val report: suspend (Report) -> Unit,
    /** How long after every send started the card may still decide before the result goes out. */
    private val decideWithinMs: Long = DECIDE_WITHIN_MS,
) {
    /** How a run ended, for the one message after it. */
    sealed class Report {
        /** Cancel was tapped: what had started was stopped. */
        data object Cancelled : Report()

        /** The items could not be prepared for every device; nothing was sent. */
        data class PrepareFailed(val error: Exception) : Report()

        /** One outcome per device, in [peers] order. */
        data class Done(val outcomes: List<FanOut.Outcome<Int>>) : Report()
    }

    /** Finds this run again after the activity was recreated. */
    val token: Long = nextToken.incrementAndGet()

    private val decided = CompletableDeferred<Boolean>()
    private val started = CompletableDeferred<Unit>()

    /** Done once every send was started, failed to start or was skipped (or preparing failed). */
    val allStarted: Deferred<Unit> get() = started

    @Volatile
    private var shares: List<V>? = null

    /** Set at once by [cancel], before the fan-out hears of it. */
    @Volatile
    private var cancelRequested = false

    private val fan = FanOut(
        targets = peers.indices.toList(),
        start = { i -> start(peers[i], checkNotNull(shares)[i]) },
        stop = { id -> stop(id) },
        release = { i -> shares?.getOrNull(i)?.let(release) },
    )

    /** Starts the run in [scope]; call once. */
    fun launch(): Job {
        ManySends.add(this)
        return scope.launch {
            try {
                run()
            } finally {
                started.complete(Unit)
                ManySends.remove(token)
            }
        }
    }

    /** Stops every started send and starts no more (the card's Cancel). */
    fun cancel() {
        cancelRequested = true
        if (decided.complete(true)) scope.launch { fan.cancelAll() }
    }

    /** The card closed without Cancel: the sends go on and the result is reported. */
    fun keep() {
        decided.complete(false)
    }

    private suspend fun run() {
        val prepared = try {
            prepare(if (cancelRequested) 0 else peers.size)
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            started.complete(Unit)
            report(if (cancelRequested) Report.Cancelled else Report.PrepareFailed(e))
            return
        }
        shares = prepared
        if (prepared.size != peers.size || cancelRequested) {
            // Cancel came before or while preparing: nothing starts.
            prepared.forEach { runCatching { release(it) } }
            started.complete(Unit)
            report(Report.Cancelled)
            return
        }
        val outcomes = try {
            fan.run()
        } finally {
            started.complete(Unit)
        }
        // The card decides within its window; without a decision the sends simply go on.
        val cancelled = (withTimeoutOrNull(decideWithinMs) { decided.await() } ?: false) || cancelRequested
        // Every started send is stopped before the report (cancel() began it already; this waits).
        if (cancelled) fan.cancelAll()
        report(if (cancelled) Report.Cancelled else Report.Done(outcomes))
    }

    companion object {
        /** Long enough for a rotation and the card's window; the card normally decides at once. */
        const val DECIDE_WITHIN_MS = 15_000L

        private val nextToken = AtomicLong()
    }
}

/** The runs whose card may still be on screen, so a recreated activity can show it again. */
internal object ManySends {
    private val active = ConcurrentHashMap<Long, ManySend<*>>()

    fun add(run: ManySend<*>) {
        active[run.token] = run
    }

    fun remove(token: Long) {
        active.remove(token)
    }

    /** The run with [token] while it has not ended, else null. */
    fun find(token: Long): ManySend<*>? = active[token]
}
