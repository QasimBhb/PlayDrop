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
 * Starts the service after boot, once the user finished onboarding. `connectedDevice` may
 * start from `BOOT_COMPLETED` (unlike `dataSync` on Android 15). The action is checked
 * because only the system can send `BOOT_COMPLETED`; anything else is ignored.
 */
class BootReceiver : BroadcastReceiver() {
    /** Reads the onboarding flag off the main thread and starts the service. */
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_BOOT_COMPLETED) return
        val app = context.applicationContext
        val pending = goAsync()
        scope.launch {
            try {
                if (Settings.read(app).onboarded) {
                    CoreHost.ensureServiceRunning(app)
                    HostLog.i("boot", "service started after boot")
                } else {
                    HostLog.i("boot", "onboarding not finished; not starting after boot")
                }
            } catch (e: Exception) {
                HostLog.w("boot", "starting after boot failed", e)
            } finally {
                pending.finish()
            }
        }
    }

    private companion object {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}
