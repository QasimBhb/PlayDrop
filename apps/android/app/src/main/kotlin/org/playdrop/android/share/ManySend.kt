package org.playdrop.android.share

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.selects.select
import kotlinx.coroutines.withTimeoutOrNull
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * How one started send ended, from the core's `TransferComplete` and `TransferFailed` events
 * (see [sendEndOf]). [completed]: every file arrived. [byCancel]: it was cancelled on this
 * device. [reason]: the core's plain sentence for a failure, empty when it completed.
 */
internal data class SendEnd(val id: String, val completed: Boolean, val byCancel: Boolean, val reason: String)

/**
 * A send to several devices after "Send" in the chooser, owned by a process-wide [scope] so
 * it does not depend on the activity that showed the chooser: pressing Home, rotating or
 * leaving keeps the sends going, and the card of a recreated activity finds it again by
 * [token] ([ManySends.find]). It prepares one share of the items per device ([prepare]),
 * then starts one send per device in order ([FanOut]), and follows each started send to its
 * end through [ends], so the report says what really happened on every device.
 *
 * [cancel] (the card's Cancel) stops every started send that has not finished and starts
 * none of the rest, also while the items are still being prepared; [keep] (the card closed
 * without Cancel) lets the result be reported. Once every send has ended ([allFinished])
 * there is nothing left to cancel, so the result is reported at once. One [report] ends
 * every run. Pure apart from the callbacks, so the rules are unit-tested.
 *
 * @param prepare returns one share of the items per device for [peers]' count, or for 0 when
 *   Cancel came first (then it only lets go of what it holds). Called once.
 * @param start starts the send of one share to one device and returns its transfer ID; on an
 *   exception it must release that share itself.
 * @param stop cancels a started transfer by ID.
 * @param release lets go of a share whose send never started.
 * @param ends the end of every send of this device (any send, not only these; matched by
 *   ID). Collected from [launch] on, before the first send starts, so a send that ends at
 *   once is not missed.
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
    private val ends: Flow<SendEnd> = emptyFlow(),
    /** How long after every send started the card may still decide before the result goes out. */
    private val decideWithinMs: Long = DECIDE_WITHIN_MS,
    /** After Cancel, how long to wait for the core to say how each stopped send ended. */
    private val settleWithinMs: Long = SETTLE_WITHIN_MS,
) {
    /** How a run ended, for the one message after it. */
    sealed class Report {
        /** Cancel was tapped before any send started: nothing was sent. */
        data object Cancelled : Report()

        /** The items could not be prepared for every device; nothing was sent. */
        data class PrepareFailed(val error: Exception) : Report()

        /**
         * One result per device, in [peers] order. [cancelled]: Cancel was tapped after the
         * sends began (devices that had already received everything are [Device.Completed]).
         */
        data class Done(val devices: List<Device>, val cancelled: Boolean) : Report()
    }

    /** How the send to one device (index [target] into the peers) ended up. */
    sealed class Device {
        abstract val target: Int

        /** The core reported that every file arrived. */
        data class Completed(override val target: Int) : Device()

        /** Started and still going (not cancelled). */
        data class Running(override val target: Int, val id: String) : Device()

        /** Started, then stopped by Cancel before it finished. */
        data class Stopped(override val target: Int) : Device()

        /** Cancel came first, so it never started. */
        data class NotStarted(override val target: Int) : Device()

        /** The start failed with [error]. */
        data class FailedToStart(override val target: Int, val error: Exception) : Device()

        /** Started, then failed for [reason] (the core's plain sentence). */
        data class Failed(override val target: Int, val reason: String) : Device()
    }

    /** Finds this run again after the activity was recreated. */
    val token: Long = nextToken.incrementAndGet()

    private val decided = CompletableDeferred<Boolean>()
    private val started = CompletableDeferred<Unit>()
    private val finished = CompletableDeferred<Unit>()

    /** Done once every send was started, failed to start or was skipped (or preparing failed). */
    val allStarted: Deferred<Unit> get() = started

    /**
     * Done once no send is left going: every device's send completed, failed, was stopped or
     * never started. From then on Cancel has nothing to stop.
     */
    val allFinished: Deferred<Unit> get() = finished

    @Volatile
    private var shares: List<V>? = null

    /** Set at once by [cancel], before the fan-out hears of it. */
    @Volatile
    private var cancelRequested = false

    /** The fan-out's outcomes once every send was started or skipped. */
    @Volatile
    private var outcomes: List<FanOut.Outcome<Int>>? = null

    /** Ends seen so far, by transfer ID; an end may arrive before its start returned. */
    private val ended = ConcurrentHashMap<String, SendEnd>()

    private val fan = FanOut(
        targets = peers.indices.toList(),
        start = { i -> start(peers[i], checkNotNull(shares)[i]) },
        stop = { id -> stop(id) },
        release = { i -> shares?.getOrNull(i)?.let(release) },
    )

    /** Starts the run in [scope]; call once. */
    fun launch(): Job {
        ManySends.add(this)
        // Undispatched, so the subscription is in place before the first send can start.
        val watcher = scope.launch(start = CoroutineStart.UNDISPATCHED) {
            ends.collect { e ->
                ended[e.id] = e
                checkFinished()
            }
        }
        return scope.launch {
            try {
                run()
            } finally {
                started.complete(Unit)
                finished.complete(Unit)
                watcher.cancel()
                ManySends.remove(token)
            }
        }
    }

    /** Stops every started send that has not finished and starts no more (the card's Cancel). */
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
        val out = try {
            fan.run()
        } finally {
            started.complete(Unit)
        }
        outcomes = out
        checkFinished()
        // The card decides within its window; without a decision the sends simply go on.
        // Once every send has ended there is nothing to decide.
        val decision = withTimeoutOrNull(decideWithinMs) {
            select {
                decided.onAwait { it }
                finished.onAwait { false }
            }
        } ?: false
        val cancelled = decision || cancelRequested
        if (cancelled) {
            // Every started send is stopped before the report (cancel() began it already;
            // this waits), then the core gets a moment to say how each one ended: one that
            // finished just before the stop is reported as sent, not as stopped.
            fan.cancelAll()
            withTimeoutOrNull(settleWithinMs) { finished.await() }
        }
        report(Report.Done(out.map { device(it, cancelled) }, cancelled))
    }

    /** Completes [allFinished] once every started send has an end. */
    private fun checkFinished() {
        val out = outcomes ?: return
        val open = out.any { o ->
            when (o) {
                is FanOut.Outcome.Started -> !ended.containsKey(o.id)
                is FanOut.Outcome.Stopped -> !ended.containsKey(o.id)
                is FanOut.Outcome.Failed, is FanOut.Outcome.Skipped -> false
            }
        }
        if (!open) finished.complete(Unit)
    }

    /** The truthful result for one device; [cancelled] when Cancel was tapped after the starts. */
    private fun device(o: FanOut.Outcome<Int>, cancelled: Boolean): Device = when (o) {
        is FanOut.Outcome.Skipped -> Device.NotStarted(o.target)
        is FanOut.Outcome.Failed -> Device.FailedToStart(o.target, o.error)
        is FanOut.Outcome.Started -> afterStart(o.target, o.id, cancelled)
        is FanOut.Outcome.Stopped -> afterStart(o.target, o.id, cancelled = true)
    }

    private fun afterStart(target: Int, id: String, cancelled: Boolean): Device {
        val end = ended[id]
        return when {
            end?.completed == true -> Device.Completed(target)
            end != null && (end.byCancel || cancelled) -> Device.Stopped(target)
            end != null -> Device.Failed(target, end.reason)
            // No end heard: Cancel sent its stop, so it did not finish; otherwise it goes on.
            cancelled -> Device.Stopped(target)
            else -> Device.Running(target, id)
        }
    }

    companion object {
        /** Long enough for a rotation and the card's window; the card normally decides at once. */
        const val DECIDE_WITHIN_MS = 15_000L

        /** How long Cancel waits for the core's word on each stopped send. */
        const val SETTLE_WITHIN_MS = 3_000L

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
