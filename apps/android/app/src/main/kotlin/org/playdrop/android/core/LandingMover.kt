package org.playdrop.android.core

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import org.playdrop.android.service.Notifications
import org.playdrop.core.HostEvent
import org.playdrop.core.TransferDirection
import java.util.concurrent.ConcurrentHashMap

/**
 * Moves landed transfers from the app's landing folder into `Download/PlayDrop` (see
 * [DownloadsSink], "Landing"), one file at a time, on one coroutine. One instance per process.
 *
 * Why one serial queue: every job (a move after a transfer completes or ends early, the sweep
 * of what a killed process left, a retry) touches the same trees, and running them one after
 * another is what makes the whole thing idempotent without further locking:
 * - before each pass the journal of an unfinished copy is settled
 *   ([DownloadsSink.recoverJournal]); a pass that cannot settle it waits for a retry;
 * - a file is moved by a journaled copy followed by deleting the landed file (never by a
 *   rename into Downloads: MediaProvider serves that as a copy a kill cuts in half, see
 *   [DownloadsSink]), so a pass cut short at any point leaves each file either landed or in
 *   Downloads;
 *   if the landed file cannot be deleted the pass stops so no later copy overwrites that
 *   journal entry;
 * - a second pass over the same transfer finds only what the first one left;
 * - files that land while a pass runs (a resumed transfer racing a sweep) are not in that
 *   pass's list and are moved by the pass queued when that transfer ends; their folders are
 *   never removed under them, since only empty folders are pruned and the lander recreates a
 *   folder that vanished.
 *
 * Files a pass leaves behind are retried in the process ([MoveRetry]), when shared storage is
 * mounted again, and when the user taps "Try again" ([retryNow]); the service is always on,
 * so waiting for the next start could take days. No WorkManager: whatever a killed process
 * did not finish is picked up by [sweep] on the next start.
 */
internal class LandingMover(
    private val app: Context,
    private val sink: DownloadsSink,
    private val scope: CoroutineScope,
) {
    private sealed interface Job

    /**
     * One pass over a transfer's landing folder. [completed] is set after a completed
     * transfer, [ended] after a failed or cancelled one; neither for a sweep or a retry.
     * [attempt] counts the retries of this transfer in this process.
     */
    private data class Move(
        val transferId: String,
        val completed: HostEvent.TransferComplete? = null,
        val ended: HostEvent.TransferFailed? = null,
        val label: String? = null,
        val attempt: Int = 0,
    ) : Job

    /** Queues a pass for every transfer that still has landed files. */
    private data object SweepAll : Job

    private val queue = Channel<Job>(Channel.UNLIMITED)

    /** Transfers with a retry timer running, so a burst of failures schedules only one. */
    private val retryScheduled: MutableSet<String> = ConcurrentHashMap.newKeySet()

    init {
        // A "Moving" notification left by a killed process would stay ongoing for good.
        Notifications.clearStaleMoving(app)
        scope.launch {
            for (job in queue) {
                try {
                    when (job) {
                        is Move -> move(job)
                        SweepAll -> sink.landedTransferIds().forEach { queue.trySend(Move(it)) }
                    }
                } catch (e: Exception) {
                    HostLog.w("landing", "a landing job failed; it is retried later", e)
                }
            }
        }
        registerMountWatch()
    }

    /** Queues a pass for each of [transferIds] (the leftovers found at start). */
    fun sweep(transferIds: List<String>) {
        transferIds.forEach { queue.trySend(Move(it)) }
    }

    /** Retries every transfer with landed files now (the notice's "Try again", a remount). */
    fun retryNow() {
        queue.trySend(SweepAll)
    }

    /** Queues the move of a completed transfer; its "received" notification is posted after it. */
    fun afterComplete(event: HostEvent.TransferComplete, label: String) {
        queue.trySend(Move(event.id, completed = event, label = label))
    }

    /**
     * Queues the move of what a failed or cancelled receive landed. The caller checked that
     * the transfer really ended ([TransferEnds]). Every landed file was verified against its
     * root already, and the direct path keeps the files it delivered when a later file fails or
     * the user cancels (the core then only removes its staging `.part` files), so landed files
     * are kept the same way. A transfer that only lost its connection gets no such event: the
     * core resumes it and its landing folder stays.
     */
    fun afterEnded(event: HostEvent.TransferFailed, label: String) {
        if (event.direction != TransferDirection.RECEIVE) return
        queue.trySend(Move(event.id, ended = event, label = label))
    }

    private fun move(job: Move) {
        val tid = job.transferId
        if (!sink.recoverJournal()) {
            // An unfinished copy could not be settled: no file may be copied until it is.
            val left = sink.landedFiles(tid).size
            Notifications.moveFinished(app, tid, job.completed, job.ended != null, job.label, 0, left, renamed = 0, forceFolder = true)
            if (left > 0) scheduleRetry(job)
            return
        }
        val files = sink.landedFiles(tid)
        if (files.isEmpty()) {
            sink.pruneLanding(tid)
            sink.moveEnded(tid)
            if (job.completed != null) Notifications.moveFinished(app, tid, job.completed, false, job.label, 0, 0, renamed = 0)
            return
        }
        val t0 = System.nanoTime()
        val tally = MoveTally(files.size)
        val throttle = ProgressThrottle()
        try {
            for (f in files) {
                val result = sink.moveLanded(tid, f)
                tally.record(result)
                if (result.stopPass) break
                if (throttle.due(tally.done, tally.total, System.nanoTime())) {
                    Notifications.moving(app, tid, tally.done, tally.total)
                }
            }
        } finally {
            // Whatever happened above, the ongoing "Moving" notice is replaced by a final one.
            if (tally.done < tally.total) tally.skipRest()
            runCatching { sink.moveEnded(tid) }
            runCatching { sink.pruneLanding(tid) }
            val ms = (System.nanoTime() - t0) / 1_000_000
            val line = "transfer ${tid.take(8)}: moved into Downloads in $ms ms: ${tally.line()}"
            if (tally.failed > 0) {
                HostLog.w("landing", "$line; the rest stays in ${sink.landingDir(tid)?.path}")
            } else {
                HostLog.i("landing", line)
            }
            Notifications.moveFinished(
                app, tid, job.completed, job.ended != null, job.label, tally.moved, tally.failed,
                renamed = tally.renumbered,
            )
            if (tally.failed > 0) scheduleRetry(job)
        }
    }

    /** Queues the next retry of [job]'s transfer after [MoveRetry]'s delay, unless one is pending. */
    private fun scheduleRetry(job: Move) {
        val tid = job.transferId
        val wait = MoveRetry.delayMs(job.attempt)
        if (wait == null) {
            HostLog.w("landing", "transfer ${tid.take(8)}: giving up retrying for now; storage remount, Try again or a restart retries it")
            return
        }
        if (!retryScheduled.add(tid)) return
        scope.launch {
            delay(wait)
            retryScheduled.remove(tid)
            queue.trySend(Move(tid, attempt = job.attempt + 1))
        }
    }

    /** Retries when shared storage comes back (it can be unmounted, for example over USB mass storage). */
    private fun registerMountWatch() {
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                HostLog.i("landing", "storage mounted; retrying landed files")
                retryNow()
            }
        }
        val filter = IntentFilter(Intent.ACTION_MEDIA_MOUNTED).apply { addDataScheme("file") }
        try {
            ContextCompat.registerReceiver(app, receiver, filter, ContextCompat.RECEIVER_NOT_EXPORTED)
        } catch (e: Exception) {
            HostLog.w("landing", "could not watch for storage mounts; retries use the timer only", e)
        }
    }
}
