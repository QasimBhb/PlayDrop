package org.playdrop.android.core

/**
 * Per-transfer timing of the receive sink, summarised once when the transfer ends so a
 * 5,000-file transfer writes one log line, not 5,000. Pure JVM so it is unit-tested; the
 * sink feeds it `System.nanoTime()` deltas. Thread-safe: the core may deliver from more
 * than one blocking thread.
 */
internal class SinkTimings {
    private class Acc {
        var files = 0
        var bytes = 0L
        var first = Long.MAX_VALUE
        var last = Long.MIN_VALUE
        val steps = LinkedHashMap<String, Long>()
    }

    private val open = HashMap<String, Acc>()

    /** Adds [nanos] spent in [step] for [transfer]. */
    @Synchronized
    fun add(transfer: String, step: String, nanos: Long) {
        val acc = open.getOrPut(transfer) { Acc() }
        acc.steps[step] = (acc.steps[step] ?: 0L) + nanos
    }

    /**
     * Counts one delivered file of [bytes] for [transfer] whose delivery ran from [start] to
     * [end] (nanoTime). The span from the first start to the last end, against the summed
     * steps, shows how much time the core spent between deliveries.
     */
    @Synchronized
    fun fileDone(transfer: String, bytes: Long, start: Long, end: Long) {
        val acc = open.getOrPut(transfer) { Acc() }
        acc.files++
        acc.bytes += bytes
        acc.first = minOf(acc.first, start)
        acc.last = maxOf(acc.last, end)
    }

    /** Removes and returns the summary of [transfer], or null when nothing was delivered. */
    @Synchronized
    fun finish(transfer: String): Summary? {
        val acc = open.remove(transfer) ?: return null
        val span = if (acc.files == 0) 0L else acc.last - acc.first
        return Summary(acc.files, acc.bytes, span, acc.steps.toMap())
    }

    /** What one transfer cost in the sink. */
    data class Summary(val files: Int, val bytes: Long, val spanNanos: Long, val stepNanos: Map<String, Long>) {
        /** Total sink time over every step. */
        val totalNanos: Long get() = stepNanos.values.sum()

        /** One line: total, per-file mean, then each step as total ms and per-file ms. */
        fun line(): String {
            val n = files.coerceAtLeast(1)
            val steps = stepNanos.entries.joinToString(", ") { (k, v) ->
                "$k ${ms(v)} ms (${perFile(v, n)} ms/file)"
            }
            return "$files files, $bytes bytes, first to last delivery ${ms(spanNanos)} ms, " +
                "sink ${ms(totalNanos)} ms (${perFile(totalNanos, n)} ms/file): $steps"
        }

        private fun ms(nanos: Long): String = "%.1f".format(java.util.Locale.ROOT, nanos / 1e6)

        private fun perFile(nanos: Long, n: Int): String = "%.3f".format(java.util.Locale.ROOT, nanos / 1e6 / n)
    }
}
