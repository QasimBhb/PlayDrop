package org.playdrop.android.ui.settings

import android.app.Application
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.OwnIdentity
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.coreVersion

/** A paired device as the default-device picker lists it. */
data class PeerChoice(
    /** Hex Device ID. */
    val deviceId: String,
    /** Local label. */
    val label: String,
    /** Short ID (SR-8). */
    val shortId: String,
)

/** Everything the Settings tab shows. */
data class SettingsUiState(
    /** Stored settings; null until loaded. */
    val values: Settings.Values? = null,
    /** Paired devices for the default picker. */
    val peers: List<PeerChoice> = emptyList(),
    /** This phone's identity once the core runs. */
    val identity: OwnIdentity? = null,
    /** Core library version, or empty when it could not be read. */
    val coreVersion: String = "",
)

/** Checks a typed port; the KDE Connect and LocalSend ports are off limits (CLAUDE.md hard rules). */
object PortRules {
    /** Outcome of [check]. */
    sealed class Result {
        /** Usable. */
        data class Ok(val port: Int) : Result()

        /** Not a number in 1024..65535. */
        data object Invalid : Result()

        /** Belongs to KDE Connect (1714 to 1764) or LocalSend (53317). */
        data object Reserved : Result()
    }

    /** Validates [text]. */
    fun check(text: String): Result {
        val t = text.trim()
        if (t.isEmpty() || t.length > 5 || !t.all { it.isDigit() }) return Result.Invalid
        val port = t.toInt()
        return when {
            port !in 1024..65535 -> Result.Invalid
            port in 1714..1764 || port == 53317 -> Result.Reserved
            else -> Result.Ok(port)
        }
    }
}

/** State and actions of the Settings tab. */
class SettingsViewModel(application: Application) : CoreViewModel(application) {
    private val version = MutableStateFlow("")

    /** The screen state. */
    val state: StateFlow<SettingsUiState> = combine(
        Settings.flow(application),
        CoreHost.peers,
        CoreHost.identity,
        version,
    ) { values, peers, identity, v ->
        SettingsUiState(
            values = values,
            peers = peers.map { PeerChoice(it.deviceId, it.label, it.shortId) },
            identity = identity,
            coreVersion = v,
        )
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), SettingsUiState())

    init {
        viewModelScope.launch {
            // Loads the native library on first use, so keep it off the main thread.
            version.value = withContext(Dispatchers.IO) {
                try {
                    coreVersion()
                } catch (_: Throwable) {
                    ""
                }
            }
        }
    }

    /** Saves the name other devices see; it applies on the next service start. */
    fun saveName(name: String) {
        val clean = name.trim().take(MAX_NAME)
        if (clean.isEmpty()) {
            post(UiMessage(R.string.ui_settings_name_empty))
            return
        }
        update(R.string.ui_settings_saved_restart) { it.copy(deviceName = clean) }
    }

    /** Saves the port after [PortRules] approves it; it applies on the next service start. */
    fun savePort(text: String) {
        when (val r = PortRules.check(text)) {
            is PortRules.Result.Ok -> update(R.string.ui_settings_saved_restart) { it.copy(port = r.port) }
            PortRules.Result.Invalid -> post(UiMessage(R.string.ui_settings_port_invalid))
            PortRules.Result.Reserved -> post(UiMessage(R.string.ui_settings_port_reserved))
        }
    }

    /** Chooses the one-tap send target; null clears it. */
    fun setDefault(peer: String?) = update(null) { it.copy(defaultPeer = peer) }

    /** Turns foreground clipboard auto-push on or off. */
    fun setAutoClipboard(on: Boolean) = update(null) { it.copy(autoClipboard = on) }

    /** Remembers that the battery request was offered after the user tapped for it. */
    fun markBatteryPromptShown() = update(null) { it.copy(batteryPromptShown = true) }

    private fun update(done: Int?, transform: (Settings.Values) -> Settings.Values) {
        viewModelScope.launch {
            try {
                Settings.update(getApplication(), transform)
                if (done != null) post(UiMessage(done))
            } catch (e: Throwable) {
                post(UiMessage.fromOrRethrow(e))
            }
        }
    }

    private companion object {
        const val MAX_NAME = 64
    }
}
