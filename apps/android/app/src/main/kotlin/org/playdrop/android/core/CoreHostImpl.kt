package org.playdrop.android.core

import android.content.Context
import android.content.Intent
import android.net.Uri
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.playdrop.android.BuildConfig
import org.playdrop.android.data.Settings
import org.playdrop.android.service.CoreService
import org.playdrop.android.service.MulticastPolicy
import org.playdrop.android.service.Notifications
import org.playdrop.core.AddDeviceCandidate
import org.playdrop.core.DeviceType
import org.playdrop.core.HostConfig
import org.playdrop.core.HostEvent
import org.playdrop.core.HostListener
import org.playdrop.core.KeepAliveProfile
import org.playdrop.core.LogLevel
import org.playdrop.core.MediaState
import org.playdrop.core.PeerSummary
import org.playdrop.core.PlayDropHost
import org.playdrop.core.ReceiveTarget
import org.playdrop.core.TransferSnapshot
import org.playdrop.core.initLogging
import java.io.File

/**
 * Backing implementation of [CoreHost]. Owns the one [PlayDropHost] of the process, turns its
 * event stream into the flows, and routes events to notifications. The UI never imports this
 * file.
 *
 * Threading: the core calls the listener on its own thread; the listener only queues the
 * event. One coroutine on [Dispatchers.IO] handles events in order, so flows and
 * notifications see them in the order the core emitted them. Every host call runs on
 * [Dispatchers.IO].
 */
internal object CoreHostImpl {
    val running = MutableStateFlow(false)
    val peers = MutableStateFlow<List<PeerSummary>>(emptyList())
    val transfers = MutableStateFlow<List<TransferSnapshot>>(emptyList())
    val addDeviceCandidates = MutableStateFlow<List<AddDeviceCandidate>>(emptyList())
    val pairingPrompt = MutableStateFlow<PendingPairing?>(null)
    val peerStatus = MutableStateFlow<Map<String, PeerStatus>>(emptyMap())
    val media = MutableStateFlow<Map<String, List<MediaState>>>(emptyMap())
    val events: SharedFlow<HostEvent> get() = eventsFlow
    val identity = MutableStateFlow<OwnIdentity?>(null)

    /** Process-wide scope for work that must outlive an activity (sends from the share sheet). */
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    private val eventsFlow = MutableSharedFlow<HostEvent>(extraBufferCapacity = 256)

    /** Progress refresh requests, conflated; the collector refreshes at most 4 times per second. */
    private val transferRefresh = MutableSharedFlow<Unit>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)

    /** Serialises start and stop so a restart never races a shutdown that still holds the port. */
    private val lifecycle = Mutex()

    @Volatile
    private var host: PlayDropHost? = null

    @Volatile
    private var appContext: Context? = null

    private var eventQueue: Channel<HostEvent>? = null
    private var processor: Job? = null
    private var refresher: Job? = null
    private var addDeviceWatch: Job? = null

    /** The receive sink, told when each transfer ends so it can flush and log. */
    @Volatile
    private var sink: DownloadsSink? = null

    /** The listener must stay referenced for as long as the host lives. */
    private var listener: HostListener? = null

    private const val PROGRESS_REFRESH_MS = 250L
    private const val ADD_DEVICE_POLL_MS = 2_000L

    /** Builds the config and starts the host. Blocking; called from the service on IO. */
    fun start(context: Context) = runBlocking {
        lifecycle.withLock {
            if (host != null) return@withLock
            val app = context.applicationContext
            appContext = app
            initLogging(if (BuildConfig.DEBUG) LogLevel.DEBUG else LogLevel.INFO)
            val settings = Settings.read(app)
            val staging = (app.getExternalFilesDir("staging") ?: File(app.filesDir, "staging")).apply { mkdirs() }
            val receiveSink = DownloadsSink(app).also { sink = it }
            val config = HostConfig(
                dataDir = File(app.filesDir, "core").absolutePath,
                receive = ReceiveTarget.Staged(stagingDir = staging.absolutePath, sink = receiveSink),
                deviceName = settings.deviceName,
                deviceType = DeviceType.PHONE,
                keepAlive = KeepAliveProfile.MOBILE,
                port = settings.port.toUShort(),
                bindIp = null,
                discovery = true,
                interfaces = AndroidInterfaces(),
                secretStore = KeystoreSecretStore(),
            )
            val channel = Channel<HostEvent>(Channel.UNLIMITED)
            eventQueue = channel
            processor = scope.launch { for (ev in channel) handle(app, ev) }
            refresher = scope.launch {
                transferRefresh.collect {
                    refreshTransfers()
                    delay(PROGRESS_REFRESH_MS)
                }
            }
            val l = object : HostListener {
                override fun onEvent(event: HostEvent) {
                    channel.trySend(event)
                }
            }
            listener = l
            val h = try {
                PlayDropHost.start(config, l)
            } catch (e: Exception) {
                teardownLocked()
                throw e
            }
            host = h
            if (identity.value == null) identity.value = OwnIdentity(h.deviceId(), h.shortId(), h.localAddr())
            peers.value = h.peers()
            transfers.value = h.transfers()
            running.value = true
            HostLog.i("core", "started on ${h.localAddr()}")
        }
    }

    /** Shuts the host down. The object is left to the UniFFI cleaner, so a racing call fails cleanly. */
    suspend fun stop() {
        lifecycle.withLock {
            val h = host ?: return@withLock
            running.value = false
            host = null
            try {
                h.shutdown()
            } catch (e: Exception) {
                HostLog.w("core", "shutdown failed", e)
            }
            teardownLocked()
            HostLog.i("core", "stopped")
        }
    }

    private fun teardownLocked() {
        eventQueue?.close()
        eventQueue = null
        refresher?.cancel()
        refresher = null
        addDeviceWatch?.cancel()
        addDeviceWatch = null
        listener = null
        identity.value = null
        addDeviceCandidates.value = emptyList()
        pairingPrompt.value = null
        media.value = emptyMap()
        MulticastPolicy.set(DiscoveryReason.ADD_DEVICE, false)
    }

    /** Runs [block] on IO against the live host. */
    suspend fun <T> withHost(block: suspend (PlayDropHost) -> T): T {
        val h = host ?: throw IllegalStateException("core not running")
        return withContext(Dispatchers.IO) { block(h) }
    }

    /** Starts [CoreService] as a foreground service. */
    fun ensureServiceRunning(context: Context) {
        if (running.value) return
        ContextCompat.startForegroundService(context, Intent(context, CoreService::class.java))
    }

    /** See [CoreHost.awaitRunning]. */
    suspend fun awaitRunning(context: Context, timeoutMs: Long): Boolean {
        if (running.value) return true
        ensureServiceRunning(context)
        return withTimeoutOrNull(timeoutMs) { running.first { it } } ?: false
    }

    /** See [CoreHost.refreshPeers]. */
    suspend fun refreshPeers() {
        val h = host ?: return
        peers.value = withContext(Dispatchers.IO) { h.peers() }
    }

    private suspend fun refreshTransfers() {
        val h = host ?: return
        try {
            transfers.value = withContext(Dispatchers.IO) { h.transfers() }
        } catch (e: Exception) {
            HostLog.w("core", "transfer list refresh failed", e)
        }
    }

    /** See [CoreHost.openAddDeviceWindow]. */
    suspend fun openAddDeviceWindow() {
        withHost { it.openAddDeviceWindow() }
        watchAddDeviceWindow()
    }

    /** See [CoreHost.showQr]. */
    suspend fun showQr(): String {
        val uri = withHost { it.showQr() }
        watchAddDeviceWindow()
        return uri
    }

    /** See [CoreHost.closeAddDeviceWindow]. */
    suspend fun closeAddDeviceWindow() {
        try {
            withHost { it.closeAddDeviceWindow() }
        } finally {
            addDeviceWindowClosed()
        }
    }

    /**
     * Holds the multicast reason while the core's window is open and notices when the core
     * closes it on its own (120 s timeout, success, three failures): no event marks that.
     */
    private fun watchAddDeviceWindow() {
        MulticastPolicy.set(DiscoveryReason.ADD_DEVICE, true)
        addDeviceWatch?.cancel()
        addDeviceWatch = scope.launch {
            while (isActive) {
                delay(ADD_DEVICE_POLL_MS)
                val open = try {
                    withHost { it.addDeviceWindowOpen() }
                } catch (e: IllegalStateException) {
                    false
                }
                if (!open) {
                    addDeviceWindowClosed()
                    break
                }
            }
        }
    }

    private fun addDeviceWindowClosed() {
        addDeviceWatch?.cancel()
        addDeviceWatch = null
        addDeviceCandidates.value = emptyList()
        MulticastPolicy.set(DiscoveryReason.ADD_DEVICE, false)
    }

    /** See [CoreHost.answerPairing]. */
    suspend fun answerPairing(accept: Boolean) {
        try {
            withHost { it.pairingRespond(accept) }
        } finally {
            pairingPrompt.value = null
        }
    }

    /** See [CoreHost.setDiscoveryWindow]. */
    fun setDiscoveryWindow(reason: DiscoveryReason, on: Boolean) = MulticastPolicy.set(reason, on)

    /** See [CoreHost.sendUris]. */
    suspend fun sendUris(context: Context, peer: String, uris: List<Uri>): String {
        val prepared = withContext(Dispatchers.IO) { UriSource.prepare(context, uris) }
        val id = try {
            withHost { it.send(peer, prepared.sources, null) }
        } catch (e: Exception) {
            prepared.release()
            throw e
        }
        prepared.attachTo(id)
        HostLog.i("send", "transfer ${id.take(8)}: ${uris.size} items, ${prepared.totalBytes} bytes")
        return id
    }

    /** See [CoreHost.labelOf]. */
    fun labelOf(peer: String): String {
        val p = peers.value.firstOrNull { it.deviceId == peer }
        return p?.label?.takeIf { it.isNotBlank() } ?: p?.shortId ?: peer.take(8)
    }

    /** Handles one event, in order. Never throws: one bad event must not stop the stream. */
    private suspend fun handle(app: Context, ev: HostEvent) {
        try {
            route(ev)
        } catch (e: Exception) {
            HostLog.w("events", "updating state for ${ev.javaClass.simpleName} failed", e)
        }
        eventsFlow.emit(ev)
        try {
            Notifications.onEvent(app, ev, ::labelOf)
        } catch (e: Exception) {
            HostLog.w("events", "notification for ${ev.javaClass.simpleName} failed", e)
        }
    }

    private suspend fun route(ev: HostEvent) {
        when (ev) {
            is HostEvent.Listening -> identity.value = OwnIdentity(ev.deviceId, ev.shortId, ev.addr)
            is HostEvent.PeerOnline -> refreshPeers()
            is HostEvent.PeerOffline -> {
                media.update { MediaPlayers.forget(it, ev.peer) }
                refreshPeers()
            }
            is HostEvent.Unpaired -> {
                media.update { MediaPlayers.forget(it, ev.peer) }
                refreshPeers()
            }
            is HostEvent.RemovedByPeer -> refreshPeers()
            is HostEvent.MediaState -> media.update { MediaPlayers.apply(it, ev.peer, ev.state) }
            is HostEvent.Paired -> {
                pairingPrompt.value = null
                refreshPeers()
            }
            is HostEvent.PairingFailed -> {
                pairingPrompt.value = null
                refreshPeers()
            }
            is HostEvent.PairingPrompt ->
                pairingPrompt.value = PendingPairing(ev.peer, ev.shortId, ev.name, ev.prompt)
            is HostEvent.AddDeviceSeen -> {
                val h = host ?: return
                addDeviceCandidates.value = withContext(Dispatchers.IO) { h.addDeviceList() }
            }
            is HostEvent.TransferProgress -> transferRefresh.tryEmit(Unit)
            is HostEvent.TransferOffer -> refreshTransfers()
            is HostEvent.TransferComplete -> {
                UriSource.transferEnded(ev.id)
                sink?.transferEnded(ev.id)
                refreshTransfers()
            }
            is HostEvent.TransferFailed -> {
                UriSource.transferEnded(ev.id)
                sink?.transferEnded(ev.id)
                refreshTransfers()
            }
            is HostEvent.Battery -> peerStatus.update { all ->
                val prev = all[ev.peer] ?: PeerStatus()
                all + (ev.peer to prev.copy(batteryLevel = ev.level.toInt(), charging = ev.charging, batteryLow = ev.low))
            }
            is HostEvent.Presence -> peerStatus.update { all ->
                val prev = all[ev.peer] ?: PeerStatus()
                all + (ev.peer to prev.copy(presence = ev.state))
            }
            is HostEvent.DiscoveryDiagnostic ->
                HostLog.i("discovery", "peer ${ev.shortId} not reached via ${ev.path}: ${ev.failure}")
            else -> Unit
        }
    }
}
