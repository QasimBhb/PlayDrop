package org.playdrop.android.service

import android.content.ComponentName
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification
import org.playdrop.android.core.HostLog

/**
 * The system binds this only after the user gave PlayDrop "Notification access" (the first
 * of the three locks, SR-41). It never starts the core: it hands each notification to
 * [NotificationMirror], which sends nothing while the core is down.
 */
class MirrorListener : NotificationListenerService() {
    override fun onListenerConnected() {
        NotificationMirror.connected(this)
    }

    /**
     * Revoked access withdraws every mirrored copy (SR-42). A transient unbind (app update,
     * low memory) with access still granted asks to be bound again; the desktop copies stay,
     * and the resync on reconnect sends the current set anyway.
     */
    override fun onListenerDisconnected() {
        val stillGranted = NotificationMirror.hasAccess(this)
        NotificationMirror.disconnected(this, accessRevoked = !stillGranted)
        if (stillGranted) {
            try {
                requestRebind(ComponentName(this, MirrorListener::class.java))
            } catch (e: Exception) {
                HostLog.w("mirror", "could not ask to be bound again: ${e.javaClass.simpleName}")
            }
        }
    }

    override fun onNotificationPosted(sbn: StatusBarNotification?) {
        if (sbn != null) NotificationMirror.posted(this, sbn)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification?) {
        if (sbn != null) NotificationMirror.removed(sbn)
    }
}
