package org.playdrop.android.core

import android.content.Context
import org.playdrop.android.data.SendHistory
import org.playdrop.android.data.SendHistoryEntry
import org.playdrop.android.data.SendHistoryStore
import org.playdrop.android.data.SendOutcome
import org.playdrop.android.data.Settings
import org.playdrop.android.service.FailureText
import org.playdrop.core.HostEvent
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferFailure

/** Where [SendHistoryRecorder] writes; the real one is the app's file, a test uses memory. */
internal interface SendHistoryBackend {
    /** Records [entry] unless [allowed] says no; [allowed] is asked under the store's lock. */
    suspend fun upsert(entry: SendHistoryEntry, allowed: suspend () -> Boolean)

    /** Fills in the device name, counts and names of an entry that lacked them. */
    suspend fun enrich(transferId: String, peerLabel: String, itemCount: Long, totalBytes: Long, names: List<String>)

    /** Ends a still-resumable failed entry as cancelled; true when an entry changed. */
    suspend fun forgotten(transferId: String): Boolean
}

/** The end of one send as the recorder needs it, free of the generated event types. */
internal sealed interface SendEndInfo {
    val id: String
    val peer: String

    /** Every file arrived; [paths] are the manifest paths. */
    data class Done(override val id: String, override val peer: String, val bytes: Long, val paths: List<String>) : SendEndInfo

    /** It stopped; [reason] is the core's own text. */
    data class Stopped(override val id: String, override val peer: String, val code: TransferFailure, val reason: String) : SendEndInfo
}

/**
 * The rules of the "Sent" history, apart from Android so they are unit-tested. A send is noted
 * when it starts ([noteSend]: only files, never the clipboard, which does not go through here)
 * and written once its final outcome is known ([onEnd]), in the process that hosts the core,
 * so a send that finishes with every screen closed is still recorded. One entry per send: a
 * failed send that resumes and completes ends as one entry. Nothing is written while
 * [keep] says the switch is off, and that is asked under the store's lock. File names are
 * never logged.
 */
internal class SendHistoryRecorder(
    private val backend: SendHistoryBackend,
    private val keep: suspend () -> Boolean,
    private val now: () -> Long = System::currentTimeMillis,
    private val maxPending: Int = MAX_PENDING,
) {
    private class Pending(val peerLabel: String, val names: List<String>, val itemCount: Long, val totalBytes: Long)

    private val lock = Any()

    /** Sends noted and not ended, oldest first; the oldest leaves when the cap is hit. */
    private val pending = LinkedHashMap<String, Pending>()

    /** Sends whose end was seen, newest last: a start noted after its end only fills in the entry. */
    private val ended = LinkedHashSet<String>()

    /** How many sends are waiting for their end (for tests). */
    internal val pendingCount: Int get() = synchronized(lock) { pending.size }

    /**
     * Remembers what [id] carries, under the device's name as it is now. When the send already
     * ended (a fast one: its end was handled before this call), the entry it wrote gets the
     * details instead, and nothing is kept.
     */
    suspend fun noteSend(id: String, peerLabel: String, names: List<String>, itemCount: Int, totalBytes: Long) {
        val cleaned = SendHistory.firstNames(names)
        val late = synchronized(lock) {
            if (id in ended) {
                true
            } else {
                while (pending.size >= maxPending) pending.remove(pending.keys.first())
                pending[id] = Pending(peerLabel, cleaned, itemCount.toLong(), totalBytes)
                false
            }
        }
        if (late) backend.enrich(id, peerLabel, itemCount.toLong(), totalBytes, cleaned)
    }

    /**
     * Writes the entry for the end of a send. [shapeOf] is the fallback count and size for a
     * send this process did not start (resumed after a restart); [resumableOf] says the core
     * keeps the record of a failed send, so it carries on by itself; [labelOf] names the device.
     */
    suspend fun onEnd(
        end: SendEndInfo,
        labelOf: (String) -> String,
        shapeOf: (String) -> TransferShape?,
        resumableOf: (String) -> Boolean,
    ) {
        val entry = when (end) {
            is SendEndInfo.Done -> {
                val known = take(end.id, keep = false)
                SendHistory.entry(
                    transferId = end.id,
                    atMs = now(),
                    peerLabel = known?.peerLabel ?: labelOf(end.peer),
                    itemCount = known?.itemCount ?: end.paths.size.toLong(),
                    totalBytes = known?.totalBytes ?: end.bytes,
                    names = known?.names ?: end.paths,
                    outcome = SendOutcome.SENT,
                )
            }
            is SendEndInfo.Stopped -> {
                val cancelled = end.code == TransferFailure.CANCELLED_BY_YOU
                // A failed send whose record is kept goes on by itself: its details stay until the end.
                val resumable = !cancelled && resumableOf(end.id)
                val known = take(end.id, keep = resumable)
                val shape = if (known == null) shapeOf(end.id) else null
                SendHistory.entry(
                    transferId = end.id,
                    atMs = now(),
                    peerLabel = known?.peerLabel ?: labelOf(end.peer),
                    itemCount = known?.itemCount ?: shape?.fileCount ?: 0,
                    totalBytes = known?.totalBytes ?: shape?.totalBytes ?: 0,
                    names = known?.names.orEmpty(),
                    outcome = if (cancelled) SendOutcome.CANCELLED else SendOutcome.FAILED,
                    failure = end.code.name.takeIf { !cancelled },
                    // Only the core's own words for a failure it has no code for, and only when
                    // they are plain (no HTTP status or wire code).
                    detail = if (end.code == TransferFailure.OTHER) FailureText.plainOrNull(end.reason) else null,
                    resumable = resumable,
                )
            }
        }
        backend.upsert(entry, keep)
    }

    /**
     * "Forget" on a failed send waiting to resume: its one entry ends as cancelled. Only then
     * is the send's note dropped; a cancel of a running send leaves it for its own end event.
     */
    suspend fun forgotten(transferId: String) {
        if (backend.forgotten(transferId)) synchronized(lock) { pending.remove(transferId) }
    }

    /** The note of [id], removed unless [keep] (a failed send that will resume). */
    private fun take(id: String, keep: Boolean): Pending? = synchronized(lock) {
        ended += id
        while (ended.size > MAX_ENDED) ended.remove(ended.first())
        if (keep) pending[id] else pending.remove(id)
    }

    companion object {
        /** Running sends noted at once; the oldest is forgotten past this. */
        const val MAX_PENDING = 256

        /** Ended sends remembered, to tell a late start note from a new send. */
        const val MAX_ENDED = 256
    }
}

/** The app's recorder, wired to the file store and the Settings switch. */
internal object SendHistoryLog {
    @Volatile
    private var appContext: Context? = null

    private val recorder = SendHistoryRecorder(
        backend = object : SendHistoryBackend {
            override suspend fun upsert(entry: SendHistoryEntry, allowed: suspend () -> Boolean) =
                SendHistoryStore.upsert(checkNotNull(appContext), entry, allowed)

            override suspend fun enrich(transferId: String, peerLabel: String, itemCount: Long, totalBytes: Long, names: List<String>) =
                SendHistoryStore.enrich(checkNotNull(appContext), transferId, peerLabel, itemCount, totalBytes, names)

            override suspend fun forgotten(transferId: String): Boolean =
                SendHistoryStore.forgotten(checkNotNull(appContext), transferId)
        },
        keep = { Settings.read(checkNotNull(appContext)).keepSendHistory },
    )

    /** Remembers what [id] carries, under the device's name as it is now. */
    suspend fun noteSend(context: Context, id: String, peer: String, names: List<String>, itemCount: Int, totalBytes: Long) {
        appContext = context.applicationContext
        // The history must never make a send look like it failed to start.
        try {
            recorder.noteSend(id, CoreHost.labelOf(peer), names, itemCount, totalBytes)
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            HostLog.w("send", "noting the send for the history failed", e)
        }
    }

    /** Writes the entry when [ev] is the end of a send. */
    suspend fun onEnd(
        app: Context,
        ev: HostEvent,
        labelOf: (String) -> String,
        shapeOf: (String) -> TransferShape?,
        resumableOf: (String) -> Boolean,
    ) {
        appContext = app.applicationContext
        val end = endOf(ev) ?: return
        recorder.onEnd(end, labelOf, shapeOf, resumableOf)
    }

    /** "Forget" on a failed send waiting to resume. */
    suspend fun forgotten(app: Context, transferId: String) {
        appContext = app.applicationContext
        recorder.forgotten(transferId)
    }

    /** The end of one of this phone's sends, or null for any other event. */
    internal fun endOf(ev: HostEvent): SendEndInfo? = when {
        ev is HostEvent.TransferComplete && ev.direction == TransferDirection.SEND ->
            SendEndInfo.Done(ev.id, ev.peer, ev.bytes.toLong(), ev.roots.map { it.path })
        ev is HostEvent.TransferFailed && ev.direction == TransferDirection.SEND ->
            SendEndInfo.Stopped(ev.id, ev.peer, ev.code, ev.reason)
        else -> null
    }
}
