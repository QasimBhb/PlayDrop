package org.playdrop.android.service

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.widget.Toast
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import org.playdrop.android.R
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog

/**
 * Handles notification actions: accept or decline an offer, stop the find ringer. Not
 * exported; only our own immutable PendingIntents reach it (SR-29).
 */
class ActionReceiver : BroadcastReceiver() {
    /** Dispatches one action. */
    override fun onReceive(context: Context, intent: Intent) {
        val app = context.applicationContext
        when (intent.action) {
            NotificationPlan.ACTION_STOP_FIND -> FindRinger.stop(app)
            NotificationPlan.ACTION_ACCEPT_OFFER, NotificationPlan.ACTION_DECLINE_OFFER -> {
                val id = intent.getStringExtra(NotificationPlan.EXTRA_TRANSFER_ID) ?: return
                val notificationId = intent.getStringExtra(NotificationPlan.EXTRA_NOTIFICATION_ID)?.toIntOrNull()
                val accept = intent.action == NotificationPlan.ACTION_ACCEPT_OFFER
                val pending = goAsync()
                scope.launch {
                    try {
                        CoreHost.withHost { it.respondOffer(id, accept) }
                        notificationId?.let { Notifications.cancel(app, it) }
                    } catch (e: Exception) {
                        HostLog.w("offer", "answering an offer failed", e)
                        withContext(Dispatchers.Main) {
                            Toast.makeText(app, app.getString(R.string.offer_answer_failed, e.message ?: e.javaClass.simpleName), Toast.LENGTH_LONG).show()
                        }
                    } finally {
                        pending.finish()
                    }
                }
            }
            else -> HostLog.w("action", "unknown notification action ignored")
        }
    }

    private companion object {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    }
}
