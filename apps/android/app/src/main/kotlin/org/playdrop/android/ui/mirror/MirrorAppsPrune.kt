package org.playdrop.android.ui.mirror

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings
import org.playdrop.android.service.NotificationMirror

/**
 * Takes uninstalled apps off the notification allow list, on positive grounds only (the rule
 * is [MirrorApps.prunable]).
 *
 * Why an allow-listed package stays visible to us: it was ticked in the picker, which lists
 * what the manifest's `<queries>` launcher intent matches, so PlayDrop can see it for as long
 * as it still matches that intent. It can stop matching while installed when its launcher
 * activity is disabled, the app is disabled or archived (the second query asks with those
 * flags), or an update drops its launcher entry (nothing here can see that: the package is
 * then reported as not found, like an uninstalled one, unless it has a notification on screen
 * or a lookup answers). A work-profile app was never in the list: the query covers this user
 * only. A failed or empty query prunes nothing.
 */
object MirrorAppsPrune {
    /**
     * Prunes the allow list against [launcher], the packages the caller's launcher query found
     * (the picker passes the list it shows). [active] are the packages with a notification on
     * screen, when the listener is bound. Copies the removed apps left on desktops are withdrawn.
     */
    suspend fun run(context: Context, launcher: Set<String>, active: Set<String> = NotificationMirror.activePackages()) {
        if (launcher.isEmpty()) return
        val app = context.applicationContext
        // Right after boot, or with the phone still locked, an app on storage that is not
        // mounted yet is missing from every query: nothing is judged gone before that is over.
        if (!LauncherQuery.startedUp(app)) return
        val before = Settings.read(app).mirrorApps
        val gone = withContext(Dispatchers.IO) {
            MirrorApps.prunable(before, launcher, widerPackages(app), { lookup(app, it) }, active)
        }
        if (gone.isEmpty()) return
        // Only what was judged gone is removed, so an app added meanwhile is never touched.
        Settings.update(app) { it.copy(mirrorApps = it.mirrorApps - gone) }
        gone.forEach(NotificationMirror::withdrawApp)
        HostLog.i("mirror", "took ${gone.size} uninstalled app(s) off the allow list")
    }

    /** Prunes against a fresh launcher query; used when the notification listener starts. */
    suspend fun run(context: Context) {
        val found = withContext(Dispatchers.IO) { launcherPackages(context, 0L) }
        run(context, found)
    }

    /** The launcher query, also matching disabled and archived apps. Empty when it fails. */
    private fun widerPackages(context: Context): Set<String> {
        var flags = PackageManager.MATCH_DISABLED_COMPONENTS.toLong() or PackageManager.MATCH_DISABLED_UNTIL_USED_COMPONENTS.toLong()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM) flags = flags or PackageManager.MATCH_ARCHIVED_PACKAGES
        return launcherPackages(context, flags)
    }

    /** Packages with a launcher entry under [flags]; empty when the query fails. */
    private fun launcherPackages(context: Context, flags: Long): Set<String> = try {
        LauncherQuery.activities(context, flags).mapTo(HashSet()) { it.activityInfo.packageName }
    } catch (e: Exception) {
        HostLog.w("mirror", "could not list apps: ${e.javaClass.simpleName}")
        emptySet()
    }

    private fun lookup(context: Context, packageName: String): MirrorApps.Lookup = try {
        LauncherQuery.applicationInfo(context, packageName)
        MirrorApps.Lookup.FOUND
    } catch (_: PackageManager.NameNotFoundException) {
        MirrorApps.Lookup.NOT_FOUND
    } catch (_: Exception) {
        MirrorApps.Lookup.FAILED
    }
}
