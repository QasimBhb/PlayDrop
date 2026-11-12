package org.playdrop.android.share

import android.content.Context
import android.widget.Toast
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
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

    /**
     * Sends [prepared] items to [peer] from the process scope, taking over the caller's
     * reference: the items are released if the send cannot start, otherwise when it ends.
     */
    fun sendFiles(context: Context, peer: String, prepared: UriSource.Prepared) {
        val app = context.applicationContext
        val label = CoreHost.labelOf(peer)
        CoreHostImpl.scope.launch {
            var send: UriSource.PreparedSend? = null
            try {
                val one = try {
                    prepared.sends(1).single()
                } finally {
                    prepared.release()
                }
                send = one
                val id = CoreHost.withHost { it.send(peer, one.sources, null) }
                one.attachTo(id)
                HostLog.i("share", "transfer ${id.take(8)} started: ${one.sources.size} items, ${one.totalBytes} bytes")
                val n = one.sources.size
                toast(app, app.resources.getQuantityString(R.plurals.toast_sending_items, n, n, label))
            } catch (e: Exception) {
                send?.release()
                HostLog.wNoDetail("share", "send failed to start", e)
                toast(app, failedText(app, e))
            }
        }
    }

    /** The toast for a send that could not start, in plain words. */
    fun failedText(context: Context, e: Exception): String = when (e) {
        is UriSource.TooManyItems -> context.getString(R.string.toast_too_many_items_open)
        else -> context.getString(R.string.toast_send_failed, reason(e))
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
                HostLog.wNoDetail("share", "clipboard send failed", e)
                toast(app, app.getString(R.string.toast_send_failed, reason(e)))
            }
        }
    }

    /** A clip to send: text, or an image read from the clipboard. Never logged (SR-32). */
    sealed class Clip {
        class Text(val text: String) : Clip()
        class Image(val bytes: ByteArray, val mime: String) : Clip()
    }

    /**
     * Sends one [clip] to the [chosen] devices and returns one result per device, in order.
     * With [dialFirst] (shared text) offline devices are dialled for up to 5 s first. The
     * devices that can take it now get it through one `push_clipboard_to` (or
     * `push_clipboard_image_to`) call, so the core's echo check covers the whole push and
     * any ticked set is reached. [sensitive] goes to every device (SR-24).
     */
    suspend fun deliverClip(chosen: List<String>, clip: Clip, sensitive: Boolean, dialFirst: Boolean): List<DeviceResult> {
        val nowKind = if (clip is Clip.Image) SendKind.CLIPBOARD_IMAGE else SendKind.CLIPBOARD_TEXT
        if (dialFirst) dial(chosen)
        val online = CoreHost.withHost { h -> CoreHost.peers.value.associate { it.deviceId to h.isOnline(it.deviceId) } }
        val candidates = CoreHost.peers.value.map { Candidate.of(it).copy(online = online[it.deviceId] == true) }
        val blockedNow = chosen.associateWith { id -> candidates.firstOrNull { it.id == id }?.let(nowKind::blocked) ?: Blocked.OFFLINE }
        val targets = ClipPlan.targets(chosen, candidates.filter { nowKind.blocked(it) == null }.map { it.id })
        var pushed: Pushed? = null
        var error: String? = null
        if (targets.isNotEmpty()) {
            try {
                pushed = CoreHost.withHost { h ->
                    val p = when (clip) {
                        is Clip.Text -> h.pushClipboardTo(targets, clip.text, sensitive)
                        is Clip.Image -> h.pushClipboardImageTo(targets, clip.bytes, clip.mime, sensitive)
                    }
                    Pushed(
                        sentTo = p.sentTo.toSet(),
                        echo = p.skippedEcho,
                        notPaired = p.skippedNotPaired.toSet(),
                        skippedOffline = p.skippedOffline.toInt(),
                        skippedNoGrant = p.skippedNoGrant.toInt(),
                        skippedUnsupported = p.skippedUnsupported.toInt(),
                    )
                }
            } catch (e: Exception) {
                HostLog.w("clipboard", "clipboard send failed", e)
                error = reason(e)
            }
        }
        return ClipPlan.results(chosen, CoreHost::labelOf, blockedNow, pushed, error)
    }

    /** Dials every offline device of [peers] at once, waiting at most 5 s. */
    private suspend fun dial(peers: List<String>) {
        CoreHost.withHost { h ->
            coroutineScope {
                peers.filter { !h.isOnline(it) }.map { peer ->
                    async { withTimeoutOrNull(CONNECT_TIMEOUT_MS) { runCatching { h.connect(peer) } } }
                }.awaitAll()
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
