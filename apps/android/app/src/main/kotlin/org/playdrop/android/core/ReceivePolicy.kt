package org.playdrop.android.core

/**
 * Pure rules of the receive sink that do not need Android: when a delivered file is synced
 * and when written files are handed to the media scanner. Unit-tested on the JVM.
 */
internal object ReceivePolicy {
    /**
     * Files at least this big are `fdatasync`ed before [DownloadsSink.deliver] returns.
     *
     * Why not every file: on the Galaxy A54 the sync of a small file through the shared
     * storage FUSE layer cost 1.5 to 2.3 ms, more than the whole rest of a direct write, and
     * Java offers no `syncfs` to flush a batch at once (one `fsync` covers one file only, so
     * "sync every N MiB" would still leave the files before it unsynced). The core already
     * synced the verified staging copy, and Android's writeback flushes dirty pages within
     * seconds (`vm.dirty_expire_centisecs`), so a small file is at risk only if the phone
     * loses power in that window. For big files the sync is under 1% of the copy (38 ms for
     * 1 GiB measured) and their loss would cost the most to redo, so they keep it.
     */
    const val SYNC_MIN_BYTES: Long = 8L * 1024 * 1024

    /** Most paths handed to the media scanner in one call, so a huge transfer is indexed in steps. */
    const val SCAN_BATCH = 500

    /** True when a delivered file of [size] bytes is synced before the sink returns. */
    fun syncAfterCopy(size: Long): Boolean = size >= SYNC_MIN_BYTES
}

/**
 * Collects written paths per transfer and hands them out in batches of at most [limit]: a
 * full batch as soon as it fills, the rest when the transfer ends. Thread-safe.
 */
internal class ScanBatcher(private val limit: Int = ReceivePolicy.SCAN_BATCH) {
    init {
        require(limit > 0) { "limit must be positive" }
    }

    private val pending = HashMap<String, MutableList<String>>()

    /** Adds [path] for [transfer]; returns a full batch to scan now, or null. */
    @Synchronized
    fun add(transfer: String, path: String): List<String>? {
        val list = pending.getOrPut(transfer) { ArrayList() }
        list.add(path)
        if (list.size < limit) return null
        pending.remove(transfer)
        return list
    }

    /** Removes and returns what is left for [transfer] (empty when nothing is). */
    @Synchronized
    fun drain(transfer: String): List<String> = pending.remove(transfer) ?: emptyList()
}
