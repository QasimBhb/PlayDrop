package org.playdrop.android.share

import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Starts one send per target, in order, and can stop all of them (SR-29: the Cancel on the
 * "Sending to A and B" card). After [cancelAll] no further send starts, every send that was
 * already started is stopped, and a send whose start was in flight is stopped as soon as it
 * returns its ID. Pure apart from the three callbacks, so the rules are unit-tested.
 *
 * @param start starts the send for a target and returns its transfer ID; on an exception it
 *   must release whatever it holds for that target itself.
 * @param stop cancels a started transfer by ID.
 * @param release frees what a target holds when its send is never started.
 */
internal class FanOut<T>(
    private val targets: List<T>,
    private val start: suspend (T) -> String,
    private val stop: suspend (String) -> Unit,
    private val release: (T) -> Unit,
) {
    /** How one target ended up. */
    sealed class Outcome<T> {
        abstract val target: T

        /** Started and not cancelled (so far). */
        data class Started<T>(override val target: T, val id: String) : Outcome<T>()

        /** The start failed with [error]. */
        data class Failed<T>(override val target: T, val error: Exception) : Outcome<T>()

        /** Cancel came first, so it never started. */
        data class Skipped<T>(override val target: T) : Outcome<T>()

        /** It started while Cancel was being handled and was stopped at once. */
        data class Stopped<T>(override val target: T, val id: String) : Outcome<T>()
    }

    private val lock = Mutex()
    private var cancelled = false
    private val startedIds = mutableListOf<String>()

    /** True once [cancelAll] was called. */
    suspend fun isCancelled(): Boolean = lock.withLock { cancelled }

    /** Starts every target in order; returns one outcome per target. Call once. */
    suspend fun run(): List<Outcome<T>> {
        val out = ArrayList<Outcome<T>>(targets.size)
        for (t in targets) {
            if (lock.withLock { cancelled }) {
                runCatching { release(t) }
                out += Outcome.Skipped(t)
                continue
            }
            val id = try {
                start(t)
            } catch (e: Exception) {
                out += Outcome.Failed(t, e)
                continue
            }
            val late = lock.withLock {
                if (!cancelled) startedIds += id
                cancelled
            }
            if (late) {
                runCatching { stop(id) }
                out += Outcome.Stopped(t, id)
            } else {
                out += Outcome.Started(t, id)
            }
        }
        return out
    }

    /** Stops every started send and keeps the rest from starting. Safe to call more than once. */
    suspend fun cancelAll() {
        val ids = lock.withLock {
            cancelled = true
            startedIds.toList().also { startedIds.clear() }
        }
        ids.forEach { id -> runCatching { stop(id) } }
    }
}
