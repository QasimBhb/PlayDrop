package org.playdrop.android.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.playdrop.android.R
import org.playdrop.android.core.PeerStatus
import org.playdrop.android.ui.clipboard.ClipboardEcho
import org.playdrop.android.ui.common.Ago
import org.playdrop.android.ui.common.Samples
import org.playdrop.android.ui.common.UiMessage
import org.playdrop.android.ui.device.GrantEdit
import org.playdrop.android.ui.device.GrantKind
import org.playdrop.android.ui.devices.DeviceRows
import org.playdrop.android.ui.diagnostics.DiagIssue
import org.playdrop.android.ui.diagnostics.DiagnosticsSummary
import org.playdrop.android.ui.settings.PortRules
import org.playdrop.core.DiscoveryPath
import org.playdrop.core.PlayDropException

/** Device rows, grants, ports, clipboard echo, diagnostics summary and error mapping. */
class LogicTest {
    @Test
    fun deviceRowsPutTheDefaultFirstAndReadPresence() {
        val status = mapOf(Samples.laptop.deviceId to PeerStatus(batteryLevel = 40, presence = "asleep"))
        val rows = DeviceRows.derive(listOf(Samples.desktop, Samples.laptop), status, Samples.desktop.deviceId, 1_791_000_000)
        assertEquals(listOf("Office PC", "Pop laptop"), rows.map { it.label })
        assertTrue(rows[0].isDefault)
        assertEquals(Ago.Hours(2), rows[0].lastSeen)
        assertTrue(rows[1].asleep)
        assertEquals(40, rows[1].batteryLevel)
    }

    @Test
    fun sendTargetIsTheDefaultOrTheOnlyDevice() {
        val both = listOf(Samples.laptop, Samples.desktop)
        assertEquals(Samples.desktop.deviceId, DeviceRows.sendTarget(both, Samples.desktop.deviceId))
        assertNull(DeviceRows.sendTarget(both, null))
        assertNull(DeviceRows.sendTarget(both, "gone"))
        assertEquals(Samples.laptop.deviceId, DeviceRows.sendTarget(listOf(Samples.laptop), "gone"))
    }

    @Test
    fun grantEditChangesOneGrant() {
        val g = GrantEdit.with(Samples.defaultGrants, GrantKind.CLIPBOARD_OUT, false)
        assertFalse(GrantEdit.get(g, GrantKind.CLIPBOARD_OUT))
        assertTrue(GrantEdit.get(g, GrantKind.CLIPBOARD_IN))
        assertEquals(Samples.defaultGrants.copy(clipboardOut = false), g)
    }

    @Test
    fun portRulesKeepOffOtherAppsPorts() {
        assertEquals(PortRules.Result.Ok(42187), PortRules.check("42187"))
        assertEquals(PortRules.Result.Reserved, PortRules.check("1716"))
        assertEquals(PortRules.Result.Reserved, PortRules.check("53317"))
        assertEquals(PortRules.Result.Invalid, PortRules.check("80"))
        assertEquals(PortRules.Result.Invalid, PortRules.check("70000"))
        assertEquals(PortRules.Result.Invalid, PortRules.check("abc"))
    }

    @Test
    fun clipboardEchoRule() {
        val echo = ClipboardEcho()
        assertEquals(ClipboardEcho.Decision.SKIP, echo.decide(null, false))
        assertEquals(ClipboardEcho.Decision.SKIP, echo.decide("", false))
        assertEquals(ClipboardEcho.Decision.PUSH, echo.decide("hello", false))
        echo.remember("hello")
        assertEquals(ClipboardEcho.Decision.SKIP, echo.decide("hello", false))
        assertEquals(ClipboardEcho.Decision.SENSITIVE, echo.decide("hunter2", true))
        assertEquals(ClipboardEcho.Decision.TOO_LARGE, echo.decide("x".repeat(ClipboardEcho.MAX_BYTES + 1), false))
        assertEquals(ClipboardEcho.Decision.PUSH, echo.decide("x".repeat(ClipboardEcho.MAX_BYTES), false))
    }

    @Test
    fun diagnosticsSummaryNamesTheFailedPath() {
        val issues = DiagnosticsSummary.of(Samples.diagnostics)
        assertEquals(
            listOf(DiagIssue.PeerNotReached("Office PC", DiscoveryPath.LAST_KNOWN, "connection refused")),
            issues,
        )
    }

    @Test
    fun diagnosticsSummaryAllFine() {
        val d = Samples.diagnostics.copy(peers = Samples.diagnostics.peers.take(1))
        assertEquals(listOf(DiagIssue.AllFine), DiagnosticsSummary.of(d))
    }

    @Test
    fun diagnosticsSummaryOrder() {
        val d = Samples.diagnostics.copy(
            bindOk = false,
            interfaces = emptyList(),
            multicast = Samples.diagnostics.multicast.copy(joinFailed = listOf("192.168.1.40: no route")),
            mdns = Samples.diagnostics.mdns.copy(error = "socket closed"),
        )
        val issues = DiagnosticsSummary.of(d)
        assertEquals(DiagIssue.NotListening(42187), issues[0])
        assertEquals(DiagIssue.NoInterface, issues[1])
        assertEquals(DiagIssue.MulticastJoinFailed("192.168.1.40", "no route"), issues[2])
        assertEquals(DiagIssue.MdnsFailed("socket closed"), issues[3])
    }

    @Test
    fun multicastBlockedWhenAPeerIsOnlineWithoutBeacon() {
        val base = Samples.diagnostics
        val d = base.copy(
            multicast = base.multicast.copy(beaconRecently = false),
            peers = listOf(base.peers[0].copy(beaconSeen = false, lastKnown = listOf("192.168.1.20:42187"))),
        )
        assertEquals(listOf(DiagIssue.MulticastBlocked("192.168.1.40", "Pop laptop")), DiagnosticsSummary.of(d))
    }

    @Test
    fun errorsBecomeMessages() {
        assertEquals(UiMessage(R.string.ui_err_not_paired), UiMessage.from(PlayDropException.NotPaired("x")))
        assertEquals(UiMessage(R.string.ui_err_port_held, listOf(42187)), UiMessage.from(PlayDropException.PortHeld(42187u)))
        assertEquals(UiMessage(R.string.ui_err_io, listOf("reset")), UiMessage.from(PlayDropException.Io("reset")))
        assertEquals(UiMessage(R.string.ui_err_not_running), UiMessage.from(IllegalStateException("down")))
    }
}
