package org.playdrop.android.ui.devices

import android.app.Application
import android.net.Uri
import androidx.lifecycle.viewModelScope
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

/** Everything the Devices screen shows. */
data class DevicesUiState(
    /** The core runs in the service. */
    val running: Boolean = true,
    /** Paired devices as cards. */
    val rows: List<DeviceRow> = emptyList(),
    /** Where "Send files" goes without asking; null when the user has to choose. */
    val sendTarget: String? = null,
)

/** State and actions of the Devices screen. */
class DevicesViewModel(application: Application) : CoreViewModel(application) {
    /** The screen state, refreshed on every peer, status or settings change and twice a minute. */
    val state: StateFlow<DevicesUiState> = combine(
        CoreHost.running,
        CoreHost.peers,
        CoreHost.peerStatus,
        Settings.flow(application),
        nowTicker(30_000),
    ) { running, peers, status, settings, now ->
        DevicesUiState(
            running = running,
            rows = DeviceRows.derive(peers, status, settings.defaultPeer, now),
            sendTarget = DeviceRows.sendTarget(peers, settings.defaultPeer),
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DevicesUiState())

    /** Starts the foreground service. */
    fun startService() {
        try {
            CoreHost.ensureServiceRunning(getApplication())
        } catch (e: Exception) {
            post(UiMessage(R.string.ui_err_start_failed, listOf(e.message ?: e.javaClass.simpleName)))
        }
    }

    /** Makes [peer] the one-tap send target. */
    fun setDefault(peer: String) {
        viewModelScope.launch {
            Settings.update(getApplication()) { it.copy(defaultPeer = peer) }
        }
    }

    /** Sends the picked [uris] to [peer]; progress shows on the Transfers screen. */
    fun send(peer: String, uris: List<Uri>) {
        if (uris.isEmpty()) return
        val label = state.value.rows.firstOrNull { it.deviceId == peer }?.label ?: ""
        viewModelScope.launch {
            val id = guarded { CoreHost.sendUris(getApplication(), peer, uris) }
            if (id != null) {
                post(UiMessage(R.plurals.ui_devices_sending, listOf(uris.size, label), quantity = uris.size))
            }
        }
    }
}
