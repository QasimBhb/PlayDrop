package org.playdrop.android.ui.adddevice

import android.app.Application
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.ui.common.Addresses
import org.playdrop.android.ui.common.AppScope
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.Format
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.AddDeviceCandidate
import org.playdrop.core.HostEvent

/** Which way of pairing the screen shows. */
enum class AddMode {
    /** Devices found on the network plus a typed address. */
    NEARBY,

    /** This phone shows a QR code for the other device to scan. */
    SHOW_QR,

    /** This phone scans the other device's QR code. */
    SCAN_QR,
}

/** Everything the Add device screen shows. */
data class AddDeviceUiState(
    /** Seconds left in the 120 s window; 0 once it closed. */
    val windowSecsLeft: Long = 0,
    /** Devices that answered, one row per device. */
    val candidates: List<AddDeviceCandidate> = emptyList(),
    /** Current tab. */
    val mode: AddMode = AddMode.NEARBY,
    /** The one-time QR URI while it is valid; never logged (SR-9, SR-32). */
    val qrUri: String? = null,
    /** Seconds the QR code stays valid. */
    val qrSecsLeft: Long = 0,
    /** Name or address being paired with, while the exchange starts. */
    val pairingWith: String? = null,
)

/**
 * State and actions of the Add device screen. The 120 s window opens when the view model is
 * created and closes when it is cleared, so a rotation does not restart it.
 */
class AddDeviceViewModel(application: Application) : CoreViewModel(application) {
    private val local = MutableStateFlow(Local())
    private val doneFlow = MutableSharedFlow<Unit>(extraBufferCapacity = 1)
    private var scanArmed = true

    /** Emits once pairing finished; the screen goes back to Devices. */
    val done: SharedFlow<Unit> = doneFlow

    /** The screen state. */
    val state: StateFlow<AddDeviceUiState> = combine(local, CoreHost.addDeviceCandidates) { l, c ->
        val now = Format.nowSecs()
        AddDeviceUiState(
            windowSecsLeft = (l.windowDeadline - now).coerceAtLeast(0),
            candidates = Candidates.collapse(c),
            mode = l.mode,
            qrUri = l.qrUri.takeIf { l.qrDeadline > now },
            qrSecsLeft = (l.qrDeadline - now).coerceAtLeast(0),
            pairingWith = l.pairingWith,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), AddDeviceUiState())

    init {
        openWindow()
        viewModelScope.launch {
            while (isActive) {
                delay(1_000)
                local.update { it.copy(tick = it.tick + 1) }
            }
        }
        viewModelScope.launch {
            CoreHost.events.collect { e ->
                when (e) {
                    // The app-level view model shows the snackbar for both outcomes.
                    is HostEvent.Paired -> {
                        local.update { it.copy(pairingWith = null) }
                        doneFlow.tryEmit(Unit)
                    }
                    is HostEvent.PairingFailed -> {
                        local.update { it.copy(pairingWith = null) }
                        scanArmed = true
                    }
                    else -> Unit
                }
            }
        }
    }

    /** Opens (or reopens) the 120 s window in which other devices may see this one (SR-6). */
    fun openWindow() {
        viewModelScope.launch {
            if (guarded { CoreHost.openAddDeviceWindow() } != null) {
                local.update { it.copy(windowDeadline = Format.nowSecs() + WINDOW_SECS) }
            }
        }
    }

    /** Switches tabs; leaving "Show QR" hides the code. */
    fun setMode(mode: AddMode) {
        local.update {
            it.copy(mode = mode, qrUri = if (mode == AddMode.SHOW_QR) it.qrUri else null, qrDeadline = 0)
        }
        if (mode == AddMode.SHOW_QR) showQr()
        if (mode == AddMode.SCAN_QR) scanArmed = true
    }

    /** Asks the core for a fresh one-time QR URI, valid for 120 s. */
    fun showQr() {
        viewModelScope.launch {
            val uri = guarded { CoreHost.showQr() } ?: return@launch
            val now = Format.nowSecs()
            local.update {
                it.copy(qrUri = uri, qrDeadline = now + WINDOW_SECS, windowDeadline = now + WINDOW_SECS)
            }
        }
    }

    /** Starts SAS pairing with a found device. */
    fun pair(candidate: AddDeviceCandidate) = pairAddr(candidate.addr, candidate.name)

    /** Starts SAS pairing with a typed address; the default port is added when missing. */
    fun pairTyped(text: String) {
        val addr = Addresses.normalize(text)
        if (addr == null) {
            post(UiMessage(R.string.ui_add_bad_address))
            return
        }
        pairAddr(addr, addr)
    }

    private fun pairAddr(addr: String, shown: String) {
        local.update { it.copy(pairingWith = shown) }
        viewModelScope.launch {
            if (core { it.pairWith(addr) } == null) local.update { it.copy(pairingWith = null) }
        }
    }

    /** A QR code was decoded; pairs once per match (the camera keeps delivering frames). */
    fun onScanned(uri: String) {
        if (!scanArmed) return
        scanArmed = false
        local.update { it.copy(pairingWith = "") }
        viewModelScope.launch {
            if (core { it.scanQr(uri) } == null) {
                local.update { it.copy(pairingWith = null) }
                delay(RESCAN_DELAY_MS)
                scanArmed = true
            }
        }
    }

    override fun onCleared() {
        AppScope.launch { runCatching { CoreHost.closeAddDeviceWindow() } }
    }

    private data class Local(
        val windowDeadline: Long = 0,
        val mode: AddMode = AddMode.NEARBY,
        val qrUri: String? = null,
        val qrDeadline: Long = 0,
        val pairingWith: String? = null,
        val tick: Long = 0,
    )

    companion object {
        /** The window and QR lifetime (SR-6, SR-9). */
        const val WINDOW_SECS = 120L
        private const val RESCAN_DELAY_MS = 2_000L
    }
}
