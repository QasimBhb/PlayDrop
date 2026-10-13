package org.playdrop.android.ui.device

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.common.nowTicker
import org.playdrop.android.ui.devices.DeviceRow
import org.playdrop.android.ui.devices.DeviceRows
import org.playdrop.android.ui.input.InputSupport
import org.playdrop.core.Grants
import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState

/** One of the six per-device permissions (SR-14). */
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
    }

    /** A copy of [g] with [kind] set to [value]. */
    fun with(g: Grants, kind: GrantKind, value: Boolean): Grants = when (kind) {
        GrantKind.FILES -> g.copy(files = value)
        GrantKind.CLIPBOARD_IN -> g.copy(clipboardIn = value)
        GrantKind.CLIPBOARD_OUT -> g.copy(clipboardOut = value)
        GrantKind.MEDIA -> g.copy(media = value)
        GrantKind.FIND -> g.copy(find = value)
        GrantKind.INPUT -> g.copy(input = value)
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
    /** Media players the device reports, for the "Now playing" cards. */
    val media: List<MediaState> = emptyList(),
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

    /** Emits once the device was unpaired from here; the screen goes back. */
    val closed: SharedFlow<Unit> = closedFlow

    /** The screen state. */
    val state: StateFlow<DeviceDetailUiState> = combine(
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
                media = media[peerId].orEmpty(),
            )
        }
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DeviceDetailUiState())

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

    /** Changes one grant; only this local screen may do so (SR-14). */
    fun setGrant(kind: GrantKind, value: Boolean) {
        if (kind == GrantKind.INPUT) return
        val base = pendingGrants.value ?: state.value.grants
        val next = GrantEdit.with(base, kind, value)
        pendingGrants.value = next
        viewModelScope.launch {
            if (core {
                    it.setGrants(peerId, next)
                    CoreHost.refreshPeers()
                } == null
            ) {
                pendingGrants.value = null
            }
        }
    }

    /** Sends a media command to one of the device's players; [value] is seek ms or volume. */
    fun mediaCommand(player: String, action: MediaAction, value: Long?) {
        launchCore({ it.sendMediaCommand(peerId, player, action, value?.toULong()) })
    }

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
        if (ringing.value) {
            val id = peerId
            org.playdrop.android.ui.common.AppScope.fireAndForget { it.find(id, false) }
        }
    }

    companion object {
        /** Route argument holding the hex Device ID. */
        const val ARG_ID = "id"
    }
}
