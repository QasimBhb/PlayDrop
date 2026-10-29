package org.playdrop.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings

/**
 * When the service starts without the user opening the app: after boot ([BootReceiver]) and
 * after the app was updated or reinstalled ([UpdateReceiver]; until then an `adb install -r` or
 * a store update left the phone unreachable until someone opened the app). Both need finished
 * onboarding; there is no setting that turns the service off. Pure JVM.
 */
internal object AutoStart {
    /** `Intent.ACTION_BOOT_COMPLETED`. */
    const val BOOT = "android.intent.action.BOOT_COMPLETED"

    /** `Intent.ACTION_MY_PACKAGE_REPLACED`: sent by the system to this app alone after it was replaced. */
    const val UPDATED = "android.intent.action.MY_PACKAGE_REPLACED"

    /** True when a broadcast with [action], received by a receiver that expects [expected], starts the service. */
    fun shouldStart(action: String?, expected: String, onboarded: Boolean): Boolean = action == expected && onboarded
}

/**
 * Starts the service for [action] once the user finished onboarding ([AutoStart]). Anything
 * but [action] is ignored. `connectedDevice` may be started from the background after
 * `BOOT_COMPLETED` and `MY_PACKAGE_REPLACED` (both are on Android's list of exemptions from
 * the background start limits, unlike `dataSync` after boot on Android 15).
 */
abstract class AutoStartReceiver(private val action: String, private val why: String) : BroadcastReceiver() {
    /** Reads the onboarding flag off the main thread and starts the service. */
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != action) return
        val app = context.applicationContext
        val pending = goAsync()
        scope.launch {
            try {
                if (AutoStart.shouldStart(intent.action, action, Settings.read(app).onboarded)) {
                    CoreHost.ensureServiceRunning(app)
                    HostLog.i("boot", "service started $why")
                } else {
                    HostLog.i("boot", "onboarding not finished; not starting $why")
                }
            } catch (e: Exception) {
                HostLog.w("boot", "starting $why failed", e)
            } finally {
                pending.finish()
            }
        }
    }

    private companion object {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}

/**
 * Starts the service after the app was updated or reinstalled. Not exported: the system
 * sends `MY_PACKAGE_REPLACED` to this app alone and delivers it to unexported receivers.
 */
class UpdateReceiver : AutoStartReceiver(AutoStart.UPDATED, "after an update")
