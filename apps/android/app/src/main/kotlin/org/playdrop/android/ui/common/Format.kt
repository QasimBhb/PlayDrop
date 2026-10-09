package org.playdrop.android.ui.common

/**
 * Number formatting shared by every screen. Units follow the desktop window (binary units
 * with one decimal) so both sides of a transfer show the same figures. Everything here is
 * plain Kotlin so it runs in JVM unit tests.
 */
object Format {
    private val UNITS = arrayOf("KiB", "MiB", "GiB", "TiB", "PiB")

    /** Bytes in binary units with one decimal: "512 B", "1.5 KiB", "12.0 MiB". */
    fun bytes(bytes: Long): String {
        if (bytes < 1024) return "${bytes.coerceAtLeast(0)} B"
        var value = bytes / 1024.0
        var unit = 0
        while (value >= 1024.0 && unit + 1 < UNITS.size) {
            value /= 1024.0
            unit += 1
        }
        return String.format(java.util.Locale.ROOT, "%.1f %s", value, UNITS[unit])
    }

    /** A rate: "30.0 MiB/s". */
    fun rate(bytesPerSec: Long): String = "${bytes(bytesPerSec)}/s"

    /** Time left at the current rate in seconds, or null when it cannot be estimated. */
    fun etaSeconds(remaining: Long, bytesPerSec: Long): Long? {
        if (bytesPerSec <= 0) return null
        val rem = remaining.coerceAtLeast(0)
        return (rem + bytesPerSec - 1) / bytesPerSec
    }

    /** "<1 s", "45 s", "2 min 5 s", "1 h 3 min". */
    fun duration(secs: Long): String = when {
        secs <= 0 -> "<1 s"
        secs < 60 -> "$secs s"
        secs < 3600 -> {
            val m = secs / 60
            val s = secs % 60
            if (s == 0L) "$m min" else "$m min $s s"
        }
        else -> "${secs / 3600} h ${(secs % 3600) / 60} min"
    }

    /** "m:ss" for a countdown. */
    fun countdown(secs: Long): String {
        val s = secs.coerceAtLeast(0)
        return String.format(java.util.Locale.ROOT, "%d:%02d", s / 60, s % 60)
    }

    /** Fraction done in 0..1; 0 when the total is unknown. */
    fun fraction(bytes: Long, total: Long): Float =
        if (total <= 0) 0f else (bytes.toDouble() / total.toDouble()).coerceIn(0.0, 1.0).toFloat()

    /** Groups a six digit SAS as "123 456" so it is easy to compare across two screens. */
    fun sas(sas: String): String =
        if (sas.length == 6) "${sas.substring(0, 3)} ${sas.substring(3)}" else sas

    /** Current Unix time in seconds. */
    fun nowSecs(): Long = System.currentTimeMillis() / 1000
}

/** How long ago something happened, resolved to text by the screen (plurals live in resources). */
sealed class Ago {
    /** Less than a minute. */
    data object JustNow : Ago()

    /** Whole minutes. */
    data class Minutes(val n: Int) : Ago()

    /** Whole hours. */
    data class Hours(val n: Int) : Ago()

    /** Whole days. */
    data class Days(val n: Int) : Ago()

    /** No timestamp at all. */
    data object Never : Ago()

    companion object {
        /** The bucket for [then] seen from [now], both Unix seconds; null [then] is [Never]. */
        fun of(then: Long?, now: Long): Ago {
            if (then == null) return Never
            val d = (now - then).coerceAtLeast(0)
            return when {
                d < 60 -> JustNow
                d < 3600 -> Minutes((d / 60).toInt())
                d < 86_400 -> Hours((d / 3600).toInt())
                else -> Days((d / 86_400).toInt())
            }
        }
    }
}
