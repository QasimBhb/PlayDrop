package org.playdrop.android.ui

import android.app.Application
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.PendingPairing
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.clipboard.ClipboardEcho
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.HostEvent

/**
 * Activity-wide state: settings (for the onboarding gate and clipboard auto-push), the
 * pairing prompt shown over every screen, app-wide event notices, and the foreground
 * clipboard push.
 */
class AppViewModel(application: Application) : CoreViewModel(application) {
    private val answered = MutableStateFlow<PendingPairing?>(null)
    private val echo = ClipboardEcho()

    /** Stored settings; null until DataStore has been read once. */
    val settings: StateFlow<Settings.Values?> = Settings.flow(application)
        .stateIn(viewModelScope, SharingStarted.Eagerly, null)

    /** The pairing prompt to show, hidden as soon as the user answered it. */
    val pairingPrompt: StateFlow<PendingPairing?> = combine(CoreHost.pairingPrompt, answered) { p, a ->
        p.takeIf { it != a }
    }.stateIn(viewModelScope, SharingStarted.Eagerly, null)

    init {
        viewModelScope.launch {
            CoreHost.pairingPrompt.collect { if (it == null) answered.value = null }
        }
        viewModelScope.launch {
            CoreHost.events.collect { e ->
                when (e) {
                    is HostEvent.Paired -> post(UiMessage(R.string.ui_add_paired, listOf(e.name)))
                    is HostEvent.PairingFailed -> post(UiMessage(R.string.ui_add_pair_failed, listOf(e.reason)))
                    is HostEvent.RemovedByPeer -> post(UiMessage(R.string.ui_removed_by_peer, listOf(CoreHost.labelOf(e.peer))))
                    // A clip that arrived from a peer must not be pushed back (echo rule).
                    is HostEvent.Clipboard -> echo.remember(e.text)
                    else -> Unit
                }
            }
        }
    }

    /** Answers the pairing prompt. */
    fun answerPairing(accept: Boolean) {
        val current = pairingPrompt.value ?: return
        answered.value = current
        viewModelScope.launch { guarded { CoreHost.answerPairing(accept) } }
    }

    /**
     * Handles a clipboard change seen while the app is in the foreground. [text] is null for
     * a clip without text. Never logs the text (SR-32).
     */
    fun onClipboardChanged(text: String?, sensitive: Boolean) {
        if (settings.value?.autoClipboard != true || !CoreHost.running.value) return
        when (echo.decide(text, sensitive)) {
            ClipboardEcho.Decision.SKIP -> Unit
            ClipboardEcho.Decision.SENSITIVE -> {
                // Remember it so the same sensitive clip is not reported on every resume.
                text?.let(echo::remember)
                post(UiMessage(R.string.ui_clip_sensitive))
            }
            ClipboardEcho.Decision.TOO_LARGE -> {
                text?.let(echo::remember)
                post(UiMessage(R.string.ui_clip_too_large))
            }
            ClipboardEcho.Decision.PUSH -> {
                val clip = text ?: return
                echo.remember(clip)
                viewModelScope.launch { core { it.pushClipboard(clip, false) } }
            }
        }
    }
}
