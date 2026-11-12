package org.playdrop.android.service

/**
 * Which notifications are mirrored right now, and to which peers: the Android notification
 * key, the opaque key sent on the wire, and the peers that took the `posted` frame, each with
 * a hash of the content it last got (so an identical repost is not sent again). Pure
 * Kotlin so JUnit can test it; [NotificationMirror] guards it with a lock.
 *
 * Bounded to [capacity] entries, oldest first out. The Android key names the package and a
 * tag, so nothing here is ever logged (SR-43).
 */
internal class MirrorBook(private val capacity: Int = DEFAULT_CAPACITY) {
    /** One mirrored notification; [sent] maps each peer to the content hash it last got. */
    data class Entry(val androidKey: String, val opaqueKey: String, val packageName: String, val sent: Map<String, Long>) {
        /** Peers that show it. */
        val peers: Set<String> get() = sent.keys
    }

    /** Insertion order is age: the first entry is the oldest. */
    private val entries = LinkedHashMap<String, Entry>()

    /** Number of mirrored notifications. */
    val size: Int get() = entries.size

    /**
     * Records that [androidKey] (opaque [opaqueKey], posted by [packageName]) now shows on
     * [peers] with content [contentHash]. A repost moves the entry to the newest end and keeps
     * the peers it already reached. Returns the entries pushed out to stay within [capacity];
     * the caller withdraws those from their peers.
     */
    fun record(androidKey: String, opaqueKey: String, packageName: String, peers: Set<String>, contentHash: Long): List<Entry> {
        val before = entries.remove(androidKey)
        val merged = before?.sent.orEmpty() + peers.associateWith { contentHash }
        if (merged.isEmpty()) return emptyList()
        entries[androidKey] = Entry(androidKey, opaqueKey, packageName, merged)
        val evicted = ArrayList<Entry>()
        while (entries.size > capacity) {
            val oldest = entries.keys.first()
            entries.remove(oldest)?.let(evicted::add)
        }
        return evicted
    }

    /** True when [peer] already shows [androidKey] with exactly this [contentHash]. */
    fun unchanged(androidKey: String, peer: String, contentHash: Long): Boolean =
        entries[androidKey]?.sent?.get(peer) == contentHash

    /** Forgets [androidKey] and returns what was mirrored, or null when it never was. */
    fun remove(androidKey: String): Entry? = entries.remove(androidKey)

    /** The Android key of the notification mirrored to [peer] as [opaqueKey], or null. */
    fun androidKeyFor(peer: String, opaqueKey: String): String? =
        entries.values.firstOrNull { it.opaqueKey == opaqueKey && peer in it.peers }?.androidKey

    /**
     * Forgets [peer] everywhere and returns the opaque keys it was showing, oldest first.
     * Entries left with no peer are dropped.
     */
    fun dropPeer(peer: String): List<String> {
        val keys = ArrayList<String>()
        val it = entries.entries.iterator()
        while (it.hasNext()) {
            val e = it.next()
            val entry = e.value
            if (peer !in entry.peers) continue
            keys.add(entry.opaqueKey)
            val rest = entry.sent - peer
            if (rest.isEmpty()) it.remove() else e.setValue(entry.copy(sent = rest))
        }
        return keys
    }

    /** Forgets every notification of [packageName] (taken off the allow list) and returns them. */
    fun removeApp(packageName: String): List<Entry> {
        val gone = entries.values.filter { it.packageName == packageName }
        for (e in gone) entries.remove(e.androidKey)
        return gone
    }

    /** Peers that show at least one mirrored notification. */
    fun peers(): Set<String> = entries.values.flatMapTo(HashSet()) { it.peers }

    /** Forgets everything and returns what was mirrored. */
    fun clear(): List<Entry> {
        val all = entries.values.toList()
        entries.clear()
        return all
    }

    companion object {
        /** Enough for a busy notification shade; the desktop shows at most 20 per peer. */
        const val DEFAULT_CAPACITY = 200
    }
}
