package org.playdrop.android.share

import android.content.Context
import androidx.activity.ComponentActivity
import kotlinx.coroutines.launch
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.CoreHostImpl
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.UriSource
import org.playdrop.android.data.Settings

/**
 * Starts the sends of [prepared] to the [chosen] devices after "Send" in the chooser, in the
 * process scope ([ManySend]), and takes over the caller's reference to the items. Every
 * device after the first gets its own share of the items, prepared there and then (the
 * activity that holds the grant is normally still alive: the card stays up for at least
 * the cancel window). One toast names every device at the end, or says Cancelled.
 */
internal fun startFileSends(
    context: Context,
    prepared: UriSource.Prepared,
    chosen: List<String>,
    title: String,
): ManySend<UriSource.PreparedSend> {
    val app = context.applicationContext
    val items = prepared.count
    val wasOnline = CoreHost.peers.value.associate { it.deviceId to it.online }
    val run = ManySend(
        title = title,
        peers = chosen,
        scope = CoreHostImpl.scope,
        prepare = { n ->
            try {
                if (n == 0) emptyList() else prepared.sends(n)
            } finally {
                prepared.release()
            }
        },
        start = { peer, share ->
            try {
                val id = CoreHost.withHost { it.send(peer, share.sources, null) }
                share.attachTo(id)
                HostLog.i("share", "transfer ${id.take(8)} started: ${share.sources.size} items, ${share.totalBytes} bytes")
                id
            } catch (e: Exception) {
                share.release()
                HostLog.wNoDetail("share", "send failed to start", e)
                throw e
            }
        },
        stop = { id -> CoreHost.withHost { it.cancelTransfer(id) } },
        release = { it.release() },
        report = { r ->
            val text = when (r) {
                ManySend.Report.Cancelled -> app.getString(R.string.toast_cancelled)
                is ManySend.Report.PrepareFailed -> {
                    HostLog.wNoDetail("share", "preparing the items for every device failed", r.error)
                    ShareSender.failedText(app, r.error)
                }
                is ManySend.Report.Done -> SendReport.sentence(fileResults(r.outcomes, chosen, wasOnline), SharePhrases.files(app, items))
            }
            ShareSender.toast(app, text)
        },
    )
    run.launch()
    return run
}

/** One result per device for a finished [ManySend] of files. */
private fun fileResults(outcomes: List<FanOut.Outcome<Int>>, chosen: List<String>, wasOnline: Map<String, Boolean>): List<DeviceResult> =
    outcomes.mapNotNull { o ->
        val peer = chosen[o.target]
        val label = CoreHost.labelOf(peer)
        when (o) {
            is FanOut.Outcome.Started -> if (wasOnline[peer] == true) DeviceResult.Sent(label) else DeviceResult.Waiting(label)
            is FanOut.Outcome.Failed -> DeviceResult.Failed(label, ShareSender.reason(o.error))
            is FanOut.Outcome.Skipped, is FanOut.Outcome.Stopped -> null
        }
    }

/**
 * Shows the sending card of [run] until its window is over and every send started, then
 * finishes the activity. Cancel stops every send. Leaving the screen ends the wait and the
 * sends go on. A rotation cancels this wait without deciding: the recreated activity shows
 * the card again ([ManySends.find]).
 */
internal suspend fun ComponentActivity.holdSendingCard(sheet: SendSheet, run: ManySend<*>) {
    if (sheet.holdSending(run.title, run.allStarted)) run.cancel() else run.keep()
    finish()
}

/** Stores [id] as the default device; outlives the activity that asked. */
internal fun Context.storeDefaultDevice(id: String) {
    val app = applicationContext
    CoreHostImpl.scope.launch { Settings.update(app) { it.copy(defaultPeer = id) } }
}
