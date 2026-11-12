package org.playdrop.android.share

import android.content.ContentResolver
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import org.playdrop.android.core.UriSource

/**
 * Which shared items PlayDrop accepts (T34, SR-29). Another app must not be able to make
 * PlayDrop offer PlayDrop's own private files: `file://` items are refused (PlayDrop can read
 * its own folders, the sharing app may not), and so is any `content://` item served by one of
 * PlayDrop's own providers. Only items another app hands over through its own provider pass.
 */
internal object ShareUris {
    /** Why a share is refused before anything is opened. */
    enum class Refusal {
        /** An item is not another app's `content://` item. */
        NOT_ALLOWED,

        /** More than [UriSource.MAX_ITEMS] items. */
        TOO_MANY,
    }

    /** True for a `content://` item from an authority that is not one of [own]. Pure. */
    fun allowed(scheme: String?, authority: String?, own: Set<String>): Boolean =
        scheme == ContentResolver.SCHEME_CONTENT && !authority.isNullOrEmpty() && authority !in own

    /** Why [uris] cannot be sent as they are, or null when they can. Pure apart from [Uri]. */
    fun refusal(uris: List<Uri>, own: Set<String>): Refusal? = when {
        uris.size > UriSource.MAX_ITEMS -> Refusal.TOO_MANY
        uris.any { !allowed(it.scheme, it.authority, own) } -> Refusal.NOT_ALLOWED
        else -> null
    }

    /** The authorities of every provider in this app (one provider may list several). */
    fun ownAuthorities(context: Context): Set<String> {
        val pm = context.packageManager
        val info = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                pm.getPackageInfo(context.packageName, PackageManager.PackageInfoFlags.of(PackageManager.GET_PROVIDERS.toLong()))
            } else {
                @Suppress("DEPRECATION")
                pm.getPackageInfo(context.packageName, PackageManager.GET_PROVIDERS)
            }
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }
        val listed = info?.providers.orEmpty().flatMap { it.authority?.split(';').orEmpty() }.map { it.trim() }.filter { it.isNotEmpty() }
        // The FileProvider's authority even if the query came back empty.
        return (listed + "${context.packageName}.files").toSet()
    }
}
