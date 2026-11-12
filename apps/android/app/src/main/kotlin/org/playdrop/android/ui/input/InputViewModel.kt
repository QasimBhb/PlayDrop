package org.playdrop.android.ui.input

import android.app.Application
import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
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
import org.playdrop.core.InputEndReason
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
    /** Why remote input stopped, shown until the user closes it or tries again; null while it runs. */
    val ended: InputEnd? = null,
)

/**
 * Remote input for one peer (route argument [ARG_ID]). Frames go through one ordered queue to
 * the core so moves, buttons and keys reach the desktop in the order they were made; the
 * [InputSessionController] decides whether a session is open.
 */
class InputViewModel(application: Application, savedState: SavedStateHandle) : CoreViewModel(application) {
    private val peerId: String = checkNotNull(savedState.get<String>(ARG_ID))
    private val queue = Channel<InputEvent>(Channel.UNLIMITED)
    private val ended = MutableStateFlow<InputEnd?>(null)
    private val session = InputSessionController { ev -> CoreHost.withHost { it.sendInput(peerId, ev) } }

    /** The screen state. */
    val state: StateFlow<InputUiState> = combine(CoreHost.peers, Settings.flow(application), ended) { peers, settings, end ->
        val peer = peers.firstOrNull { it.deviceId == peerId }
        InputUiState(
            label = peer?.let { CoreHost.labelOf(peerId) }.orEmpty(),
            online = peer?.online == true,
            features = InputSupport.of(peer?.capabilities.orEmpty()),
            sensitivity = settings.trackpadSensitivity,
            ended = end,
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
                .collect {
                    handle(session.setOnline(it))
                    clearLostNoticeIfBack()
                }
        }
        viewModelScope.launch {
            // The core sends exactly one of these per session we started, with the reason as
            // a code (an older computer's refusal included), so this is where every end is told.
            CoreHost.events.filterIsInstance<HostEvent.InputSessionEnded>().collect { ev ->
                if (ev.peer != peerId) return@collect
                if (ev.code == InputEndReason.CONNECTION_LOST) {
                    connectionLost(ev.reason)
                    return@collect
                }
                lostWatch?.cancel()
                session.remoteEnded()
                ended.value = InputEnd.of(ev.code, CoreHost.labelOf(peerId), ev.reason)
            }
        }
        viewModelScope.launch {
            CoreHost.events.filterIsInstance<HostEvent.PeerRefused>().collect { ev ->
                if (ev.peer != peerId || ev.frameType != FRAME_INPUT) return@collect
                if (ev.code == CODE_FORBIDDEN) {
                    // The InputSessionEnded that follows says why; stop sending meanwhile.
                    refuse(InputEndReason.GRANT_OFF)
                } else {
                    post(UiMessage(R.string.ui_input_refused_other, listOf(ev.message ?: ev.code)))
                }
            }
        }
    }

    /** The screen is visible: open the session. */
    fun start() {
        viewModelScope.launch {
            handle(session.start())
            clearLostNoticeIfBack()
        }
    }

    /** "Try again" on the end notice: hide it and open the session again. */
    fun retry() {
        ended.value = null
        viewModelScope.launch { handle(session.retry()) }
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
            InputSessionController.Outcome.Refused -> refuse(InputEndReason.NOT_ACCEPTED)
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

    /**
     * The peer or the core refused input: stop sending until "Try again". The core's
     * [HostEvent.InputSessionEnded] carries the exact reason and replaces [likely]; this
     * notice only shows when that event does not come.
     */
    private suspend fun refuse(likely: InputEndReason) {
        session.refused()
        if (ended.value == null) ended.value = InputEnd.of(likely, CoreHost.labelOf(peerId), "")
    }

    private var lastFailureShownAt = 0L

    /** Waits for the peer after a lost connection; see [connectionLost]. */
    private var lostWatch: Job? = null

    /**
     * The connection dropped under an open session. The session stays wanted, so it reopens
     * by itself when the peer is back ([InputSessionController.setOnline]), or right away when
     * the connection was replaced without the peer ever looking offline. "Connection lost"
     * shows only when it is not back within [LOST_NOTICE_MS], and goes away when it reopens.
     */
    private suspend fun connectionLost(detail: String) {
        session.connectionLost()
        lostWatch?.cancel()
        lostWatch = viewModelScope.launch {
            delay(RESUME_PROBE_MS)
            // A failure here is the stale "online" of a link that is going away; the reconnect
            // path tries again, so it is not shown.
            session.resume()
            clearLostNoticeIfBack()
            delay(LOST_NOTICE_MS - RESUME_PROBE_MS)
            if (session.state != InputSessionController.State.ACTIVE && ended.value == null) {
                ended.value = InputEnd.of(InputEndReason.CONNECTION_LOST, CoreHost.labelOf(peerId), detail)
            }
        }
    }

    /** The session is open again: a "Connection lost" notice is out of date. */
    private fun clearLostNoticeIfBack() {
        if (session.state != InputSessionController.State.ACTIVE) return
        lostWatch?.cancel()
        if (ended.value?.code == InputEndReason.CONNECTION_LOST) ended.value = null
    }

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

        /** After a lost connection, how long before the session is retried on a peer still online. */
        private const val RESUME_PROBE_MS = 1_000L

        /** How long a lost connection may take to come back before the screen says so. */
        private const val LOST_NOTICE_MS = 5_000L
    }
}
