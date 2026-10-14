package org.playdrop.android.core

import android.content.ContentResolver
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.system.Os
import android.system.OsConstants
import org.playdrop.core.PlayDropException
import org.playdrop.core.SendSource
import org.playdrop.core.SourceOpener
import java.io.FileNotFoundException
import java.util.concurrent.ConcurrentHashMap

/**
 * Turns `content://` and `file://` URIs (share sheet, photo picker, document picker) into
 * [SendSource.Stream] items the core can send without ever seeing a path (anti-pattern 20).
 *
 * URI read grants from a share intent can end when the sharing activity finishes, while the
 * transfer keeps going in the service and the core reopens the file for every upload and
 * resume. So each item keeps one descriptor open from the moment it is prepared; if a later
 * reopen through the resolver is refused, the opener hands out a duplicate of that descriptor
 * rewound to offset 0. The held descriptors are released when the transfer ends
 * ([PreparedSend.attachTo]).
 */
object UriSource {
    /** Read-only items ready for `PlayDropHost.send`, plus the descriptors that keep them readable. */
    class PreparedSend internal constructor(
        /** The items, in the order given. */
        val sources: List<SendSource>,
        /** Display names, same order; for toasts. */
        val names: List<String>,
        /** Sum of the item sizes in bytes. */
        val totalBytes: Long,
        private val held: List<ParcelFileDescriptor>,
    ) {
        /**
         * Keeps the held descriptors open until transfer [transferId] completes, fails or is
         * cancelled. Call it with the ID `send` returned.
         */
        fun attachTo(transferId: String) {
            heldByTransfer[transferId] = this
        }

        /** Closes the held descriptors now (the send was cancelled or failed to start). */
        fun release() {
            held.forEach { pfd -> runCatching { pfd.close() } }
        }
    }

    private val heldByTransfer = ConcurrentHashMap<String, PreparedSend>()

    /** Called by the event router when a transfer ends in any way. */
    internal fun transferEnded(transferId: String) {
        heldByTransfer.remove(transferId)?.release()
    }

    /**
     * Prepares [uris] for sending. Blocking (provider queries and opens): call on
     * `Dispatchers.IO`. Throws [PlayDropException.Io] when an item cannot be read or is not a
     * regular file (a pipe cannot be resumed), naming the item.
     */
    fun prepare(context: Context, uris: List<Uri>): PreparedSend {
        val resolver = context.applicationContext.contentResolver
        val sources = mutableListOf<SendSource>()
        val names = mutableListOf<String>()
        val held = mutableListOf<ParcelFileDescriptor>()
        var total = 0L
        try {
            for (uri in uris) {
                takePersistable(resolver, uri)
                val meta = query(resolver, uri)
                val pfd = openChecked(resolver, uri, meta.name)
                held += pfd
                val size = Os.fstat(pfd.fileDescriptor).st_size
                total += size
                names += meta.name
                sources += SendSource.Stream(
                    name = meta.name,
                    size = size.toULong(),
                    mtimeMs = meta.mtimeMs,
                    mime = resolver.getType(uri) ?: DownloadsSink.guessMime(meta.name),
                    opener = UriOpener(resolver, uri, meta.name, pfd),
                )
            }
        } catch (e: Exception) {
            held.forEach { runCatching { it.close() } }
            throw e
        }
        return PreparedSend(sources, names, total, held)
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

    private fun takePersistable(resolver: ContentResolver, uri: Uri) {
        if (uri.scheme != ContentResolver.SCHEME_CONTENT) return
        try {
            resolver.takePersistableUriPermission(uri, Intent.FLAG_GRANT_READ_URI_PERMISSION)
        } catch (_: SecurityException) {
            // Not offered for this URI (share sheet and photo picker grants are not persistable).
        }
    }

    /** Reopens one item for the core; see the class comment for the fallback. */
    private class UriOpener(
        private val resolver: ContentResolver,
        private val uri: Uri,
        private val name: String,
        private val held: ParcelFileDescriptor,
    ) : SourceOpener {
        override fun open(): Int {
            val fresh = try {
                openChecked(resolver, uri, name)
            } catch (e: PlayDropException) {
                HostLog.d("send", "reopen refused (${e.javaClass.simpleName}); using the held descriptor")
                null
            }
            if (fresh != null) return fresh.detachFd()
            // A dup shares the file offset with the held descriptor; the core opens one item at
            // a time and seeks before reading, so rewinding here is enough.
            val dup = try {
                held.dup()
            } catch (e: Exception) {
                HostLog.w("send", "duplicating the held descriptor failed", e)
                throw PlayDropException.Io("Cannot read $name any more: ${e.javaClass.simpleName}")
            }
            try {
                Os.lseek(dup.fileDescriptor, 0, OsConstants.SEEK_SET)
            } catch (e: Exception) {
                dup.close()
                throw PlayDropException.Io("Cannot rewind $name: ${e.javaClass.simpleName}")
            }
            return dup.detachFd()
        }
    }
}
