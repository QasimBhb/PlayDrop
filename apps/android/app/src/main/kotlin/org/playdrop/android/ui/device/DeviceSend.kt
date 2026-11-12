package org.playdrop.android.ui.device

import org.playdrop.android.R
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.devices.DeviceRow
import org.playdrop.core.Grants

/** What the phone's clipboard held when the user tapped "Send clipboard". */
sealed interface ClipRead {
    /** No clip, or one with no usable item. */
    data object Empty : ClipRead

    /**
     * A copied image: its `content://` location (as text, so the rules stay free of Android
     * types) and type, read only once the send goes ahead. [sensitive] as for text.
     */
    data class Image(val uri: String, val mime: String, val sensitive: Boolean) : ClipRead

    /** Copied text; [sensitive] when the app that copied it marked it so (SR-24). */
    data class Text(val text: String, val sensitive: Boolean) : ClipRead
}

/** A clip on its way to this one device. Never logged (SR-32). */
sealed interface DeviceClip {
    /** Text. */
    data class Text(val text: String) : DeviceClip

    /** An image still in the clipboard, read when it is sent. */
    data class Image(val uri: String, val mime: String) : DeviceClip
}

/** What to do with a [ClipRead]. */
sealed interface ClipboardPlan {
    /** Send [clip] now. */
    data class Send(val clip: DeviceClip) : ClipboardPlan

    /** Ask the user first, then send [clip] marked sensitive. */
    data class Confirm(val clip: DeviceClip) : ClipboardPlan

    /** Do not send; show [message]. */
    data class Tell(val message: UiMessage) : ClipboardPlan
}

/** What the core's push to this one device reported, reduced to what the page says. */
enum class ClipDelivery {
    /** It went. */
    SENT,

    /** The echo rule kept it back: the same clip was the last one sent or received. */
    UNCHANGED,

    /** The device was offline, even after dialling it. */
    OFFLINE,

    /** The device's "Receive this phone's clipboard" switch is off. */
    NO_GRANT,

    /** The device cannot take copied images. */
    NO_IMAGES,

    /** The device is no longer paired. */
    NOT_PAIRED,
}

/** The confirmation card shown after a pick: [count] files on their way to [label]. */
data class SendNotice(val count: Int, val label: String, val offline: Boolean)

/** Pure rules of the send actions and the permissions summary, so they are unit-tested. */
object DeviceSend {
    /** The switches counted in the "Permissions" row summary, in screen order. */
    val PERMISSION_KINDS = listOf(
        GrantKind.FILES,
        GrantKind.CLIPBOARD_IN,
        GrantKind.CLIPBOARD_OUT,
        GrantKind.MEDIA,
        GrantKind.FIND,
        GrantKind.NOTIFICATIONS_OUT,
    )

    /** File and photo sends need a device that has not removed this phone. */
    fun canSendFiles(row: DeviceRow?): Boolean = row != null && !row.removedByPeer

    /** Clipboard send also waits while an earlier one is still being delivered. */
    fun canSendClipboard(row: DeviceRow?, busy: Boolean): Boolean = canSendFiles(row) && !busy

    /**
     * Decides what happens to [read], following the notification action's rules plus the
     * grant. Text and images both go to this one device; a clip marked sensitive asks first.
     */
    fun planClipboard(read: ClipRead, clipboardOutAllowed: Boolean): ClipboardPlan {
        // The grant first: with it off the page does not even read the clipboard.
        if (!clipboardOutAllowed) return ClipboardPlan.Tell(UiMessage(R.string.ui_send_clip_needs_grant))
        val (clip, sensitive) = when (read) {
            ClipRead.Empty -> return ClipboardPlan.Tell(UiMessage(R.string.toast_nothing_to_send))
            is ClipRead.Text -> DeviceClip.Text(read.text) to read.sensitive
            is ClipRead.Image -> DeviceClip.Image(read.uri, read.mime) to read.sensitive
        }
        return if (sensitive) ClipboardPlan.Confirm(clip) else ClipboardPlan.Send(clip)
    }

    /**
     * Reads the core's answer to a push limited to [peer]: the lists and counters of
     * `ClipboardPush`, passed as plain values so this stays unit-tested.
     */
    fun delivery(
        peer: String,
        sentTo: List<String>,
        skippedEcho: Boolean,
        skippedNoGrant: Int,
        skippedUnsupported: Int,
        skippedNotPaired: List<String>,
    ): ClipDelivery = when {
        peer in sentTo -> ClipDelivery.SENT
        peer in skippedNotPaired -> ClipDelivery.NOT_PAIRED
        skippedEcho -> ClipDelivery.UNCHANGED
        skippedNoGrant > 0 -> ClipDelivery.NO_GRANT
        skippedUnsupported > 0 -> ClipDelivery.NO_IMAGES
        else -> ClipDelivery.OFFLINE
    }

    /** The line the page shows for [delivery] to the device called [label]. */
    fun deliveryMessage(delivery: ClipDelivery, label: String): UiMessage = when (delivery) {
        ClipDelivery.SENT -> UiMessage(R.string.toast_copied_to, listOf(label))
        ClipDelivery.UNCHANGED -> UiMessage(R.string.toast_clipboard_unchanged)
        ClipDelivery.OFFLINE -> UiMessage(R.string.toast_peer_offline, listOf(label))
        ClipDelivery.NO_GRANT -> UiMessage(R.string.ui_send_clip_needs_grant)
        ClipDelivery.NO_IMAGES -> UiMessage(R.string.ui_send_clip_no_images, listOf(label))
        ClipDelivery.NOT_PAIRED -> UiMessage(R.string.ui_send_removed)
    }

    /** "Sending 3 files to calliope". */
    fun startedMessage(count: Int, label: String): UiMessage =
        UiMessage(R.plurals.ui_send_started, listOf(count, label), quantity = count)

    /** The one-line summary under "Permissions": how many switches are on. */
    fun grantSummary(grants: Grants): UiMessage {
        val on = PERMISSION_KINDS.count { GrantEdit.get(grants, it) }
        return if (on == 0) {
            UiMessage(R.string.ui_permissions_none)
        } else {
            UiMessage(R.string.ui_permissions_some, listOf(on, PERMISSION_KINDS.size))
        }
    }
}
