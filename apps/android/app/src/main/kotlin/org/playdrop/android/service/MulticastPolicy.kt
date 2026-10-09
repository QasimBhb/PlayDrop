package org.playdrop.android.service

import android.content.Context
import android.net.wifi.WifiManager
import android.os.Handler
import android.os.Looper
import org.playdrop.android.core.DiscoveryReason
import org.playdrop.android.core.HostLog

/**
 * Holds the Wi-Fi MulticastLock only while something needs to hear multicast discovery, and
 * never permanently (PLAN.md anti-pattern 10: the lock costs battery on every packet):
 *
 * - [DiscoveryReason.ADD_DEVICE] while the "Add device" window is open;
 * - [DiscoveryReason.FOREGROUND] while `MainActivity` is on screen;
 * - [DiscoveryReason.NETWORK] for 60 s after the core starts or the network changes.
 *
 * Paired peers do not need the lock: they reconnect by last-known address and mDNS unicast
 * replies. Thread-safe.
 */
internal object MulticastPolicy {
    /** How long a start or network change keeps the lock. */
    const val NETWORK_WINDOW_MS = 60_000L

    private val main = Handler(Looper.getMainLooper())
    private val reasons = mutableSetOf<DiscoveryReason>()
    private var lock: WifiManager.MulticastLock? = null
    private var appContext: Context? = null
    private val endNetworkWindow = Runnable { set(DiscoveryReason.NETWORK, false) }

    /** Remembers the context used to create the lock. Called once from the application. */
    fun init(context: Context) {
        synchronized(this) { appContext = context.applicationContext }
    }

    /** Turns one reason on or off and acquires or releases the lock to match. */
    fun set(reason: DiscoveryReason, on: Boolean) {
        synchronized(this) {
            val changed = if (on) reasons.add(reason) else reasons.remove(reason)
            if (changed) HostLog.d("multicast", "reason $reason ${if (on) "on" else "off"}; active=$reasons")
            apply()
        }
    }

    /** Opens the 60 s window after a start or network change (restarts it if open). */
    fun openNetworkWindow() {
        main.removeCallbacks(endNetworkWindow)
        set(DiscoveryReason.NETWORK, true)
        main.postDelayed(endNetworkWindow, NETWORK_WINDOW_MS)
    }

    /** Drops every reason and the lock (service stopping). */
    fun releaseAll() {
        main.removeCallbacks(endNetworkWindow)
        synchronized(this) {
            reasons.clear()
            apply()
        }
    }

    /** True while the lock is held; for diagnostics. */
    fun held(): Boolean = synchronized(this) { lock?.isHeld == true }

    private fun apply() {
        val want = reasons.isNotEmpty()
        val ctx = appContext ?: return
        val l = lock ?: ctx.getSystemService(WifiManager::class.java)
            ?.createMulticastLock("playdrop-discovery")
            ?.apply { setReferenceCounted(false) }
            ?.also { lock = it }
        if (l == null) {
            HostLog.w("multicast", "no Wi-Fi service; multicast discovery cannot be enabled")
            return
        }
        try {
            if (want && !l.isHeld) {
                l.acquire()
                HostLog.i("multicast", "lock acquired for $reasons")
            } else if (!want && l.isHeld) {
                l.release()
                HostLog.i("multicast", "lock released")
            }
        } catch (e: Exception) {
            HostLog.w("multicast", "multicast lock change failed", e)
        }
    }
}
