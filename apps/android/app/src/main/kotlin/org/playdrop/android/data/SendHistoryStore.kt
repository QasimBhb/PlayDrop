package org.playdrop.android.data

import android.content.Context
import android.util.Log
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException

/**
 * Where the "Sent" history lives: one small text file in the app's private storage (never
 * backed up, see `data_extraction_rules`). The rules are in [SendHistory]; this only reads and
 * writes. A file that cannot be read counts as empty and a failed write is logged without
 * the content, so the history can never break a send.
 */
object SendHistoryStore {
    private const val FILE = "send-history.txt"

    private val lock = Mutex()
    private val state = MutableStateFlow<List<SendHistoryEntry>?>(null)

    /** The history, newest first; null until it was read once. Call [load] first. */
    val entries: StateFlow<List<SendHistoryEntry>?> = state

    /** Reads the file into [entries] once. */
    suspend fun load(context: Context) {
        lock.withLock { ensureLoaded(context) }
    }

    /**
     * Records [entry] when [allowed] says so, checked under the store's lock so a switch turned
     * off (and the clear that follows it) can never be overtaken by a write in flight.
     */
    suspend fun upsert(context: Context, entry: SendHistoryEntry, allowed: suspend () -> Boolean) {
        lock.withLock {
            if (!allowed()) return
            val next = SendHistory.upsert(ensureLoaded(context), entry)
            state.value = next
            write(context, next)
        }
    }

    /** Fills in what an entry lacked (see [SendHistory.enriched]). */
    suspend fun enrich(context: Context, transferId: String, peerLabel: String, itemCount: Long, totalBytes: Long, names: List<String>) {
        lock.withLock {
            val before = ensureLoaded(context)
            if (before.none { it.transferId == transferId }) return
            val next = SendHistory.enriched(before, transferId, peerLabel, itemCount, totalBytes, names)
            if (next == before) return
            state.value = next
            write(context, next)
        }
    }

    /** Ends a forgotten, still resumable failed send as cancelled; true when an entry changed. */
    suspend fun forgotten(context: Context, transferId: String): Boolean = lock.withLock {
        val before = ensureLoaded(context)
        val next = SendHistory.forgotten(before, transferId)
        if (next == before) return@withLock false
        state.value = next
        write(context, next)
        true
    }

    /** Drops "will carry on" claims the core no longer backs (see [SendHistory.reconcile]). */
    suspend fun reconcile(context: Context, listed: Set<String>?, nowMs: Long) {
        lock.withLock {
            val before = ensureLoaded(context)
            val next = SendHistory.reconcile(before, listed, nowMs)
            if (next == before) return
            state.value = next
            write(context, next)
        }
    }

    /** Forgets every entry and deletes the file. */
    suspend fun clear(context: Context) {
        lock.withLock {
            state.value = emptyList()
            withContext(Dispatchers.IO) {
                try {
                    file(context).delete()
                } catch (e: SecurityException) {
                    Log.w(TAG, "send history delete failed (${e.javaClass.simpleName})")
                }
            }
        }
    }

    private suspend fun ensureLoaded(context: Context): List<SendHistoryEntry> {
        state.value?.let { return it }
        val read = withContext(Dispatchers.IO) {
            try {
                val f = file(context)
                if (f.exists()) SendHistory.decode(f.readText()) else emptyList()
            } catch (e: IOException) {
                Log.w(TAG, "send history read failed (${e.javaClass.simpleName})")
                emptyList()
            }
        }
        state.value = read
        return read
    }

    private suspend fun write(context: Context, history: List<SendHistoryEntry>) {
        withContext(Dispatchers.IO) {
            try {
                val target = file(context)
                val tmp = File(target.parentFile, "$FILE.tmp")
                tmp.writeText(SendHistory.encode(history))
                if (!tmp.renameTo(target)) {
                    target.delete()
                    if (!tmp.renameTo(target)) Log.w(TAG, "send history rename failed")
                }
            } catch (e: IOException) {
                Log.w(TAG, "send history write failed (${e.javaClass.simpleName})")
            }
        }
    }

    private fun file(context: Context) = File(context.applicationContext.filesDir, FILE)

    private const val TAG = "playdrop"
}
