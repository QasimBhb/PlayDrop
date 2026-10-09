package org.playdrop.android

import android.app.Activity
import android.app.Application
import android.os.Bundle
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.DiscoveryReason
import org.playdrop.android.service.MulticastPolicy
import org.playdrop.android.service.Notifications

/**
 * Process-wide setup: notification channels, the MulticastLock policy, and the
 * "MainActivity is in the foreground" discovery reason (anti-pattern 10), tracked here so the
 * UI does not have to.
 */
class PlayDropApp : Application() {
    /** Creates channels and installs the activity tracker. */
    override fun onCreate() {
        super.onCreate()
        Notifications.ensureChannels(this)
        MulticastPolicy.init(this)
        registerActivityLifecycleCallbacks(ForegroundTracker())
    }

    /** Turns [DiscoveryReason.FOREGROUND] on while `MainActivity` is started. */
    private class ForegroundTracker : ActivityLifecycleCallbacks {
        private var started = 0

        override fun onActivityStarted(activity: Activity) {
            if (activity !is MainActivity) return
            if (started++ == 0) CoreHost.setDiscoveryWindow(DiscoveryReason.FOREGROUND, true)
        }

        override fun onActivityStopped(activity: Activity) {
            if (activity !is MainActivity) return
            if (--started == 0) CoreHost.setDiscoveryWindow(DiscoveryReason.FOREGROUND, false)
        }

        override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) = Unit
        override fun onActivityResumed(activity: Activity) = Unit
        override fun onActivityPaused(activity: Activity) = Unit
        override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) = Unit
        override fun onActivityDestroyed(activity: Activity) = Unit
    }
}
