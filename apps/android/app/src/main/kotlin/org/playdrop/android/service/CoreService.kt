package org.playdrop.android.service

import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.os.Build
import android.os.IBinder
import androidx.core.app.ServiceCompat
import androidx.lifecycle.LifecycleService
import androidx.lifecycle.lifecycleScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.CoreHostImpl
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings
import org.playdrop.core.PlayDropException

/**
 * The always-on host of the Rust core: a `connectedDevice` foreground service (PLAN.md
 * anti-pattern 14: never `dataSync`). It owns the listening socket for the life of the
 * service, not of any activity (anti-pattern 3), so closing the UI never stops receiving.
 *
 * Besides starting the core it feeds the core what only Android knows: network changes
 * (debounced 1 s), battery and presence ([DeviceSignals]), and the MulticastLock policy
 * ([MulticastPolicy]). `START_STICKY` brings it back after the system kills it.
 */
class CoreService : LifecycleService() {
    private lateinit var signals: DeviceSignals
    private var startJob: Job? = null
    private var networkJob: Job? = null
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    private var tetherWatcher: TetherWatcher? = null

    /** Posts the foreground notification, then wires the platform hooks. */
    override fun onCreate() {
        super.onCreate()
        Notifications.ensureChannels(this)
        if (!goForeground()) return
        signals = DeviceSignals(applicationContext, lifecycleScope)
        signals.register()
        registerNetworkCallback()
        lifecycleScope.launch {
            combine(CoreHost.running, CoreHost.peers, Settings.flow(this@CoreService)) { running, peers, settings ->
                val default = settings.defaultPeer?.let { id -> peers.firstOrNull { it.deviceId == id } }
                Triple(running, peers.count { it.online }, default?.label?.ifBlank { default.shortId })
            }.collectLatest { (running, online, label) ->
                Notifications.updateService(this@CoreService, running, online, label)
            }
        }
    }

    /** Starts the core once; repeated starts only re-assert the foreground state. */
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        super.onStartCommand(intent, flags, startId)
        if (!goForeground()) return START_NOT_STICKY
        if (startJob?.isActive != true && !CoreHost.running.value) {
            startJob = lifecycleScope.launch(Dispatchers.IO) { startCore() }
        }
        return START_STICKY
    }

    private suspend fun startCore() {
        try {
            CoreHost.start(applicationContext)
            MulticastPolicy.openNetworkWindow()
            signals.reportNow()
        } catch (e: PlayDropException.PortHeld) {
            HostLog.w("service", "core start failed", e)
            Notifications.startFailed(this, getString(R.string.service_port_held, e.port.toInt()))
            stopSelf()
        } catch (e: Exception) {
            HostLog.w("service", "core start failed", e)
            Notifications.startFailed(this, e.message ?: e.javaClass.simpleName)
            stopSelf()
        }
    }

    /**
     * Calls `startForeground` with the `connectedDevice` type. Returns false (and stops) when
     * the system refuses, for example a start from the background on Android 12+.
     */
    private fun goForeground(): Boolean {
        val n = Notifications.service(this, CoreHost.running.value, CoreHost.peers.value.count { it.online }, null)
        return try {
            val type = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE else 0
            ServiceCompat.startForeground(this, NotificationPlan.SERVICE_ID, n, type)
            true
        } catch (e: Exception) {
            HostLog.w("service", "startForeground refused", e)
            Notifications.startFailed(this, e.message ?: e.javaClass.simpleName)
            stopSelf()
            false
        }
    }

    private fun registerNetworkCallback() {
        val cm = getSystemService(ConnectivityManager::class.java)
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = networkChanged("available")

            override fun onLost(network: Network) = networkChanged("lost")

            override fun onLinkPropertiesChanged(network: Network, linkProperties: LinkProperties) =
                networkChanged("link properties")
        }
        try {
            cm.registerDefaultNetworkCallback(cb)
            networkCallback = cb
        } catch (e: Exception) {
            HostLog.w("service", "network callback registration failed; network changes are not seen", e)
        }
        tetherWatcher = TetherWatcher(this) { why -> networkChanged(why) }.also { it.register() }
    }

    /** Debounces bursts of network callbacks into one `networkChanged` after 1 s of quiet. */
    private fun networkChanged(why: String) {
        networkJob?.cancel()
        networkJob = lifecycleScope.launch(Dispatchers.IO) {
            delay(NETWORK_DEBOUNCE_MS)
            if (!CoreHost.running.value) return@launch
            HostLog.i("service", "network changed ($why)")
            MulticastPolicy.openNetworkWindow()
            try {
                CoreHost.withHost { it.networkChanged() }
            } catch (e: Exception) {
                HostLog.w("service", "networkChanged failed", e)
            }
        }
    }

    /** Unwires everything and stops the core in the background (shutdown can take seconds). */
    override fun onDestroy() {
        networkCallback?.let { cb ->
            runCatching { getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(cb) }
        }
        networkCallback = null
        tetherWatcher?.unregister()
        tetherWatcher = null
        if (::signals.isInitialized) signals.unregister()
        MulticastPolicy.releaseAll()
        FindRinger.stop(this)
        CoreHostImpl.scope.launch { CoreHost.stop() }
        super.onDestroy()
    }

    /** Not bound; the UI talks to the core through [CoreHost]. */
    override fun onBind(intent: Intent): IBinder? {
        super.onBind(intent)
        return null
    }

    private companion object {
        const val NETWORK_DEBOUNCE_MS = 1_000L
    }
}
