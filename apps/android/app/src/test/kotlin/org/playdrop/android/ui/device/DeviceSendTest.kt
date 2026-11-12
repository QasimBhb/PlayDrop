package org.playdrop.android.ui.device

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.UiMessage

/** Send button rules, the clipboard plan, the confirmation text and the Permissions summary. */
class DeviceSendTest {
    private val row = Samples.deviceRows[0]
    private val grants = Samples.defaultGrants

    @Test
    fun filesNeedADeviceThatHasNotRemovedThePhone() {
        assertTrue(DeviceSend.canSendFiles(row))
        assertTrue(DeviceSend.canSendFiles(Samples.deviceRows[1]))
        assertFalse(DeviceSend.canSendFiles(row.copy(removedByPeer = true)))
        assertFalse(DeviceSend.canSendFiles(null))
    }

    @Test
    fun clipboardWaitsForAnEarlierSend() {
        assertTrue(DeviceSend.canSendClipboard(row, busy = false))
        assertFalse(DeviceSend.canSendClipboard(row, busy = true))
        assertFalse(DeviceSend.canSendClipboard(row.copy(removedByPeer = true), busy = false))
    }

    @Test
    fun plainTextIsSentAtOnce() {
        assertEquals(ClipboardPlan.Send(DeviceClip.Text("hello")), DeviceSend.planClipboard(ClipRead.Text("hello", false), true))
    }

    @Test
    fun sensitiveTextAsksFirst() {
        assertEquals(ClipboardPlan.Confirm(DeviceClip.Text("pw")), DeviceSend.planClipboard(ClipRead.Text("pw", true), true))
    }

    @Test
    fun emptyClipboardSaysSo() {
        assertEquals(
            ClipboardPlan.Tell(UiMessage(R.string.toast_nothing_to_send)),
            DeviceSend.planClipboard(ClipRead.Empty, true),
        )
    }

    @Test
    fun imagesGoToThisOneDevice() {
        val uri = "content://clip/1"
        assertEquals(
            ClipboardPlan.Send(DeviceClip.Image(uri, "image/png")),
            DeviceSend.planClipboard(ClipRead.Image(uri, "image/png", sensitive = false), true),
        )
        assertEquals(
            ClipboardPlan.Confirm(DeviceClip.Image(uri, "image/jpeg")),
            DeviceSend.planClipboard(ClipRead.Image(uri, "image/jpeg", sensitive = true), true),
        )
        assertEquals(
            ClipboardPlan.Tell(UiMessage(R.string.ui_send_clip_needs_grant)),
            DeviceSend.planClipboard(ClipRead.Image(uri, "image/png", sensitive = false), false),
        )
    }

    @Test
    fun theCoresAnswerBecomesOneLine() {
        fun d(sent: List<String> = emptyList(), echo: Boolean = false, noGrant: Int = 0, unsupported: Int = 0, notPaired: List<String> = emptyList()) =
            DeviceSend.delivery("aa", sent, echo, noGrant, unsupported, notPaired)
        assertEquals(ClipDelivery.SENT, d(sent = listOf("aa")))
        assertEquals(ClipDelivery.UNCHANGED, d(echo = true))
        assertEquals(ClipDelivery.NO_GRANT, d(noGrant = 1))
        assertEquals(ClipDelivery.NO_IMAGES, d(unsupported = 1))
        assertEquals(ClipDelivery.NOT_PAIRED, d(notPaired = listOf("aa")))
        assertEquals(ClipDelivery.OFFLINE, d())
        assertEquals(UiMessage(R.string.toast_copied_to, listOf("calliope")), DeviceSend.deliveryMessage(ClipDelivery.SENT, "calliope"))
        assertEquals(UiMessage(R.string.ui_send_clip_no_images, listOf("calliope")), DeviceSend.deliveryMessage(ClipDelivery.NO_IMAGES, "calliope"))
        assertEquals(UiMessage(R.string.toast_peer_offline, listOf("calliope")), DeviceSend.deliveryMessage(ClipDelivery.OFFLINE, "calliope"))
    }

    @Test
    fun withoutTheGrantNothingIsSentAndNothingIsAsked() {
        val needs = ClipboardPlan.Tell(UiMessage(R.string.ui_send_clip_needs_grant))
        assertEquals(needs, DeviceSend.planClipboard(ClipRead.Text("hello", false), false))
        assertEquals(needs, DeviceSend.planClipboard(ClipRead.Text("pw", true), false))
        // The page does not read the clipboard with the grant off; the answer is still the grant.
        assertEquals(needs, DeviceSend.planClipboard(ClipRead.Empty, false))
    }

    @Test
    fun confirmationNamesCountAndDevice() {
        val m = DeviceSend.startedMessage(3, "calliope")
        assertEquals(R.plurals.ui_send_started, m.res)
        assertEquals(3, m.quantity)
        assertEquals(listOf<Any>(3, "calliope"), m.args)
    }

    @Test
    fun summaryCountsTheSwitchesThatAreOn() {
        // Defaults: files, both clipboard directions, media and find are on; mirroring is off.
        assertEquals(UiMessage(R.string.ui_permissions_some, listOf(5, 6)), DeviceSend.grantSummary(grants))
        val all = grants.copy(notificationsOut = true)
        assertEquals(UiMessage(R.string.ui_permissions_some, listOf(6, 6)), DeviceSend.grantSummary(all))
    }

    @Test
    fun summaryWhenNothingIsAllowed() {
        val none = grants.copy(files = false, clipboardIn = false, clipboardOut = false, media = false, find = false)
        assertEquals(UiMessage(R.string.ui_permissions_none), DeviceSend.grantSummary(none))
    }

    @Test
    fun summaryIgnoresRemoteInput() {
        // INPUT is never offered on a phone, so it must not change the count.
        assertEquals(DeviceSend.grantSummary(grants), DeviceSend.grantSummary(grants.copy(input = true)))
    }
}
