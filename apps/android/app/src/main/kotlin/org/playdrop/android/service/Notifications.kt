package org.playdrop.android.service

import android.Manifest
import android.app.DownloadManager
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.RingtoneManager
import android.net.Uri
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import org.playdrop.android.R
import org.playdrop.android.core.HostLog
import org.playdrop.android.core.DownloadsSink
import org.playdrop.android.share.ClipboardSendActivity
import org.playdrop.android.share.PickAndSendActivity
import org.playdrop.core.HostEvent
import org.playdrop.core.TransferDirection
import java.text.NumberFormat

/**
 * Every notification the host layer posts. Channels: `service` (low, the persistent
 * notification), `transfers` (default), `pairing` (high), `find` (high, alarm sound and
 * vibration). No notification ever contains clipboard content (SR-32), and every action is an
 * explicit, immutable PendingIntent (SR-29).
 */
internal object Notifications {
    /** Channel of the persistent foreground-service notification. */
    const val CHANNEL_SERVICE = "service"

    /** Channel for offers, received files, failures and pings. */
    const val CHANNEL_TRANSFERS = "transfers"

    /** Channel for "new device paired" (SR-8). */
    const val CHANNEL_PAIRING = "pairing"

    /** Channel for "find my phone". */
    const val CHANNEL_FIND = "find"

    @Volatile
    private var warnedDisabled = false

    /** Creates the channels; safe to call more than once. */
    fun ensureChannels(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val alarm = RingtoneManager.getDefaultUri(RingtoneManager.TYPE_ALARM)
        val channels = listOf(
            NotificationChannel(CHANNEL_SERVICE, context.getString(R.string.channel_service), NotificationManager.IMPORTANCE_LOW)
                .apply {
                    description = context.getString(R.string.channel_service_desc)
                    setShowBadge(false)
                },
            NotificationChannel(CHANNEL_TRANSFERS, context.getString(R.string.channel_transfers), NotificationManager.IMPORTANCE_DEFAULT)
                .apply { description = context.getString(R.string.channel_transfers_desc) },
            NotificationChannel(CHANNEL_PAIRING, context.getString(R.string.channel_pairing), NotificationManager.IMPORTANCE_HIGH)
                .apply { description = context.getString(R.string.channel_pairing_desc) },
            NotificationChannel(CHANNEL_FIND, context.getString(R.string.channel_find), NotificationManager.IMPORTANCE_HIGH)
                .apply {
                    description = context.getString(R.string.channel_find_desc)
                    setSound(
                        alarm,
                        AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_ALARM)
                            .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                            .build(),
                    )
                    enableVibration(true)
                    vibrationPattern = longArrayOf(0, 800, 400, 800, 400, 800)
                    setBypassDnd(true)
                },
        )
        nm.createNotificationChannels(channels)
    }

    /**
     * The persistent notification: devices online plus "Send clipboard" and "Send to
     * <default>" (or "Send files" without a default).
     */
    fun service(context: Context, running: Boolean, online: Int, defaultLabel: String?): Notification {
        val text = when {
            !running -> context.getString(R.string.service_starting)
            online == 0 -> context.getString(R.string.service_none_online)
            else -> context.resources.getQuantityString(R.plurals.service_online, online, online)
        }
        val sendLabel = defaultLabel?.let { context.getString(R.string.action_send_to, it) }
            ?: context.getString(R.string.action_send_files)
        return NotificationCompat.Builder(context, CHANNEL_SERVICE)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.service_title))
            .setContentText(text)
            .setOngoing(true)
            .setShowWhen(false)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .setContentIntent(openApp(context))
            .addAction(0, context.getString(R.string.action_send_clipboard), activity(context, ClipboardSendActivity::class.java, 11))
            .addAction(0, sendLabel, activity(context, PickAndSendActivity::class.java, 12))
            .build()
    }

    /** Re-posts the persistent notification with fresh numbers. */
    fun updateService(context: Context, running: Boolean, online: Int, defaultLabel: String?) {
        post(context, NotificationPlan.SERVICE_ID, service(context, running, online, defaultLabel))
    }

    /** Tells the user the core could not start, with the reason (anti-pattern 18). */
    fun startFailed(context: Context, reason: String) {
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.service_failed_title))
            .setContentText(reason)
            .setStyle(NotificationCompat.BigTextStyle().bigText(reason))
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.START_FAILED_ID, n)
    }

    /**
     * Posts whatever [event] calls for. [labelOf] maps a hex Device ID to the local label
     * (falling back to the Short ID). Clipboard events are applied here too. With
     * [deferReceived] a completed receive posts nothing yet: its files are still being moved
     * into Downloads and [moveFinished] posts the notification.
     */
    suspend fun onEvent(context: Context, event: HostEvent, labelOf: (String) -> String, deferReceived: Boolean = false) {
        when (event) {
            is HostEvent.Paired -> paired(context, event)
            is HostEvent.TransferOffer -> offer(context, event, labelOf(event.peer))
            is HostEvent.TransferComplete -> {
                cancel(context, NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, event.id))
                if (event.direction == TransferDirection.RECEIVE && !deferReceived) received(context, event, labelOf(event.peer))
            }
            is HostEvent.TransferFailed -> {
                cancel(context, NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, event.id))
                failed(context, event, labelOf(event.peer))
            }
            is HostEvent.Ping -> ping(context, event, labelOf(event.peer))
            is HostEvent.Find -> if (event.start) find(context, labelOf(event.peer)) else FindRinger.stop(context)
            is HostEvent.Clipboard -> clipboard(context, event.peer, event.sensitive, labelOf(event.peer)) {
                ClipboardApplier.applyText(context, event.text, event.sensitive)
            }
            is HostEvent.ClipboardImage -> clipboard(context, event.peer, event.sensitive, labelOf(event.peer)) {
                ClipboardApplier.applyImage(context, event.data, event.mime, event.sensitive)
            }
            else -> Unit
        }
    }

    /**
     * "Moving 1,200 of 5,000 files into Downloads", silent and ongoing, under the transfer's
     * "received" ID with the [NotificationPlan.MOVING_TAG] tag. The final notice of the move
     * removes it ([moveFinished]); a start removes any a killed process left ([clearStaleMoving]).
     * The caller throttles it.
     */
    fun moving(context: Context, transferId: String, done: Int, total: Int) {
        val fmt = NumberFormat.getIntegerInstance()
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.moving_title))
            .setContentText(context.resources.getQuantityString(R.plurals.moving_text, total, fmt.format(done), fmt.format(total)))
            .setProgress(total, done, false)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setSilent(true)
            .setContentIntent(openApp(context))
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.RECEIVED, transferId), n, NotificationPlan.MOVING_TAG)
    }

    /**
     * Replaces every "Moving" notification a killed process left with a plain notice that the
     * files are in Download/PlayDrop (the sweep that follows moves anything still landed and
     * posts its own notice under the same ID).
     */
    fun clearStaleMoving(context: Context) {
        val nm = context.getSystemService(NotificationManager::class.java)
        val stale = try {
            nm.activeNotifications.filter { it.tag == NotificationPlan.MOVING_TAG }
        } catch (e: Exception) {
            HostLog.w("notify", "could not list active notifications", e)
            return
        }
        for (sbn in stale) {
            nm.cancel(sbn.tag, sbn.id)
            val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
                .setSmallIcon(R.drawable.ic_stat_playdrop)
                .setContentTitle(context.getString(R.string.moved_interrupted_title))
                .setContentText(context.getString(R.string.moved_interrupted_text))
                .setContentIntent(downloadsIntent(context, sbn.id))
                .setSilent(true)
                .setAutoCancel(true)
                .build()
            post(context, sbn.id, n)
        }
    }

    /**
     * Ends a move pass: always removes the ongoing "Moving" notice first. For a completed
     * transfer ([completed] not null) it then posts the normal "received" notification (with
     * [forceFolder], one that opens the Downloads list). For a transfer that failed or was
     * cancelled ([endedEarly]) it says how many files arrived before it stopped; the failure
     * keeps its own notification with the reason. For leftovers and retries it says how many
     * files reached Downloads. While files are left behind ([failed]) a separate notice says
     * how many, that they are moved automatically, and offers "Try again" (anti-pattern 18:
     * nothing fails silently); once none are left, that notice is removed.
     */
    fun moveFinished(
        context: Context,
        transferId: String,
        completed: HostEvent.TransferComplete?,
        endedEarly: Boolean,
        label: String?,
        moved: Int,
        failed: Int,
        forceFolder: Boolean,
    ) {
        val receivedId = NotificationPlan.notificationId(NotificationPlan.Kind.RECEIVED, transferId)
        NotificationManagerCompat.from(context).cancel(NotificationPlan.MOVING_TAG, receivedId)
        when (NotificationPlan.moveNotice(completed != null, endedEarly, moved)) {
            NotificationPlan.MoveNotice.RECEIVED ->
                completed?.let { received(context, it, label ?: it.peer.take(8), forceFolder) }
            NotificationPlan.MoveNotice.ARRIVED_BEFORE_END, NotificationPlan.MoveNotice.MOVED_AFTER_RESTART -> {
                val title = if (endedEarly) R.plurals.moved_before_end else R.plurals.moved_after_restart
                val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
                    .setSmallIcon(R.drawable.ic_stat_playdrop)
                    .setContentTitle(context.resources.getQuantityString(title, moved, moved))
                    .setContentText(context.getString(R.string.moved_after_restart_text))
                    .setContentIntent(downloadsIntent(context, receivedId))
                    .setAutoCancel(true)
                    .build()
                post(context, receivedId, n)
            }
            NotificationPlan.MoveNotice.NONE -> Unit
        }
        val incompleteId = NotificationPlan.notificationId(NotificationPlan.Kind.MOVE_INCOMPLETE, transferId)
        if (failed <= 0) {
            cancel(context, incompleteId)
            return
        }
        val text = context.getString(R.string.move_failed_text)
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.resources.getQuantityString(R.plurals.move_failed_title, failed, failed))
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setContentIntent(downloadsIntent(context, incompleteId))
            .addAction(0, context.getString(R.string.action_retry_move), broadcast(context, NotificationPlan.retryMoveAction(incompleteId)))
            .setOnlyAlertOnce(true)
            .setAutoCancel(true)
            .build()
        post(context, incompleteId, n)
    }

    /** Opens the system Downloads list; [requestCode] keeps it apart from other notifications' intents. */
    private fun downloadsIntent(context: Context, requestCode: Int): PendingIntent =
        PendingIntent.getActivity(context, requestCode, downloads(), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)

    /** Removes a notification. */
    fun cancel(context: Context, id: Int) {
        NotificationManagerCompat.from(context).cancel(id)
    }

    private fun paired(context: Context, e: HostEvent.Paired) {
        val text = context.getString(R.string.paired_text, e.name, e.shortId)
        val n = NotificationCompat.Builder(context, CHANNEL_PAIRING)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.paired_title))
            .setContentText(text)
            .setStyle(NotificationCompat.BigTextStyle().bigText(text))
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.PAIRED, e.peer), n)
    }

    private fun offer(context: Context, e: HostEvent.TransferOffer, label: String) {
        val id = NotificationPlan.notificationId(NotificationPlan.Kind.OFFER, e.id)
        val count = e.fileCount.toInt()
        val text = context.resources.getQuantityString(
            R.plurals.offer_text, count, count, NotificationPlan.humanBytes(e.totalBytes.toLong()),
        )
        val b = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.offer_title, label))
            .setContentText(text)
            .setCategory(NotificationCompat.CATEGORY_MESSAGE)
            .setContentIntent(openApp(context))
            .setOngoing(true)
        val titles = listOf(R.string.action_accept, R.string.action_decline)
        NotificationPlan.offerActions(e.id, id).zip(titles).forEach { (spec, title) ->
            b.addAction(0, context.getString(title), broadcast(context, spec))
        }
        post(context, id, b.build())
    }

    private fun received(context: Context, e: HostEvent.TransferComplete, label: String, forceFolder: Boolean = false) {
        val delivered = e.delivered.map { NotificationPlan.Delivered(it.path, it.location) }
        if (delivered.isEmpty()) return
        val plan = NotificationPlan.received(delivered, forceFolder) { DownloadsSink.guessMime(it) }
        val (title, tap) = when (plan) {
            is NotificationPlan.Received.One -> context.getString(R.string.received_one, plan.name) to
                Intent(Intent.ACTION_VIEW).setDataAndType(Uri.parse(plan.uri), plan.mime)
                    .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            is NotificationPlan.Received.OneRisky -> context.getString(R.string.received_one, plan.name) to downloads()
            is NotificationPlan.Received.Many ->
                context.resources.getQuantityString(R.plurals.received_many, plan.count, plan.count) to downloads()
        }
        val tapIntent = if (tap.resolveActivity(context.packageManager) != null) tap else downloads()
        val pi = PendingIntent.getActivity(
            context,
            NotificationPlan.notificationId(NotificationPlan.Kind.RECEIVED, e.id),
            tapIntent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(title)
            .setContentText(context.getString(R.string.received_from, label))
            .setContentIntent(pi)
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.RECEIVED, e.id), n)
    }

    private fun failed(context: Context, e: HostEvent.TransferFailed, label: String) {
        val title = if (e.direction == TransferDirection.SEND) {
            context.getString(R.string.transfer_failed_send, label)
        } else {
            context.getString(R.string.transfer_failed_receive, label)
        }
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(title)
            .setContentText(e.reason)
            .setStyle(NotificationCompat.BigTextStyle().bigText(e.reason))
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.FAILED, e.id), n)
    }

    private fun ping(context: Context, e: HostEvent.Ping, label: String) {
        val text = e.message?.takeIf { it.isNotBlank() } ?: context.getString(R.string.ping_no_message)
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.ping_title, label))
            .setContentText(text)
            .setContentIntent(openApp(context))
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.PING, e.peer), n)
    }

    private fun find(context: Context, label: String) {
        val stop = broadcast(context, NotificationPlan.stopFindAction())
        val n = NotificationCompat.Builder(context, CHANNEL_FIND)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.find_title, label))
            .setContentText(context.getString(R.string.find_text))
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setCategory(NotificationCompat.CATEGORY_ALARM)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setDeleteIntent(stop)
            .setContentIntent(stop)
            .addAction(0, context.getString(R.string.action_stop), stop)
            .build()
        post(context, NotificationPlan.FIND_ID, n)
        FindRinger.start(context)
    }

    private suspend fun clipboard(
        context: Context,
        peer: String,
        sensitive: Boolean,
        label: String,
        apply: suspend () -> Unit,
    ) {
        try {
            apply()
        } catch (e: Exception) {
            HostLog.w("clipboard", "applying a received clip failed", e)
            failNotice(context, peer, context.getString(R.string.clipboard_apply_failed, label, e.javaClass.simpleName))
            return
        }
        if (!sensitive) return
        // Quiet notice only, never the content (SR-32).
        val n = NotificationCompat.Builder(context, CHANNEL_SERVICE)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.clipboard_from, label))
            .setContentText(context.getString(R.string.clipboard_sensitive_text))
            .setSilent(true)
            .setAutoCancel(true)
            .setTimeoutAfter(CLIP_NOTICE_MS)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.CLIPBOARD, peer), n)
    }

    private fun failNotice(context: Context, peer: String, text: String) {
        val n = NotificationCompat.Builder(context, CHANNEL_TRANSFERS)
            .setSmallIcon(R.drawable.ic_stat_playdrop)
            .setContentTitle(context.getString(R.string.app_name))
            .setContentText(text)
            .setAutoCancel(true)
            .build()
        post(context, NotificationPlan.notificationId(NotificationPlan.Kind.CLIPBOARD, peer), n)
    }

    private fun post(context: Context, id: Int, n: Notification, tag: String? = null) {
        val granted = Build.VERSION.SDK_INT < 33 ||
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
        if (!granted || !NotificationManagerCompat.from(context).areNotificationsEnabled()) {
            if (!warnedDisabled) {
                warnedDisabled = true
                HostLog.w("notify", "notifications are off; offers, received files and find-my-phone stay invisible")
            }
            return
        }
        try {
            NotificationManagerCompat.from(context).notify(tag, id, n)
        } catch (e: SecurityException) {
            HostLog.w("notify", "posting a notification was refused", e)
        }
    }

    /** Opens the app's launcher activity. */
    private fun openApp(context: Context): PendingIntent {
        val launch = context.packageManager.getLaunchIntentForPackage(context.packageName)
            ?: Intent().setPackage(context.packageName)
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP)
        return PendingIntent.getActivity(context, 10, launch, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    private fun activity(context: Context, cls: Class<*>, requestCode: Int): PendingIntent {
        val intent = Intent(context, cls).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        return PendingIntent.getActivity(context, requestCode, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
    }

    /** Explicit broadcast to [ActionReceiver] built from a plan entry. */
    private fun broadcast(context: Context, spec: NotificationPlan.ActionSpec): PendingIntent {
        val intent = Intent(context, ActionReceiver::class.java).setAction(spec.action)
        spec.extras.forEach { (k, v) -> intent.putExtra(k, v) }
        return PendingIntent.getBroadcast(
            context,
            spec.requestCode,
            intent,
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
    }

    /** The system Downloads list. */
    private fun downloads(): Intent =
        Intent(DownloadManager.ACTION_VIEW_DOWNLOADS).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)

    private const val CLIP_NOTICE_MS = 10_000L
}
