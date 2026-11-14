package org.playdrop.android.ui.mirror

import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ResolveInfo
import android.os.Build
import android.os.UserManager
import android.os.Environment

/**
 * The launcher-app query and the package lookups, written once with the API level checked:
 * `ResolveInfoFlags` and `ApplicationInfoFlags` exist from Android 13, and the app supports
 * Android 8 and up, where touching them throws `NoClassDefFoundError` (an `Error`, not
 * caught by `catch (e: Exception)`).
 */
internal object LauncherQuery {
    /** Apps with a launcher entry, matched under [flags] (`PackageManager.MATCH_*`). */
    fun activities(context: Context, flags: Long = 0L): List<ResolveInfo> {
        val pm = context.packageManager
        val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            pm.queryIntentActivities(intent, PackageManager.ResolveInfoFlags.of(flags))
        } else {
            // The archived flag does not exist before Android 15; the low 32 bits are the old flags.
            @Suppress("DEPRECATION")
            pm.queryIntentActivities(intent, flags.toInt())
        }
    }

    /** Throws [PackageManager.NameNotFoundException] when the system has no such package visible to us. */
    @Throws(PackageManager.NameNotFoundException::class)
    fun applicationInfo(context: Context, packageName: String) {
        val pm = context.packageManager
        when {
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM ->
                pm.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(PackageManager.MATCH_ARCHIVED_PACKAGES))
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU ->
                pm.getApplicationInfo(packageName, PackageManager.ApplicationInfoFlags.of(0))
            else -> @Suppress("DEPRECATION") pm.getApplicationInfo(packageName, 0)
        }
    }

    /**
     * True once the phone has finished starting up for this user: the user's storage is
     * unlocked and the shared storage is mounted. Right after boot an app on storage that is
     * not mounted yet is missing from every query, so nothing may be judged gone before this.
     */
    fun startedUp(context: Context): Boolean {
        val unlocked = try {
            context.getSystemService(UserManager::class.java)?.isUserUnlocked == true
        } catch (e: Exception) {
            false
        }
        return unlocked && Environment.getExternalStorageState() == Environment.MEDIA_MOUNTED
    }
}
