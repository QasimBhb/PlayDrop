package org.playdrop.android.data

/** How a send from this phone ended. */
enum class SendOutcome {
    /** Every file arrived. */
    SENT,

    /** It stopped for a reason ([SendHistoryEntry.failure] says which). */
    FAILED,

    /** The person cancelled it on this phone. */
    CANCELLED,
}

/**
 * One line of the "Sent" history: what was sent from this phone, to whom and how it ended.
 * File names are kept (the first few only) because the person asked to see them; they are
 * never logged.
 */
data class SendHistoryEntry(
    /** The transfer ID: one entry per send, updated in place as the send ends. */
    val transferId: String,
    /** When the send ended, Unix milliseconds. */
    val atMs: Long,
    /** The device's name at the time of the send. */
    val peerLabel: String,
    /** Items in the send. */
    val itemCount: Long,
    /** Total size in bytes. */
    val totalBytes: Long,
    /** The first few file names. */
    val names: List<String>,
    val outcome: SendOutcome,
    /** The core's failure code name (`TransferFailure`) for a failed send, else null. */
    val failure: String? = null,
    /** The core's own sentence for a failure it has no code for, already cleaned; else null. */
    val detail: String? = null,
    /**
     * True while the send failed but its record is kept, so it carries on by itself when the
     * device is back or when the same files are sent again. Such an entry reads "Failed" with
     * the reason and a line saying it will carry on; it becomes the final outcome later.
     */
    val resumable: Boolean = false,
)

/**
 * Pure rules of the "Sent" history, kept apart from storage and Compose so they are
 * unit-tested: building an entry, keeping the newest [MAX_ENTRIES], and a plain-text file
 * format that survives damage (a line that does not read is dropped, the rest is kept).
 */
object SendHistory {
    /** The history keeps this many of the latest sends. */
    const val MAX_ENTRIES = 50

    /** How many file names an entry keeps. */
    const val MAX_NAMES = 3

    /** Longest file name kept, in characters. */
    const val MAX_NAME_CHARS = 120

    /** Longest failure sentence kept, in characters. */
    const val MAX_DETAIL_CHARS = 200

    /**
     * An entry for a send that ended at [atMs]. [names] may be whole paths: only the last part
     * of each is kept, at most [MAX_NAMES] of them, and blank ones are skipped.
     */
    fun entry(
        transferId: String,
        atMs: Long,
        peerLabel: String,
        itemCount: Long,
        totalBytes: Long,
        names: List<String>,
        outcome: SendOutcome,
        failure: String? = null,
        detail: String? = null,
        resumable: Boolean = false,
    ): SendHistoryEntry = SendHistoryEntry(
        transferId = transferId,
        atMs = atMs,
        peerLabel = peerLabel,
        itemCount = itemCount.coerceAtLeast(0),
        totalBytes = totalBytes.coerceAtLeast(0),
        names = firstNames(names),
        outcome = outcome,
        failure = failure,
        detail = detail?.take(MAX_DETAIL_CHARS),
        resumable = resumable && outcome == SendOutcome.FAILED,
    )

    /** The names an entry keeps: the last path part, trimmed, cut, at most [MAX_NAMES]. */
    fun firstNames(names: List<String>): List<String> = names.asSequence()
        .map { it.substringAfterLast('/').trim().take(MAX_NAME_CHARS) }
        .filter { it.isNotEmpty() }
        .take(MAX_NAMES)
        .toList()

    /**
     * [history] (newest first) with [entry] in it once, on top: an earlier entry of the same
     * transfer is dropped (a failed send that resumes and completes ends as one entry, and its
     * final outcome is the newest), and nothing is kept past [max] entries.
     */
    fun upsert(history: List<SendHistoryEntry>, entry: SendHistoryEntry, max: Int = MAX_ENTRIES): List<SendHistoryEntry> =
        (listOf(entry) + history.filterNot { it.transferId == entry.transferId }).take(max)

    /**
     * The entry of [transferId] with the details that were not known when it ended (a send
     * whose end was handled before its start was noted): device name, counts and names. Other
     * fields, and every other entry, stay as they are.
     */
    fun enriched(
        history: List<SendHistoryEntry>,
        transferId: String,
        peerLabel: String,
        itemCount: Long,
        totalBytes: Long,
        names: List<String>,
    ): List<SendHistoryEntry> = history.map {
        if (it.transferId == transferId) {
            it.copy(
                peerLabel = peerLabel,
                itemCount = itemCount.coerceAtLeast(0),
                totalBytes = totalBytes.coerceAtLeast(0),
                names = firstNames(names),
            )
        } else {
            it
        }
    }

    /**
     * Stops entries claiming "will carry on" once that is no longer so. The core ends a waiting
     * send's record after 24 hours without any event, so it is the only thing to ask: [listed]
     * holds the transfer IDs the core still lists as resumable, or null when it cannot be
     * asked (the core is not running). An entry stays "resumable" only while it is listed; with
     * no answer it stays only until it is older than [MAX_RESUMABLE_AGE_MS]. A dropped claim
     * leaves the entry as a plain failure.
     */
    fun reconcile(history: List<SendHistoryEntry>, listed: Set<String>?, nowMs: Long): List<SendHistoryEntry> =
        history.map {
            val stale = if (listed != null) it.transferId !in listed else nowMs - it.atMs > MAX_RESUMABLE_AGE_MS
            if (it.resumable && stale) it.copy(resumable = false) else it
        }

    /** The core keeps a waiting send for 24 hours; a little more is allowed for the hourly sweep. */
    const val MAX_RESUMABLE_AGE_MS = 25L * 60 * 60 * 1000

    /**
     * "Forget" on a send that failed and was waiting to resume ends it as cancelled: the
     * person chose to drop it, which is what "cancelled" means, and its failure reason no
     * longer applies. Any other entry, or an unknown [transferId], is left as it is.
     */
    fun forgotten(history: List<SendHistoryEntry>, transferId: String): List<SendHistoryEntry> =
        history.map {
            if (it.transferId == transferId && it.outcome == SendOutcome.FAILED && it.resumable) {
                it.copy(outcome = SendOutcome.CANCELLED, failure = null, detail = null, resumable = false)
            } else {
                it
            }
        }

    // ---- file format: one send per line, tab-separated, newest first ----

    private const val VERSION = "2"
    private const val FIXED_FIELDS = 10

    /** The text to store for [history], at most [MAX_ENTRIES] lines. */
    fun encode(history: List<SendHistoryEntry>): String = history.take(MAX_ENTRIES).joinToString("") { e ->
        val fields = listOf(
            VERSION,
            e.transferId,
            e.atMs.toString(),
            e.outcome.name,
            e.peerLabel,
            e.itemCount.toString(),
            e.totalBytes.toString(),
            e.failure.orEmpty(),
            e.detail.orEmpty(),
            if (e.resumable) "1" else "0",
        ) + e.names
        fields.joinToString("\t") { escape(it) } + "\n"
    }

    /**
     * The history stored in [text], newest first. Blank or damaged lines are skipped, so a
     * half-written or edited file loses only the lines it broke; never throws.
     */
    fun decode(text: String): List<SendHistoryEntry> =
        text.lineSequence().mapNotNull(::decodeLine).take(MAX_ENTRIES).toList()

    private fun decodeLine(line: String): SendHistoryEntry? {
        if (line.isBlank()) return null
        val f = line.split('\t').map(::unescape)
        if (f.size < FIXED_FIELDS || f[0] != VERSION) return null
        val id = f[1].takeIf { it.isNotEmpty() } ?: return null
        val at = f[2].toLongOrNull() ?: return null
        val outcome = SendOutcome.entries.firstOrNull { it.name == f[3] } ?: return null
        val count = f[5].toLongOrNull()?.takeIf { it >= 0 } ?: return null
        val bytes = f[6].toLongOrNull()?.takeIf { it >= 0 } ?: return null
        return SendHistoryEntry(
            transferId = id,
            atMs = at,
            peerLabel = f[4],
            itemCount = count,
            totalBytes = bytes,
            names = f.drop(FIXED_FIELDS).take(MAX_NAMES),
            outcome = outcome,
            failure = f[7].ifEmpty { null },
            detail = f[8].ifEmpty { null },
            resumable = f[9] == "1" && outcome == SendOutcome.FAILED,
        )
    }

    private fun escape(s: String): String = buildString {
        for (c in s) {
            when (c) {
                '\\' -> append("\\\\")
                '\t' -> append("\\t")
                '\n' -> append("\\n")
                '\r' -> append("\\r")
                else -> append(c)
            }
        }
    }

    private fun unescape(s: String): String {
        if ('\\' !in s) return s
        return buildString {
            var i = 0
            while (i < s.length) {
                val c = s[i]
                if (c == '\\' && i + 1 < s.length) {
                    when (s[i + 1]) {
                        't' -> append('\t')
                        'n' -> append('\n')
                        'r' -> append('\r')
                        '\\' -> append('\\')
                        else -> append(s[i + 1])
                    }
                    i += 2
                } else {
                    append(c)
                    i++
                }
            }
        }
    }
}
