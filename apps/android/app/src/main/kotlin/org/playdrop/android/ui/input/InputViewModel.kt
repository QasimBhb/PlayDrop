package org.playdrop.android.ui.input

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterIsInstance
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.common.AppScope
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.core.HostEvent
import org.playdrop.core.InputEvent
import org.playdrop.core.PresenterAction

/** Everything the remote input screen shows. */
data class InputUiState(
    /** Local label of the peer; empty while unknown. */
    val label: String = "",
    /** The peer has a session up. */
    val online: Boolean = false,
    /** What the peer accepts. */
    val features: InputSupport.Features = InputSupport.Features(pointer = false, keys = false, presenter = false),
    /** Trackpad speed from settings. */
    val sensitivity: Float = Settings.DEFAULT_TRACKPAD_SENSITIVITY,
)

/**
 * Remote input for one peer (route argument [ARG_ID]). Frames go through one ordered queue to
 * the core so moves, buttons and keys reach the desktop in the order they were made; the
 * [InputSessionController] decides whether a session is open.
 */
class InputViewModel(application: Application, savedState: SavedStateHandle) : CoreViewModel(application) {
    private val peerId: String = checkNotNull(savedState.get<String>(ARG_ID))
    private val queue = Channel<InputEvent>(Channel.UNLIMITED)
    private val closedFlow = MutableSharedFlow<UiMessage>(extraBufferCapacity = 1)
    private val session = InputSessionController { ev -> CoreHost.withHost { it.sendInput(peerId, ev) } }

    /** Emits once when the screen must close, with the reason to show. */
    val closed: SharedFlow<UiMessage> = closedFlow

    /** The screen state. */
    val state: StateFlow<InputUiState> = combine(CoreHost.peers, Settings.flow(application)) { peers, settings ->
        val peer = peers.firstOrNull { it.deviceId == peerId }
        InputUiState(
            label = peer?.let { CoreHost.labelOf(peerId) }.orEmpty(),
            online = peer?.online == true,
            features = InputSupport.of(peer?.capabilities.orEmpty()),
            sensitivity = settings.trackpadSensitivity,
        )
    }.stateIn(viewModelScope, SharingStarted.Eagerly, InputUiState())

    init {
        viewModelScope.launch {
            for (ev in queue) handle(session.send(ev))
        }
        viewModelScope.launch {
            CoreHost.peers
                .map { peers -> peers.firstOrNull { it.deviceId == peerId }?.online == true }
                .distinctUntilChanged()
                .collect { handle(session.setOnline(it)) }
        }
        viewModelScope.launch {
            CoreHost.events.filterIsInstance<HostEvent.InputSessionEnded>().collect { ev ->
                if (ev.peer != peerId) return@collect
                session.remoteEnded()
                closedFlow.tryEmit(UiMessage(R.string.ui_input_ended, listOf(ev.reason)))
            }
        }
        viewModelScope.launch {
            CoreHost.events.filterIsInstance<HostEvent.PeerRefused>().collect { ev ->
                if (ev.peer != peerId || ev.frameType != FRAME_INPUT) return@collect
                if (ev.code == CODE_FORBIDDEN) {
                    refuse()
                } else {
                    post(UiMessage(R.string.ui_input_refused_other, listOf(ev.message ?: ev.code)))
                }
            }
        }
    }

    /** The screen is visible: open the session. */
    fun start() {
        viewModelScope.launch { handle(session.start()) }
    }

    /** The screen left the foreground: close the session. */
    fun stop() {
        viewModelScope.launch { handle(session.stop()) }
    }

    /** Queues frames for the open session; dropped silently while none is open. */
    fun send(events: List<InputEvent>) {
        events.forEach { queue.trySend(it) }
    }

    /** Queues one frame. */
    fun send(event: InputEvent) {
        queue.trySend(event)
    }

    /** Presenter shortcut. */
    fun presenter(action: PresenterAction) = send(InputEvent.Presenter(action = action))

    /** Stores a new trackpad speed. */
    fun setSensitivity(value: Float) {
        val clamped = value.coerceIn(Settings.MIN_TRACKPAD_SENSITIVITY, Settings.MAX_TRACKPAD_SENSITIVITY)
        viewModelScope.launch { Settings.update(getApplication()) { it.copy(trackpadSensitivity = clamped) } }
    }

    private suspend fun handle(outcome: InputSessionController.Outcome) {
        when (outcome) {
            InputSessionController.Outcome.Refused -> refuse()
            is InputSessionController.Outcome.Failed -> {
                // A dropped connection fails every queued move; one message is enough.
                val now = System.currentTimeMillis()
                if (now - lastFailureShownAt >= FAILURE_QUIET_MS) {
                    lastFailureShownAt = now
                    post(UiMessage.fromOrRethrow(outcome.error))
                }
            }
            else -> Unit
        }
    }

    /** The desktop has remote input off for this phone: stop for good, tell the user, leave. */
    private suspend fun refuse() {
        session.refused()
        if (refusedShown) return
        refusedShown = true
        closedFlow.tryEmit(UiMessage(R.string.ui_input_refused, listOf(CoreHost.labelOf(peerId))))
    }

    private var refusedShown = false
    private var lastFailureShownAt = 0L

    override fun onCleared() {
        queue.close()
        // The screen is gone; make sure the desktop releases everything even if onStop never ran.
        if (session.state == InputSessionController.State.ACTIVE) {
            val s = session
            AppScope.launch { runCatching { s.stop() } }
        }
    }

    companion object {
        /** Route argument holding the hex Device ID. */
        const val ARG_ID = "id"

        private const val FRAME_INPUT = "input"
        private const val CODE_FORBIDDEN = "forbidden"
        private const val FAILURE_QUIET_MS = 3_000L
    }
}
