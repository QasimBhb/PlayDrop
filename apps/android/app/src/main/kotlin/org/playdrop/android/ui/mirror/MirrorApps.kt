package org.playdrop.android.ui.mirror

import java.text.Collator
import java.util.Locale

/** One app the picker offers: its package and its name. */
data class MirrorApp(val packageName: String, val label: String)

/** The picker's list logic, free of Android so JUnit can test it. */
object MirrorApps {
    /**
     * One row per package (an app with two launcher entries is listed once, under the first
     * name seen), PlayDrop itself left out (its notifications are never mirrored, SR-41),
     * sorted by name in the user's language and then by package so the order is stable.
     */
    fun forPicker(found: List<MirrorApp>, ownPackage: String, locale: Locale = Locale.getDefault()): List<MirrorApp> {
        val collator = Collator.getInstance(locale).apply { strength = Collator.SECONDARY }
        return found
            .asSequence()
            .filter { it.packageName != ownPackage && it.packageName.isNotBlank() }
            .map { if (it.label.isBlank()) it.copy(label = it.packageName) else it.copy(label = it.label.trim()) }
            .distinctBy { it.packageName }
            .sortedWith { a, b -> collator.compare(a.label, b.label).takeIf { it != 0 } ?: a.packageName.compareTo(b.packageName) }
            .toList()
    }

    /**
     * Packages on [allowed] that the picker's query did not find, sorted. Such an app may be
     * uninstalled, but it may also be archived, disabled or hidden from PlayDrop by package
     * visibility, so it is never taken off the list on its own: the picker shows it as "Not
     * found on this phone" with a Remove button. When the query found nothing at all ([listed]
     * empty, so it most likely failed) none are reported.
     */
    fun notFound(allowed: Set<String>, listed: Set<String>): List<String> {
        if (listed.isEmpty()) return emptyList()
        return allowed.filter { it !in listed }.sorted()
    }

    /** [allowed] with [packageName] added when [on], removed otherwise. */
    fun toggled(allowed: Set<String>, packageName: String, on: Boolean): Set<String> =
        if (on) allowed + packageName else allowed - packageName
}
