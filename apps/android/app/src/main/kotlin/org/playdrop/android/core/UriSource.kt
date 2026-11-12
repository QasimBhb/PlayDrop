package org.playdrop.android.core

import android.content.ContentResolver
import android.content.Context
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.system.Os
import android.system.OsConstants
import org.playdrop.core.PlayDropException
import org.playdrop.core.SendSource
import org.playdrop.core.SourceOpener
import java.io.File
import java.io.FileNotFoundException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Turns `content://` URIs (share sheet, photo picker, document picker) into
 * [SendSource.Stream] items the core can send without ever seeing a path (anti-pattern 20).
 *
 * URI read grants from a share or a picker end with the activity that received them, while
 * the transfer keeps going in the service and the core reopens the file for every upload and
 * resume. So each item keeps one descriptor open from the moment it is prepared ([Prepared]),
 * and that one set is shared by every send of the items, to as many devices as were chosen:
 * the set is reference counted and closed when the last send ends ([PreparedSend.release]).
 *
 * When a later reopen through the provider is refused, an item is read through its held
 * descriptor without ever sharing a file offset between two sends (two sends reading through
 * one offset would read each other's bytes):
 * - most items are reopened read-only through `/proc/self/fd/<held>`, which gives each open
 *   its own offset; [prepare] tries this once per item, since a provider's file can be one
 *   this app may not open by itself (another app's private file, or one under a media
 *   folder's own permissions);
 * - for an item where that reopen does not work, the first send reads a duplicate of the
 *   held descriptor and every further send gets a descriptor of its own, opened from the
 *   provider while the grant is still valid ([Prepared.sends]); such a duplicate is read by
 *   that one send only, which opens an item once at a time and seeks before it reads.
 *
 * Only seekable regular files are accepted: a pipe or a socket can neither be resumed nor
 * reopened. Persisted read grants are never taken: the held descriptors cover the life of a
 * transfer, and after a process restart a send resumes only when the same items are shared
 * or picked again (the core matches them), which brings a fresh grant.
 */
object UriSource {
    /**
     * Most items one share or pick may carry. A 5,000 file share (the project's benchmark)
     * fits with room to spare; far beyond it the open descriptors would crowd the process
     * limit, which every socket of the app shares.
     */
    const val MAX_ITEMS = 20_000

    /** Holding the items open for the asked number of sends would use too many descriptors. */
    class TooManyItems(val items: Int) : Exception("too many items to hold open at once")

    /** One item: what the core is told about it and the descriptor held for it. */
    internal class Item(
        val uri: Uri,
        val name: String,
        val size: Long,
        val mtimeMs: Long,
        val mime: String?,
        val held: ParcelFileDescriptor,
        /** A read-only reopen through `/proc/self/fd` gives the same file. */
        val reopenable: Boolean,
    )

    /**
     * The prepared items and their held descriptors, shared by every send of them. The
     * preparer owns one reference ([release]); each [PreparedSend] from [sends] owns one more.
     * The descriptors close when the last reference goes.
     */
    class Prepared internal constructor(
        private val resolver: ContentResolver,
        private val items: List<Item>,
    ) {
        private val hold = SharedHold { items.forEach { runCatching { it.held.close() } } }

        /** How many items. */
        val count: Int get() = items.size

        /** Sum of the item sizes in bytes. */
        val totalBytes: Long = items.sumOf { it.size }

        /** Items whose further sends need descriptors of their own (see the class comment). */
        internal val notReopenable: Int = items.count { !it.reopenable }

        /**
         * One [PreparedSend] per send, [n] of them, all reading this one set. Blocking (opens
         * a descriptor per further send for each item that cannot be reopened): call on
         * `Dispatchers.IO`, while the grant is still valid. Throws [TooManyItems] when those
         * descriptors would not fit, or [PlayDropException.Io] when an item cannot be opened
         * again; nothing is held for the sends then. Does not drop the caller's reference.
         */
        fun sends(n: Int): List<PreparedSend> {
            require(n >= 1) { "at least one send" }
            val extra = FdBudget.extraFor(notReopenable, n)
            if (extra > 0 && extra > FdBudget.available()) throw TooManyItems(count)
            val own = mutableListOf<List<ParcelFileDescriptor?>>()
            try {
                for (k in 0 until n) {
                    own += items.map { item ->
                        when {
                            // The first send reads its duplicate of the held descriptor.
                            k == 0 || item.reopenable -> null
                            else -> openChecked(resolver, item.uri, item.name)
                        }
                    }
                }
                check(hold.retain(n)) { "the prepared items were already released" }
            } catch (e: Exception) {
                own.flatten().forEach { pfd -> runCatching { pfd?.close() } }
                throw e
            }
            return own.map { mine ->
                val sources = items.mapIndexed { i, item ->
                    SendSource.Stream(
                        name = item.name,
                        size = item.size.toULong(),
                        mtimeMs = item.mtimeMs,
                        mime = item.mime,
                        opener = UriOpener(resolver, item, mine[i] ?: item.held),
                    )
                }
                PreparedSend(sources, items.map { it.name }, totalBytes, mine.filterNotNull(), hold)
            }
        }

        /** Drops the preparer's reference: the descriptors close once no send holds them. */
        fun release() = hold.release()
    }

    /** Read-only items ready for one `PlayDropHost.send`, plus what keeps them readable. */
    class PreparedSend internal constructor(
        /** The items, in the order given. */
        val sources: List<SendSource>,
        /** Display names, same order. */
        val names: List<String>,
        /** Sum of the item sizes in bytes. */
        val totalBytes: Long,
        /** Descriptors opened for this send alone. */
        private val own: List<ParcelFileDescriptor>,
        private val hold: SharedHold,
    ) {
        private val released = AtomicBoolean(false)

        /**
         * Keeps what this send reads open until transfer [transferId] completes, fails or is
         * cancelled. Call it with the ID `send` returned.
         */
        fun attachTo(transferId: String) {
            heldByTransfer[transferId] = this
        }

        /** Lets go of this send's share of the items (its send ended or never started). Idempotent. */
        fun release() {
            if (!released.compareAndSet(false, true)) return
            own.forEach { pfd -> runCatching { pfd.close() } }
            hold.release()
        }
    }

    private val heldByTransfer = ConcurrentHashMap<String, PreparedSend>()

    /** Called by the event router when a transfer ends in any way. */
    internal fun transferEnded(transferId: String) {
        heldByTransfer.remove(transferId)?.release()
    }

    /**
     * Opens [uris] for sending and holds one descriptor per item. Blocking (provider queries
     * and opens): call on `Dispatchers.IO` while the grant is valid. Throws [TooManyItems]
     * above [MAX_ITEMS] or when the descriptors would not fit, and [PlayDropException.Io] when
     * an item cannot be read or is not a regular file (its text names the item: keep it out
     * of warning-level logs, SR-32).
     */
    fun prepare(context: Context, uris: List<Uri>): Prepared {
        if (uris.size > MAX_ITEMS || uris.size > FdBudget.available()) throw TooManyItems(uris.size)
        val resolver = context.applicationContext.contentResolver
        val items = mutableListOf<Item>()
        try {
            for (uri in uris) {
                val meta = query(resolver, uri)
                val pfd = openChecked(resolver, uri, meta.name)
                val item = try {
                    Item(
                        uri = uri,
                        name = meta.name,
                        size = Os.fstat(pfd.fileDescriptor).st_size,
                        mtimeMs = meta.mtimeMs,
                        mime = resolver.getType(uri) ?: DownloadsSink.guessMime(meta.name),
                        held = pfd,
                        reopenable = reopenThroughProc(pfd)?.let { it.close(); true } ?: false,
                    )
                } catch (e: Exception) {
                    pfd.close()
                    throw e
                }
                items += item
            }
        } catch (e: Exception) {
            items.forEach { runCatching { it.held.close() } }
            throw e
        }
        val prepared = Prepared(resolver, items)
        HostLog.d("send", "prepared ${items.size} items, ${prepared.notReopenable} without a reopen through /proc")
        return prepared
    }

    /** Prepares [uris] for exactly one send: [prepare] and [Prepared.sends] with one. */
    fun prepareOne(context: Context, uris: List<Uri>): PreparedSend {
        val prepared = prepare(context, uris)
        try {
            return prepared.sends(1).single()
        } finally {
            prepared.release()
        }
    }

    /** Name and modification time of one item. */
    private data class Meta(val name: String, val mtimeMs: Long)

    private fun query(resolver: ContentResolver, uri: Uri): Meta {
        var display: String? = null
        var mtime = 0L
        if (uri.scheme == ContentResolver.SCHEME_CONTENT) {
            try {
                resolver.query(uri, null, null, null, null)?.use { c ->
                    if (c.moveToFirst()) {
                        val n = c.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                        if (n >= 0 && !c.isNull(n)) display = c.getString(n)
                        val m = c.getColumnIndex(DocumentsContract.Document.COLUMN_LAST_MODIFIED)
                        if (m >= 0 && !c.isNull(m)) mtime = c.getLong(m).coerceAtLeast(0)
                    }
                }
            } catch (e: Exception) {
                // Some providers reject a null projection; the name falls back to the URI.
                HostLog.w("send", "metadata query failed; using the URI name", e)
            }
        }
        return Meta(FileNames.sendName(display, uri.lastPathSegment), mtime)
    }

    /** Opens [uri] read-only and refuses anything that is not a seekable regular file. */
    internal fun openChecked(resolver: ContentResolver, uri: Uri, name: String): ParcelFileDescriptor {
        val pfd = try {
            resolver.openFileDescriptor(uri, "r")
        } catch (e: FileNotFoundException) {
            throw PlayDropException.Io("Cannot read $name: ${e.message ?: "not found"}")
        } catch (e: SecurityException) {
            throw PlayDropException.Io("Cannot read $name: permission to read it was withdrawn")
        } ?: throw PlayDropException.Io("Cannot read $name: the app that shared it returned nothing")
        val mode = try {
            Os.fstat(pfd.fileDescriptor).st_mode
        } catch (e: Exception) {
            pfd.close()
            throw PlayDropException.Io("Cannot read $name: ${e.javaClass.simpleName}")
        }
        if (!OsConstants.S_ISREG(mode)) {
            pfd.close()
            throw PlayDropException.Io("Cannot send $name: the sharing app gave a stream, not a file, so it could not be resumed")
        }
        return pfd
    }

    /**
     * A new read-only open of the file behind [held], with its own file offset, or null when
     * this app may not open that file by itself (or it is no longer the same file).
     */
    private fun reopenThroughProc(held: ParcelFileDescriptor): ParcelFileDescriptor? {
        val fresh = try {
            ParcelFileDescriptor.open(File("/proc/self/fd/${held.fd}"), ParcelFileDescriptor.MODE_READ_ONLY)
        } catch (_: Exception) {
            return null
        }
        return try {
            val a = Os.fstat(held.fileDescriptor)
            val b = Os.fstat(fresh.fileDescriptor)
            if (a.st_dev == b.st_dev && a.st_ino == b.st_ino && OsConstants.S_ISREG(b.st_mode)) fresh else null
        } catch (_: Exception) {
            null
        }.also { if (it == null) runCatching { fresh.close() } }
    }

    /** Reopens one item of one send for the core; see the class comment for the fallbacks. */
    private class UriOpener(
        private val resolver: ContentResolver,
        private val item: Item,
        /** This send's own descriptor of the item: the held one for the first send. */
        private val mine: ParcelFileDescriptor,
    ) : SourceOpener {
        override fun open(): Int {
            val fresh = try {
                openChecked(resolver, item.uri, item.name)
            } catch (e: PlayDropException) {
                HostLog.d("send", "reopen of ${item.name} refused (${e.javaClass.simpleName}); using the held descriptor")
                null
            }
            if (fresh != null) return fresh.detachFd()
            if (item.reopenable) {
                val again = try {
                    reopenThroughProc(item.held)
                } catch (_: Exception) {
                    null
                }
                if (again != null) return again.detachFd()
            }
            // Read by this send only; the core opens one item at a time and seeks before
            // reading, so rewinding the shared offset here is enough.
            val dup = try {
                mine.dup()
            } catch (e: Exception) {
                throw PlayDropException.Io("Cannot read ${item.name} any more: ${e.javaClass.simpleName}")
            }
            try {
                Os.lseek(dup.fileDescriptor, 0, OsConstants.SEEK_SET)
            } catch (e: Exception) {
                dup.close()
                throw PlayDropException.Io("Cannot rewind ${item.name}: ${e.javaClass.simpleName}")
            }
            return dup.detachFd()
        }
    }
}

/**
 * A reference count that runs [close] once, when the last reference goes. Starts with one
 * reference (the creator's). Pure, so the counting is unit-tested.
 */
internal class SharedHold(private val close: () -> Unit) {
    private var refs = 1
    private var closed = false

    /** True once [close] ran. */
    val isClosed: Boolean
        @Synchronized get() = closed

    /** Adds [n] references; false (and nothing added) when it is already closed. */
    @Synchronized
    fun retain(n: Int = 1): Boolean {
        require(n >= 0)
        if (closed) return false
        refs += n
        return true
    }

    /** Drops one reference; the last one runs [close]. Extra calls after that do nothing. */
    fun release() {
        val last = synchronized(this) {
            if (closed) return
            refs -= 1
            if (refs == 0) closed = true
            refs == 0
        }
        if (last) close()
    }
}

/** How many descriptors the held items may take, so sockets and files elsewhere still open. */
internal object FdBudget {
    /** Kept free for the core's sockets, the database and everything else. */
    const val RESERVE = 2_048

    /** Android's usual per-process limit, used when `/proc/self/limits` cannot be read. */
    private const val DEFAULT_LIMIT = 32_768

    /** Descriptors the further sends need: one per item that cannot be reopened, per send after the first. */
    fun extraFor(notReopenable: Int, sends: Int): Long = notReopenable.toLong() * (sends - 1).coerceAtLeast(0)

    /** Free descriptors under the soft limit, less [RESERVE]. */
    fun available(): Int {
        val open = File("/proc/self/fd").list()?.size ?: 0
        return free(softLimit(), open)
    }

    /** [limit] less [open] and [RESERVE], never below zero. */
    fun free(limit: Int, open: Int): Int = (limit - open - RESERVE).coerceAtLeast(0)

    /** The "Max open files" soft limit from a `/proc/self/limits` text. */
    fun parseSoftLimit(limits: String): Int? = limits.lineSequence()
        .firstOrNull { it.startsWith("Max open files") }
        ?.removePrefix("Max open files")
        ?.trim()
        ?.split(Regex("\\s+"))
        ?.firstOrNull()
        ?.toIntOrNull()

    private fun softLimit(): Int = try {
        parseSoftLimit(File("/proc/self/limits").readText()) ?: DEFAULT_LIMIT
    } catch (_: Exception) {
        DEFAULT_LIMIT
    }
}
