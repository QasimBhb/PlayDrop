package org.playdrop.android.share

import org.junit.Assert.assertEquals
import org.junit.Test

/** Device names in plain English and the one-line result of a send to several devices. */
class SendReportTest {
    /** The English wording of strings_share.xml, for clips. */
    private val english = EnglishPhrases()

    @Test
    fun joinsNames() {
        assertEquals("", NameList.join(emptyList(), "and"))
        assertEquals("A", NameList.join(listOf("A"), "and"))
        assertEquals("A and B", NameList.join(listOf("A", "B"), "and"))
        assertEquals("A, B and C", NameList.join(listOf("A", "B", "C"), "and"))
        assertEquals("A, B, C and D", NameList.join(listOf("A", "B", "C", "D"), "and"))
    }

    @Test
    fun allSent() {
        val r = listOf(DeviceResult.Sent("calliope"), DeviceResult.Sent("Pixel 9 Pro XL"))
        assertEquals("Sent to calliope and Pixel 9 Pro XL.", SendReport.sentence(r, english))
    }

    @Test
    fun someSentSomeOffline() {
        val r = listOf(DeviceResult.Sent("A"), DeviceResult.Offline("B"))
        assertEquals("Sent to A. B is offline.", SendReport.sentence(r, english))
    }

    @Test
    fun mixedOutcomesInAFixedOrder() {
        val r = listOf(
            DeviceResult.Failed("E", "no space"),
            DeviceResult.Offline("C"),
            DeviceResult.Sent("A"),
            DeviceResult.ClipboardOff("D"),
            DeviceResult.Offline("B"),
            DeviceResult.Sent("F"),
        )
        assertEquals(
            "Sent to A and F. C and B are offline. Clipboard to D is off. Could not send to E: no space",
            SendReport.sentence(r, english),
        )
    }

    @Test
    fun filesToAnOfflineDeviceKeepTrying() {
        val r = listOf(DeviceResult.Sent("A"), DeviceResult.Waiting("B"), DeviceResult.Waiting("C"))
        assertEquals("Sent to A. B and C are offline. PlayDrop keeps trying for 10 minutes.", SendReport.sentence(r, english))
    }

    @Test
    fun echoImagesAndNotPaired() {
        assertEquals("Already sent to A.", SendReport.sentence(listOf(DeviceResult.AlreadySent("A")), english))
        assertEquals("B cannot take copied images.", SendReport.sentence(listOf(DeviceResult.NoImages("B")), english))
        assertEquals("A and B are no longer paired.", SendReport.sentence(listOf(DeviceResult.NotPaired("A"), DeviceResult.NotPaired("B")), english))
        assertEquals(
            "Sent to A. B did not take it.",
            SendReport.sentence(listOf(DeviceResult.Sent("A"), DeviceResult.NotTaken("B")), english),
        )
    }
}
