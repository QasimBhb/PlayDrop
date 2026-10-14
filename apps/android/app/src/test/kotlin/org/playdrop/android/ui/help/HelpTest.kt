package org.playdrop.android.ui.help

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.service.TetherWatcher
import org.playdrop.android.ui.diagnostics.DiagIssue
import org.playdrop.android.ui.diagnostics.DiagnosticsSummary
import org.playdrop.core.DiscoveryPath

class HelpTest {
    @Test
    fun otherNetworkPointsToHotspot() {
        val issues = listOf(DiagIssue.OtherNetwork("Office PC"))
        assertEquals(HelpTopic.OTHER_NETWORK, DiagnosticsSummary.helpTopic(issues))
    }

    @Test
    fun blockedMulticastAndDiscoveryOffPointToQrOrAddress() {
        assertEquals(
            HelpTopic.SAME_NETWORK,
            DiagnosticsSummary.helpTopic(listOf(DiagIssue.MulticastBlocked("192.168.1.40", "Office PC"))),
        )
        assertEquals(HelpTopic.SAME_NETWORK, DiagnosticsSummary.helpTopic(listOf(DiagIssue.DiscoveryOff)))
    }

    @Test
    fun otherNetworkWinsWhenBothAppear() {
        val issues = listOf(DiagIssue.DiscoveryOff, DiagIssue.OtherNetwork("Office PC"))
        assertEquals(HelpTopic.OTHER_NETWORK, DiagnosticsSummary.helpTopic(issues))
    }

    @Test
    fun ambiguousFindingsSuggestHelpButNoTopic() {
        val failed = listOf(DiagIssue.PeerNotReached("Office PC", DiscoveryPath.LAST_KNOWN, "connection refused"))
        assertNull(DiagnosticsSummary.helpTopic(failed))
        assertTrue(DiagnosticsSummary.suggestsHelp(failed))
    }

    @Test
    fun localFaultsAndAllFineSuggestNothing() {
        assertFalse(DiagnosticsSummary.suggestsHelp(listOf(DiagIssue.AllFine)))
        assertFalse(DiagnosticsSummary.suggestsHelp(listOf(DiagIssue.NoInterface)))
        assertFalse(DiagnosticsSummary.suggestsHelp(listOf(DiagIssue.NotListening(42187))))
    }

    @Test
    fun topicRouteArgumentsRoundTrip() {
        HelpTopic.entries.forEach { assertEquals(it, HelpTopic.of(it.arg)) }
        assertNull(HelpTopic.of(null))
        assertNull(HelpTopic.of("bogus"))
    }

    @Test
    fun tetherWatcherHandlesHotspotAndUsbBroadcastsOnly() {
        assertEquals("hotspot state", TetherWatcher.reasonFor("android.net.wifi.WIFI_AP_STATE_CHANGED"))
        assertEquals("tether state", TetherWatcher.reasonFor("android.net.conn.TETHER_STATE_CHANGED"))
        assertEquals("usb state", TetherWatcher.reasonFor("android.hardware.usb.action.USB_STATE"))
        assertNull(TetherWatcher.reasonFor("android.intent.action.SCREEN_ON"))
        assertNull(TetherWatcher.reasonFor(null))
    }
}
