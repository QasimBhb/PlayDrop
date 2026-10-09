package org.playdrop.android.ui.devices

import org.playdrop.android.core.PeerStatus
import org.playdrop.android.ui.common.Ago
import org.playdrop.core.PeerSummary

/** A paired device as a card shows it. */
data class DeviceRow(
    /** Hex Device ID. */
    val deviceId: String,
    /** Local label. */
    val label: String,
    /** Short ID, always shown next to the label (SR-8). */
    val shortId: String,
    /** Session up. */
    val online: Boolean,
    /** Online but reported `asleep`. */
    val asleep: Boolean,
    /** Last battery percent the device reported, if any. */
    val batteryLevel: Int?,
    /** Charging at the last report. */
    val charging: Boolean,
    /** When it was last heard from; shown while offline. */
    val lastSeen: Ago,
    /** The one-tap send target. */
    val isDefault: Boolean,
    /** The other side unpaired us; pairing again is needed. */
    val removedByPeer: Boolean,
)

/** Turns the core's peers into card rows: default first, then online ones, then by label. */
object DeviceRows {
    /** Builds the rows; [now] is Unix seconds. */
    fun derive(
        peers: List<PeerSummary>,
        status: Map<String, PeerStatus>,
        defaultPeer: String?,
        now: Long,
    ): List<DeviceRow> = peers.map { p ->
        val s = status[p.deviceId]
        DeviceRow(
            deviceId = p.deviceId,
            label = p.label,
            shortId = p.shortId,
            online = p.online,
            asleep = p.online && s?.presence == "asleep",
            batteryLevel = s?.batteryLevel,
            charging = s?.charging ?: false,
            lastSeen = Ago.of(p.lastSeen?.toLong(), now),
            isDefault = p.deviceId == defaultPeer,
            removedByPeer = p.removedByPeer,
        )
    }.sortedWith(
        compareByDescending<DeviceRow> { it.isDefault }
            .thenByDescending { it.online }
            .thenBy { it.label.lowercase() },
    )

    /**
     * Where a one-tap send goes: the default device if it is still paired, otherwise the
     * only paired device, otherwise null (the screen asks).
     */
    fun sendTarget(peers: List<PeerSummary>, defaultPeer: String?): String? =
        peers.firstOrNull { it.deviceId == defaultPeer }?.deviceId
            ?: peers.singleOrNull()?.deviceId
}
