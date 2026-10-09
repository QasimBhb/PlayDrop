package org.playdrop.android.share

import android.content.Context
import android.widget.Toast
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.CoreHostImpl
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.core.PlayDropException

/**
 * Hands sends from the short-lived share activities to the process scope, so the activity
 * can finish while the transfer continues in the service. Every outcome reaches the user as
 * a toast (anti-pattern 18).
 */
internal object ShareSender {
    private const val CONNECT_TIMEOUT_MS = 5_000L

    /** Sends [prepared] items to [peer]; releases them if the send cannot start. */
    fun sendFiles(context: Context, peer: String, prepared: UriSource.PreparedSend) {
        val app = context.applicationContext
        val label = CoreHost.labelOf(peer)
        CoreHostImpl.scope.launch {
            try {
                val id = CoreHost.withHost { it.send(peer, prepared.sources, null) }
                prepared.attachTo(id)
                HostLog.i("share", "transfer ${id.take(8)} started: ${prepared.sources.size} items, ${prepared.totalBytes} bytes")
                val n = prepared.sources.size
                toast(app, app.resources.getQuantityString(R.plurals.toast_sending_items, n, n, label))
            } catch (e: Exception) {
                prepared.release()
                HostLog.w("share", "send failed to start", e)
                toast(app, app.getString(R.string.toast_send_failed, reason(e)))
            }
        }
    }

    /** Puts [text] on [peer]'s clipboard, dialling it first if it is offline. */
    fun sendText(context: Context, peer: String, text: String) {
        val app = context.applicationContext
        val label = CoreHost.labelOf(peer)
        CoreHostImpl.scope.launch {
            try {
                val online = CoreHost.withHost { h ->
                    if (!h.isOnline(peer)) {
                        withTimeoutOrNull(CONNECT_TIMEOUT_MS) { runCatching { h.connect(peer) } }
                    }
                    h.isOnline(peer)
                }
                if (!online) {
                    toast(app, app.getString(R.string.toast_peer_offline, label))
                    return@launch
                }
                val sent = CoreHost.withHost { it.sendClipboard(peer, text, false) }
                toast(app, if (sent) app.getString(R.string.toast_copied_to, label) else app.getString(R.string.toast_clipboard_unchanged))
            } catch (e: Exception) {
                HostLog.w("share", "clipboard send failed", e)
                toast(app, app.getString(R.string.toast_send_failed, reason(e)))
            }
        }
    }

    /** A short reason for a toast. */
    fun reason(e: Exception): String = when (e) {
        is PlayDropException.Io -> e.detail
        is PlayDropException.Other -> e.detail
        is PlayDropException.NotPaired -> e.detail
        is PlayDropException.Forbidden -> e.detail
        is PlayDropException.TooLarge -> e.detail
        is PlayDropException.PathInvalid -> e.detail
        is PlayDropException.NoSpace -> e.detail
        else -> e.message ?: e.javaClass.simpleName
    }

    /** Shows a toast from any thread. */
    suspend fun toast(context: Context, text: String) {
        withContext(Dispatchers.Main) { Toast.makeText(context.applicationContext, text, Toast.LENGTH_LONG).show() }
    }
}
