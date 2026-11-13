package org.playdrop.android.ui.device

import android.app.Application
import android.net.Uri
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.MediaPlayers
import org.playdrop.android.data.Settings
import org.playdrop.android.service.NotificationMirror
import org.playdrop.android.share.ClipImage
import org.playdrop.android.ui.common.AppScope
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.common.nowTicker
import org.playdrop.android.ui.devices.DeviceRow
import org.playdrop.android.ui.devices.DeviceRows
import org.playdrop.android.ui.input.InputSupport
import org.playdrop.core.Grants
import org.playdrop.core.HostEvent
import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState

/** One of the per-device permissions (SR-14). */
enum class GrantKind {
    /** Receive files without asking (auto-accept). */
    FILES,

    /** Let the device put text on this phone's clipboard. */
    CLIPBOARD_IN,

    /** Send this phone's clipboard to the device. */
    CLIPBOARD_OUT,

    /** Media control. */
    MEDIA,

    /** Let the device ring this phone. */
    FIND,

    /**
     * Let the device control this phone with remote input. A phone never accepts input
     * (it does not advertise `input.*`), so the screen does not offer this toggle.
     */
    INPUT,

    /** Mirror this phone's notifications to the device (SR-41; off until the user turns it on). */
    NOTIFICATIONS_OUT,
}

/** Reads and changes one grant on a plain [Grants] value. */
object GrantEdit {
    /** The current value of [kind]. */
    fun get(g: Grants, kind: GrantKind): Boolean = when (kind) {
        GrantKind.FILES -> g.files
        GrantKind.CLIPBOARD_IN -> g.clipboardIn
        GrantKind.CLIPBOARD_OUT -> g.clipboardOut
        GrantKind.MEDIA -> g.media
        GrantKind.FIND -> g.find
        GrantKind.INPUT -> g.input
        GrantKind.NOTIFICATIONS_OUT -> g.notificationsOut
    }

    /** A copy of [g] with [kind] set to [value]. */
    fun with(g: Grants, kind: GrantKind, value: Boolean): Grants = when (kind) {
        GrantKind.FILES -> g.copy(files = value)
        GrantKind.CLIPBOARD_IN -> g.copy(clipboardIn = value)
        GrantKind.CLIPBOARD_OUT -> g.copy(clipboardOut = value)
        GrantKind.MEDIA -> g.copy(media = value)
        GrantKind.FIND -> g.copy(find = value)
        GrantKind.INPUT -> g.copy(input = value)
        GrantKind.NOTIFICATIONS_OUT -> g.copy(notificationsOut = value)
    }
}

/** Everything the Device detail screen shows. */
data class DeviceDetailUiState(
    /** The card data; null once the device is no longer paired. */
    val row: DeviceRow? = null,
    /** Current grants, including a change still on its way to the core. */
    val grants: Grants = Grants(files = true, clipboardIn = true, clipboardOut = true, input = false, media = true, find = true),
    /** Last-known addresses, `ip:port`. */
    val lastKnown: List<String> = emptyList(),
    /** Pairing time, Unix seconds. */
    val pairedAt: Long = 0,
    /** A find request is ringing the device. */
    val ringing: Boolean = false,
    /** What the device accepts while online (`input.pointer`, `media.control`, ...); empty offline. */
    val capabilities: List<String> = emptyList(),
    /** Media players the device reports, for the "Now playing" cards; never the computer volume entry. */
    val media: List<MediaState> = emptyList(),
    /** The computer's master volume, when the device offers it ([MediaPlayers.SYSTEM_VOLUME_ID]). */
    val systemVolume: MediaState? = null,
    /** How many apps are on the notification mirroring allow list (shared by every device). */
    val mirrorApps: Int = 0,
    /** Confirmation of the last file send started from this page. */
    val notice: SendNotice? = null,
    /** A clipboard send is being delivered. */
    val sendingClipboard: Boolean = false,
    /** A clip marked sensitive waits for the user's yes (SR-24). */
    val askSensitive: Boolean = false,
) {
    /** Show the "Remote input" button: online and able to take pointer or key input. */
    val canControl: Boolean get() = row?.online == true && InputSupport.canControl(capabilities)
}

/** State and actions of the Device detail screen; the device ID comes from the route. */
class DeviceDetailViewModel(application: Application, savedState: SavedStateHandle) : CoreViewModel(application) {
    private val peerId: String = checkNotNull(savedState.get<String>(ARG_ID))
    private val pendingGrants = MutableStateFlow<Grants?>(null)
    private val ringing = MutableStateFlow(false)
    private val closedFlow = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private val notice = MutableStateFlow<SendNotice?>(null)
    private val sendingClipboard = MutableStateFlow(false)

    /** A sensitive clip held until the user answers; kept off the UI state so the text never leaves this class. */
    private val pendingSensitive = MutableStateFlow<DeviceClip?>(null)

    /** Emits once the device was unpaired from here; the screen goes back. */
    val closed: SharedFlow<Unit> = closedFlow

    /** The screen state. */
    val state: StateFlow<DeviceDetailUiState> = combine(
        deviceState(application),
        notice,
        sendingClipboard,
        pendingSensitive,
    ) { base, sent, busy, ask ->
        base.copy(notice = sent, sendingClipboard = busy, askSensitive = ask != null)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DeviceDetailUiState())

    private fun deviceState(application: Application): kotlinx.coroutines.flow.Flow<DeviceDetailUiState> = combine(
        combine(CoreHost.peers, CoreHost.peerStatus, CoreHost.media, ::Triple),
        Settings.flow(application),
        pendingGrants,
        ringing,
        nowTicker(30_000),
    ) { (peers, status, media), settings, pending, ring, now ->
        val peer = peers.firstOrNull { it.deviceId == peerId }
        if (peer == null) {
            DeviceDetailUiState()
        } else {
            DeviceDetailUiState(
                row = DeviceRows.derive(listOf(peer), status, settings.defaultPeer, now).first(),
                grants = pending ?: peer.grants,
                lastKnown = peer.lastKnown,
                pairedAt = peer.pairedAt.toLong(),
                ringing = ring,
                capabilities = if (peer.online) peer.capabilities else emptyList(),
                media = MediaPlayers.realPlayers(media[peerId].orEmpty()),
                systemVolume = MediaPlayers.systemVolume(media[peerId].orEmpty()),
                mirrorApps = settings.mirrorApps.size,
            )
        }
    }

    init {
        viewModelScope.launch {
            CoreHost.events.filterIsInstance<HostEvent.PeerRefused>().collect { ev ->
                if (ev.peer != peerId) return@collect
                MediaRefusal.message(ev.frameType, ev.code, ev.message, CoreHost.labelOf(peerId))?.let(::post)
            }
        }
    }

    /** Sets the local label (the device cannot change it, SR-8). */
    fun rename(label: String) {
        val clean = label.trim()
        if (clean.isEmpty()) return
        launchCore({
            it.setLabel(peerId, clean)
            CoreHost.refreshPeers()
        })
    }

    /** Makes this device the one-tap send target. */
    fun setDefault() {
        viewModelScope.launch {
            Settings.update(getApplication()) { it.copy(defaultPeer = peerId) }
            post(UiMessage(R.string.ui_detail_default_set))
        }
    }

    /** Sends a ping with an optional message. */
    fun ping(message: String?) {
        val text = message?.trim()?.takeIf { it.isNotEmpty() }
        launchCore({ it.ping(peerId, text) }) { post(UiMessage(R.string.ui_detail_ping_sent)) }
    }

    /** Starts or stops ringing the device. */
    fun toggleFind() {
        val start = !ringing.value
        launchCore({ it.find(peerId, start) }) { ringing.value = start }
    }

    /** Dials the device now from its last-known addresses. */
    fun connect() {
        launchCore({ it.connect(peerId) })
    }

    /**
     * Changes one grant; only this local screen may do so (SR-14). Turning
     * [GrantKind.NOTIFICATIONS_OUT] off then withdraws every copy mirrored to the device
     * (SR-42); turning it on sends the current notifications that pass the filter.
     */
    fun setGrant(kind: GrantKind, value: Boolean) {
        if (kind == GrantKind.INPUT) return
        val base = pendingGrants.value ?: state.value.grants
        val next = GrantEdit.with(base, kind, value)
        pendingGrants.value = next
        val app = getApplication<Application>()
        viewModelScope.launch {
            if (kind == GrantKind.NOTIFICATIONS_OUT && value && !NotificationMirror.hasAccess(app)) {
                post(UiMessage(R.string.ui_mirror_need_access))
            }
            if (core {
                    it.setGrants(peerId, next)
                    CoreHost.refreshPeers()
                } == null
            ) {
                pendingGrants.value = null
                return@launch
            }
            if (kind == GrantKind.NOTIFICATIONS_OUT) {
                if (value) NotificationMirror.resyncPeer(app, peerId) else NotificationMirror.withdrawFrom(peerId)
            }
        }
    }

    /**
     * Sends picked or chosen [uris] to this device at once, the same way the share sheet does
     * (`CoreHost.sendUris` keeps each item readable until the transfer ends). The device may
     * be offline: the call is the same, only the confirmation says so. Runs in the app scope
     * so leaving the page does not drop a send that is being prepared.
     */
    fun sendFiles(uris: List<Uri>) {
        if (uris.isEmpty()) return
        val label = CoreHost.labelOf(peerId)
        val offline = state.value.row?.online != true
        sendUrisFromScreen(peerId, uris) { notice.value = SendNotice(uris.size, label, offline) }
    }

    /** Acts on what the clipboard held; see [DeviceSend.planClipboard]. */
    fun sendClipboard(read: ClipRead) {
        when (val plan = DeviceSend.planClipboard(read, state.value.grants.clipboardOut)) {
            is ClipboardPlan.Tell -> post(plan.message)
            is ClipboardPlan.Confirm -> pendingSensitive.value = plan.clip
            is ClipboardPlan.Send -> pushClipboard(plan.clip, sensitive = false)
        }
    }

    /** The user agreed to send the held sensitive clip; it goes out marked sensitive. */
    fun confirmSensitive() {
        val clip = pendingSensitive.value ?: return
        pendingSensitive.value = null
        pushClipboard(clip, sensitive = true)
    }

    /** The user backed out; the held clip is dropped. */
    fun cancelSensitive() {
        pendingSensitive.value = null
        post(UiMessage(R.string.toast_cancelled))
    }

    /** Hides the confirmation card. */
    fun dismissNotice() {
        notice.value = null
    }

    /**
     * Puts [clip] on this device's clipboard only, dialling first if it is offline (as the
     * share path does). One `push_clipboard_to` (or `push_clipboard_image_to`) limited to this
     * device, so text and copied images take the same way and the core's rules apply.
     */
    private fun pushClipboard(clip: DeviceClip, sensitive: Boolean) {
        if (!sendingClipboard.compareAndSet(expect = false, update = true)) return
        val app = getApplication<Application>()
        val label = CoreHost.labelOf(peerId)
        viewModelScope.launch {
            try {
                val delivery = core { h ->
                    if (!h.isOnline(peerId)) {
                        withTimeoutOrNull(CONNECT_TIMEOUT_MS) { runCatching { h.connect(peerId) } }
                    }
                    if (!h.isOnline(peerId)) return@core ClipDelivery.OFFLINE
                    val peers = listOf(peerId)
                    val p = when (clip) {
                        is DeviceClip.Text -> h.pushClipboardTo(peers, clip.text, sensitive)
                        is DeviceClip.Image -> h.pushClipboardImageTo(
                            peers,
                            ClipImage.read(app.contentResolver, Uri.parse(clip.uri)),
                            clip.mime,
                            sensitive,
                        )
                    }
                    DeviceSend.delivery(
                        peerId,
                        p.sentTo,
                        p.skippedEcho,
                        p.skippedNoGrant.toInt(),
                        p.skippedUnsupported.toInt(),
                        p.skippedNotPaired,
                    )
                }
                // null: the failure was already posted.
                if (delivery != null) post(DeviceSend.deliveryMessage(delivery, label))
            } finally {
                sendingClipboard.value = false
            }
        }
    }

    /** Sends a media command to one of the device's players; [value] is seek ms or volume. */
    fun mediaCommand(player: String, action: MediaAction, value: Long?) {
        launchCore({ it.sendMediaCommand(peerId, player, action, value?.toULong()) })
    }

    /** Sets the computer's master volume, 0 to 100, through the reserved player. */
    fun setSystemVolume(volume: Long) = mediaCommand(MediaPlayers.SYSTEM_VOLUME_ID, MediaAction.VOLUME, volume.coerceIn(0, 100))

    /** Unpairs the device, then closes the screen. */
    fun unpair() {
        launchCore({
            it.unpair(peerId)
            CoreHost.refreshPeers()
        }) {
            viewModelScope.launch {
                Settings.update(getApplication()) { s ->
                    if (s.defaultPeer == peerId) s.copy(defaultPeer = null) else s
                }
            }
            closedFlow.tryEmit(Unit)
        }
    }

    override fun onCleared() {
        super.onCleared()
        if (ringing.value) {
            val id = peerId
            org.playdrop.android.ui.common.AppScope.fireAndForget { it.find(id, false) }
        }
    }

    companion object {
        /** Route argument holding the hex Device ID. */
        const val ARG_ID = "id"

        private const val CONNECT_TIMEOUT_MS = 5_000L
    }
}
