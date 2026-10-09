package org.playdrop.android.ui.common

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.core.CoreHost
import org.playdrop.core.PlayDropHost

/**
 * Base for view models that call the core. Every call runs on [Dispatchers.IO] and every
 * failure becomes a [UiMessage] on [messages] (anti-pattern 18); nothing is logged (SR-32).
 */
abstract class CoreViewModel(application: Application) : AndroidViewModel(application) {
    private val messageFlow = MutableSharedFlow<UiMessage>(extraBufferCapacity = 16)

    /** One-off texts for the snackbar. */
    val messages: SharedFlow<UiMessage> = messageFlow

    /** Queues a snackbar text. */
    protected fun post(message: UiMessage) {
        messageFlow.tryEmit(message)
    }

    /** Runs [block] against the host off the main thread; null after a failure (already posted). */
    protected suspend fun <T> core(block: suspend (PlayDropHost) -> T): T? = guarded { CoreHost.withHost(block) }

    /**
     * Runs a facade call such as `CoreHost.showQr()` off the main thread; null after a
     * failure (already posted).
     */
    protected suspend fun <T> guarded(block: suspend () -> T): T? =
        try {
            withContext(Dispatchers.IO) { block() }
        } catch (e: Throwable) {
            post(UiMessage.fromOrRethrow(e))
            null
        }

    /** Launches [block] in [viewModelScope] through [core]; [onSuccess] runs on the main thread. */
    protected fun <T> launchCore(block: suspend (PlayDropHost) -> T, onSuccess: (T) -> Unit = {}) {
        viewModelScope.launch {
            val result = core(block)
            if (result != null) onSuccess(result)
        }
    }
}

/**
 * A process-wide scope for calls that must finish after the screen that started them is
 * gone, such as closing the "Add device" window when its view model is cleared.
 */
object AppScope : CoroutineScope by CoroutineScope(SupervisorJob() + Dispatchers.IO) {
    /** Fire and forget a core call; failures are dropped because no screen is left to show them. */
    fun fireAndForget(block: suspend (PlayDropHost) -> Unit) {
        launch {
            runCatching { CoreHost.withHost(block) }
        }
    }
}
