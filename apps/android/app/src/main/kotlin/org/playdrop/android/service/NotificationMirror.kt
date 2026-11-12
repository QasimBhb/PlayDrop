package org.playdrop.android.service

import android.app.Notification
import android.content.Context
import android.content.pm.PackageManager
import android.os.Process
import android.service.notification.StatusBarNotification
import androidx.core.app.NotificationManagerCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import org.playdrop.android.core.CoreHost
import org.playdrop.android.core.HostLog
import org.playdrop.android.data.Settings
import org.playdrop.core.MirroredNotification

/**
 * Notification mirroring, phone to desktop (docs/design/notification-mirroring.md). The
 * [MirrorListener] hands every posted and removed notification here; this object filters it
 * ([MirrorFilter]), sends it to each online peer whose `notificationsOut` grant is on, and
 * remembers what went where ([MirrorBook]) so a removal, a desktop `dismiss`, a grant turned
 * off, an app taken off the allow list or lost access can be answered.
 *
 * A desktop closes every mirrored copy when the phone goes offline, and opaque keys change
 * with the process, so a peer that comes (back) online, the listener connecting and the grant
 * being turned on each send the current set again ([resyncPeer]).
 *
 * Work runs in order on one coroutine. The core is never started from here: with the core
 * down, nothing is sent. A send that fails (grant off, peer offline, no capability) counts as
 * not sent and is dropped quietly. Nothing logged here names an app, a title, a text or a
 * key (SR-43).
 */
internal object NotificationMirror {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val queue = Channel<suspend () -> Unit>(Channel.UNLIMITED)
    private val lock = Mutex()
    private val book = MirrorBook()

    /** The connected listener, for active notifications and `cancelNotification`; null while access is off. */
    @Volatile
    private var listener: MirrorListener? = null

    init {
        scope.launch {
            for (job in queue) {
                try {
                    job()
                } catch (e: Exception) {
                    // The class name only: a message could carry notification content (SR-43).
                    HostLog.w("mirror", "mirroring step failed: ${e.javaClass.simpleName}")
                }
            }
        }
    }

    /** True when the user gave PlayDrop notification access in system Settings. */
    fun hasAccess(context: Context): Boolean =
        NotificationManagerCompat.getEnabledListenerPackages(context).contains(context.packageName)

    /** The listener is bound: notifications start flowing, and online peers get the current set. */
    fun connected(service: MirrorListener) {
        listener = service
        HostLog.i("mirror", "notification access on")
        val app = service.applicationContext
        queue.trySend {
            for (peer in eligiblePeers()) resync(app, peer)
        }
    }

    /**
     * The listener is unbound, usually because access was revoked (SR-42): every mirrored
     * copy is withdrawn from its peers, best effort, and the book is cleared.
     */
    fun disconnected(service: MirrorListener, accessRevoked: Boolean) {
        if (listener === service) listener = null
        if (!accessRevoked) {
            // A transient unbind: keep the book; the copies stay and the rebind resyncs.
            HostLog.i("mirror", "listener unbound; asking to be bound again")
            return
        }
        HostLog.i("mirror", "notification access off")
        queue.trySend {
            lock.withLock {
                for (entry in book.clear()) withdraw(entry)
            }
        }
    }

    /** A notification was posted or updated. */
    fun posted(context: Context, sbn: StatusBarNotification) {
        if (!CoreHost.running.value) return
        val app = context.applicationContext
        queue.trySend { mirror(app, sbn, eligiblePeers(), resend = false) }
    }

    /** A notification went away: tell every peer that showed it. */
    fun removed(sbn: StatusBarNotification) {
        val androidKey = sbn.key
        queue.trySend {
            lock.withLock { book.remove(androidKey)?.let { withdraw(it) } }
        }
    }

    /**
     * A desktop closed its copy (`dismiss`, protocol.md 5.16): cancel the phone's notification.
     * The core only delivers this with our `notificationsOut` grant for [peer]; a key we never
     * sent to [peer] is ignored. The listener's removal callback then withdraws the other
     * peers' copies.
     */
    fun dismiss(peer: String, opaqueKey: String) {
        queue.trySend {
            val androidKey = lock.withLock { book.androidKeyFor(peer, opaqueKey) } ?: return@trySend
            val l = listener ?: return@trySend
            try {
                l.cancelNotification(androidKey)
            } catch (e: Exception) {
                HostLog.w("mirror", "could not dismiss a mirrored notification: ${e.javaClass.simpleName}")
            }
        }
    }

    /**
     * [peer] came online, or its `notificationsOut` grant was just turned on: when the grant
     * is on, send it the current notifications that pass the filter, at most
     * [MirrorFilter.RESYNC_LIMIT], so it shows the present set again.
     */
    fun resyncPeer(context: Context, peer: String) {
        val app = context.applicationContext
        queue.trySend {
            if (peer in eligiblePeers()) resync(app, peer)
        }
    }

    /**
     * Withdraws everything mirrored to [peer] (sends `removed` for each, best effort; the core
     * lets `removed` through without the grant) and forgets it there. Call after turning the
     * peer's `notificationsOut` grant off (SR-42).
     */
    fun withdrawFrom(peer: String) {
        queue.trySend {
            lock.withLock {
                for (key in book.dropPeer(peer)) sendRemoved(peer, key)
            }
        }
    }

    /** [packageName] left the allow list: withdraw its mirrored copies from every peer. */
    fun withdrawApp(packageName: String) {
        queue.trySend {
            lock.withLock {
                for (entry in book.removeApp(packageName)) withdraw(entry)
            }
        }
    }

    /**
     * [peer] went offline or was unpaired: the desktop closes its own copies (SR-42), so only
     * the book forgets it.
     */
    fun peerGone(peer: String) {
        queue.trySend { lock.withLock { book.dropPeer(peer) } }
    }

    /** Hex IDs of online peers with the `notificationsOut` grant on. */
    private fun eligiblePeers(): List<String> =
        CoreHost.peers.value.filter { it.online && it.grants.notificationsOut }.map { it.deviceId }

    /** Sends the newest active notifications that pass the filter to [peer]. */
    private suspend fun resync(app: Context, peer: String) {
        if (!CoreHost.running.value) return
        val l = listener ?: return
        val active = try {
            l.activeNotifications?.toList().orEmpty()
        } catch (e: Exception) {
            HostLog.w("mirror", "could not read active notifications: ${e.javaClass.simpleName}")
            return
        }
        val settings = Settings.read(app)
        val passing = active.filter { decide(app, it, settings.mirrorApps) != null }
        val picked = MirrorFilter.forResync(passing, { it.postTime })
        for (sbn in picked) mirror(app, sbn, listOf(peer), resend = true)
        if (picked.isNotEmpty()) HostLog.d("mirror", "sent ${picked.size} current notifications to a peer that came online")
    }

    /**
     * Sends [sbn] to [targets] when it passes the filter. One that no longer passes (now
     * ongoing, say) but was mirrored is withdrawn. Unless [resend], a peer that already shows
     * the same content is skipped, so an app that reposts often does not use up the
     * desktop's 30 `posted` frames a minute (SR-44).
     */
    private suspend fun mirror(app: Context, sbn: StatusBarNotification, targets: List<String>, resend: Boolean) {
        val androidKey = sbn.key
        val allowed = Settings.read(app).mirrorApps
        val out = decide(app, sbn, allowed)
        if (out == null) {
            lock.withLock { book.remove(androidKey)?.let { withdraw(it) } }
            return
        }
        val hash = out.contentHash()
        val whenMs = sbn.notification.`when`.takeIf { it > 0 } ?: sbn.postTime
        lock.withLock {
            val opaque = try {
                CoreHost.withHost { it.notificationKey(androidKey) }
            } catch (e: IllegalStateException) {
                return@withLock
            }
            val notification = MirroredNotification(opaque, out.app, out.title, out.text, whenMs.coerceAtLeast(0).toULong())
            val reached = HashSet<String>()
            val due = if (resend) targets else targets.filterNot { book.unchanged(androidKey, it, hash) }
            if (due.isEmpty()) return@withLock
            for (peer in due) {
                val sent = try {
                    CoreHost.withHost { it.sendNotification(peer, notification) }
                    true
                } catch (e: Exception) {
                    false
                }
                if (sent) reached.add(peer)
            }
            for (old in book.record(androidKey, opaque, sbn.packageName, reached, hash)) withdraw(old)
        }
    }

    private fun decide(app: Context, sbn: StatusBarNotification, allowed: Set<String>): MirrorFilter.Outgoing? {
        val candidate = candidateOf(sbn)
        // Cheap checks first: the label lookup is a binder call.
        if (candidate.packageName !in allowed) return null
        return MirrorFilter.decide(
            candidate = candidate,
            ownPackage = app.packageName,
            allowed = allowed,
            appLabel = labelOf(app, candidate.packageName),
        )
    }

    /** Sends `removed` for [entry] to every peer that showed it. */
    private suspend fun withdraw(entry: MirrorBook.Entry) {
        for (peer in entry.peers) sendRemoved(peer, entry.opaqueKey)
    }

    /** Sends `removed`; a failure (offline, core down) is dropped quietly. */
    private suspend fun sendRemoved(peer: String, opaqueKey: String) {
        try {
            CoreHost.withHost { it.sendNotificationRemoved(peer, opaqueKey) }
        } catch (e: Exception) {
            // Not sent: the desktop closes its copies itself on offline and unpair.
        }
    }

    /** The posting app's name from the package manager, or empty when it is not visible. */
    private fun labelOf(context: Context, packageName: String): String = try {
        val pm = context.packageManager
        pm.getApplicationLabel(pm.getApplicationInfo(packageName, 0)).toString()
    } catch (e: PackageManager.NameNotFoundException) {
        ""
    }

    /** Reads what [MirrorFilter] needs from [sbn]. */
    private fun candidateOf(sbn: StatusBarNotification): MirrorFilter.Candidate {
        val n = sbn.notification
        return MirrorFilter.Candidate(
            packageName = sbn.packageName,
            otherUser = sbn.user != Process.myUserHandle(),
            ongoing = sbn.isOngoing || n.flags and Notification.FLAG_ONGOING_EVENT != 0,
            foregroundService = n.flags and Notification.FLAG_FOREGROUND_SERVICE != 0,
            groupSummary = n.flags and Notification.FLAG_GROUP_SUMMARY != 0,
            visibility = n.visibility,
            title = titleOf(n),
            text = textOf(n),
        )
    }

    private fun titleOf(n: Notification): String? =
        (n.extras.getCharSequence(Notification.EXTRA_TITLE_BIG) ?: n.extras.getCharSequence(Notification.EXTRA_TITLE))?.toString()

    private fun textOf(n: Notification): String? =
        (n.extras.getCharSequence(Notification.EXTRA_TEXT) ?: n.extras.getCharSequence(Notification.EXTRA_BIG_TEXT))?.toString()
}
