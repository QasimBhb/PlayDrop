package org.playdrop.android.ui.common

import org.playdrop.android.core.OwnIdentity
import org.playdrop.android.core.PendingPairing
import org.playdrop.android.data.Settings
import org.playdrop.android.ui.adddevice.AddDeviceUiState
import org.playdrop.android.ui.device.DeviceDetailUiState
import org.playdrop.android.ui.devices.DeviceRow
import org.playdrop.android.ui.devices.DevicesUiState
import org.playdrop.android.ui.diagnostics.DiagnosticsSummary
import org.playdrop.android.ui.diagnostics.DiagnosticsUiState
import org.playdrop.android.ui.settings.PeerChoice
import org.playdrop.android.ui.settings.SettingsUiState
import org.playdrop.android.ui.transfers.OfferInfo
import org.playdrop.android.ui.transfers.TransferRows
import org.playdrop.core.AddDeviceCandidate
import org.playdrop.core.DeliveredFile
import org.playdrop.core.Diagnostics
import org.playdrop.core.DialFailure
import org.playdrop.core.DiscoveryPath
import org.playdrop.core.Grants
import org.playdrop.core.Ipv4Interface
import org.playdrop.core.MdnsStatus
import org.playdrop.core.MediaAction
import org.playdrop.core.MediaState
import org.playdrop.core.MulticastStatus
import org.playdrop.core.PairingPrompt
import org.playdrop.core.PeerDiagnostic
import org.playdrop.core.PeerSummary
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferSnapshot
import org.playdrop.core.TransferStatus

/** Sample data for Compose previews and unit tests. Nothing here touches the native core. */
object Samples {
    private const val NOW = 1_791_000_000L

    /** Grants as pairing sets them: everything but input. */
    val defaultGrants = Grants(files = true, clipboardIn = true, clipboardOut = true, input = false, media = true, find = true)

    /** A desktop music player, playing, one minute into a 3:35 track. */
    val song = MediaState(
        player = "spotify",
        name = "Spotify",
        playing = true,
        title = "Song",
        artist = "Artist",
        album = "Album",
        durationMs = 215_000uL,
        positionMs = 61_200uL,
        positionAtMs = (NOW * 1000).toULong(),
        volume = 70u.toUByte(),
        can = listOf(MediaAction.PLAY_PAUSE, MediaAction.NEXT, MediaAction.PREVIOUS, MediaAction.SEEK, MediaAction.VOLUME),
    )

    /** A paired laptop, online. */
    val laptop = PeerSummary(
        deviceId = "a1".repeat(32),
        shortId = "K3QD-7F2M-XA9P-41BC",
        label = "Pop laptop",
        online = true,
        grants = defaultGrants,
        lastKnown = listOf("192.168.1.20:42187", "[fe80::1]:42187"),
        pairedAt = (NOW - 86_400 * 20).toULong(),
        lastSeen = NOW.toULong(),
        removedByPeer = false,
        capabilities = listOf("input.pointer", "input.key", "input.presenter", "media.control"),
    )

    /** A paired desktop, offline for two hours. */
    val desktop = PeerSummary(
        deviceId = "b2".repeat(32),
        shortId = "M8TR-2KQW-9ZLC-0PDE",
        label = "Office PC",
        online = false,
        grants = defaultGrants.copy(files = false),
        lastKnown = listOf("10.0.0.5:42187"),
        pairedAt = (NOW - 86_400 * 3).toULong(),
        lastSeen = (NOW - 7_200).toULong(),
        removedByPeer = false,
        capabilities = emptyList(),
    )

    /** Device cards. */
    val deviceRows = listOf(
        DeviceRow(laptop.deviceId, laptop.label, laptop.shortId, true, false, 82, true, Ago.JustNow, true, false),
        DeviceRow(desktop.deviceId, desktop.label, desktop.shortId, false, false, null, false, Ago.Hours(2), false, false),
    )

    /** Devices tab with two devices. */
    val devicesState = DevicesUiState(running = true, rows = deviceRows, sendTarget = laptop.deviceId)

    /** Device detail of the laptop. */
    val detailState = DeviceDetailUiState(
        row = deviceRows[0],
        grants = defaultGrants,
        lastKnown = laptop.lastKnown,
        pairedAt = laptop.pairedAt.toLong(),
        ringing = false,
        capabilities = laptop.capabilities,
        media = listOf(song),
    )

    /** Add device with three adverts collapsing into two rows. */
    val addDeviceState = AddDeviceUiState(
        windowSecsLeft = 104,
        candidates = listOf(
            AddDeviceCandidate("Qasim's laptop", "laptop", "192.168.1.20:42187"),
            AddDeviceCandidate("Living room PC", "desktop", "192.168.1.31:42187"),
        ),
    )

    /** A SAS pairing prompt. */
    val sasPrompt = PendingPairing(
        peer = laptop.deviceId,
        shortId = laptop.shortId,
        name = "Pop laptop",
        prompt = PairingPrompt.Sas("482913"),
    )

    /** Transfer snapshots in every state. */
    val transfers = listOf(
        snapshot("t1", TransferDirection.SEND, TransferStatus.COMPLETE, 52_428_800, 52_428_800, 0, 12),
        snapshot(
            "t2",
            TransferDirection.RECEIVE,
            TransferStatus.COMPLETE,
            3_145_728,
            3_145_728,
            0,
            1,
            delivered = listOf(DeliveredFile("IMG_0042.jpg", "content://media/external/downloads/42")),
        ),
        snapshot("t3", TransferDirection.SEND, TransferStatus.ACTIVE, 314_572_800, 524_288_000, 62_914_560, 1),
        snapshot("t4", TransferDirection.RECEIVE, TransferStatus.OFFERED, 0, 8_388_608, 0, 5),
        snapshot("t5", TransferDirection.SEND, TransferStatus.FAILED, 1_000, 2_000, 0, 1, note = "connection lost"),
    )

    /** Transfer rows for the preview. */
    val transferRows = TransferRows.derive(
        transfers,
        listOf(laptop, desktop),
        mapOf("t4" to OfferInfo(listOf("a.pdf", "b.pdf", "c.pdf"), 5, 8_388_608, "From the meeting")),
    )

    /** Settings tab. */
    val settingsState = SettingsUiState(
        values = Settings.Values(
            deviceName = "Pixel 9",
            defaultPeer = laptop.deviceId,
            port = 42187,
            autoClipboard = true,
            onboarded = true,
            batteryPromptShown = false,
        ),
        peers = listOf(PeerChoice(laptop.deviceId, laptop.label, laptop.shortId), PeerChoice(desktop.deviceId, desktop.label, desktop.shortId)),
        identity = OwnIdentity(deviceId = "c3".repeat(32), shortId = "Z7QA-3MNE-8KFT-2WXY", addr = "0.0.0.0:42187"),
        coreVersion = "0.1.0",
    )

    /** A diagnose() snapshot with one unreachable peer. */
    val diagnostics = Diagnostics(
        addr = "0.0.0.0:42187",
        deviceId = "c3".repeat(32),
        shortId = "Z7QA-3MNE-8KFT-2WXY",
        port = 42187u,
        bindOk = true,
        interfaces = listOf(Ipv4Interface(3u, "192.168.1.40", "255.255.255.0")),
        multicast = MulticastStatus(true, listOf("192.168.1.40"), emptyList(), true, 4u),
        mdns = MdnsStatus(enabled = true, registered = true, error = null, peerSeen = true, lastSeenSecsAgo = 9u),
        peers = listOf(
            PeerDiagnostic(laptop.deviceId, laptop.shortId, laptop.label, true, laptop.lastKnown, true, true, null),
            PeerDiagnostic(
                desktop.deviceId,
                desktop.shortId,
                desktop.label,
                false,
                listOf("192.168.1.31:42187"),
                false,
                false,
                DialFailure(DiscoveryPath.LAST_KNOWN, "connection refused", NOW.toULong()),
            ),
        ),
    )

    /** Diagnostics screen state. */
    val diagnosticsState = DiagnosticsUiState(report = diagnostics, summary = DiagnosticsSummary.of(diagnostics))

    /** One snapshot with defaults for the fields tests rarely care about. */
    fun snapshot(
        id: String,
        direction: TransferDirection,
        state: TransferStatus,
        bytes: Long,
        total: Long,
        rate: Long,
        files: Long,
        peer: String = laptop.deviceId,
        note: String? = null,
        delivered: List<DeliveredFile> = emptyList(),
    ) = TransferSnapshot(
        id = id,
        peer = peer,
        shortId = laptop.shortId,
        direction = direction,
        state = state,
        bytes = bytes.toULong(),
        total = total.toULong(),
        bytesPerSec = rate.toULong(),
        startedAt = NOW.toULong(),
        fileCount = files.toULong(),
        note = note,
        delivered = delivered,
    )
}
