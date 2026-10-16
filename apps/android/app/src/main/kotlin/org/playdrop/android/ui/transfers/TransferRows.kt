package org.playdrop.android.ui.transfers

import org.playdrop.android.ui.common.Format
import org.playdrop.core.PeerSummary
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferSnapshot
import org.playdrop.core.TransferStatus

/** What an incoming offer listed, kept from its `TransferOffer` event. */
data class OfferInfo(
    /** First offered paths. */
    val names: List<String>,
    /** Files in the offer. */
    val fileCount: Long,
    /** Bytes in the offer. */
    val totalBytes: Long,
    /** Sender's note. */
    val note: String?,
)

/** A transfer as a row shows it. */
data class TransferRow(
    /** Transfer ID. */
    val id: String,
    /** True for an incoming transfer. */
    val receive: Boolean,
    /** Label of the other device, or its Short ID when it is no longer paired. */
    val peerLabel: String,
    /** Where it stands. */
    val state: TransferStatus,
    /** Bytes moved. */
    val bytes: Long,
    /** Total bytes. */
    val total: Long,
    /** Recent rate, bytes per second. */
    val bytesPerSec: Long,
    /** Progress 0..1. */
    val fraction: Float,
    /** Seconds left at the current rate, while active. */
    val etaSecs: Long?,
    /** Files in the transfer. */
    val fileCount: Long,
    /** Files finished so far (sent whole, or verified and placed); 0 when the core does not say. */
    val filesDone: Long,
    /** The row reads "N of M files" next to bytes and rate, see [Format.showFilesDone]. */
    val showFilesDone: Boolean,
    /** First file names of an offer, with the count of the rest. */
    val offeredNames: List<String>,
    /** Files not listed in [offeredNames]. */
    val moreNames: Long,
    /** Sender's note or failure reason. */
    val note: String?,
    /** Cancel is offered. */
    val canCancel: Boolean,
    /** Accept and Decline are offered. */
    val canRespond: Boolean,
    /** Content URI of the first delivered file, for "Open". */
    val openUri: String?,
    /** Relative path of that file, to judge whether opening it is safe (SR-23). */
    val openName: String?,
    /** Several files were delivered: offer "Open folder". */
    val showFolder: Boolean,
)

/** Derives transfer rows from snapshots; plain logic for JVM tests. */
object TransferRows {
    /** How many offered names a row lists before "and N more". */
    const val NAMES_SHOWN = 3

    /** Rows newest first. Offers waiting for an answer come before everything else. */
    fun derive(
        snapshots: List<TransferSnapshot>,
        peers: List<PeerSummary>,
        offers: Map<String, OfferInfo>,
        relocate: (transferId: String, location: String) -> String = { _, location -> location },
    ): List<TransferRow> {
        val labels = peers.associate { it.deviceId to it.label }
        return snapshots.map { s -> row(s, labels[s.peer] ?: s.shortId, offers[s.id], relocate) }
            .withIndex()
            .sortedWith(
                compareByDescending<IndexedValue<TransferRow>> { it.value.canRespond }
                    .thenByDescending { it.index },
            )
            .map { it.value }
    }

    /** Count of incoming offers waiting for the user, for the navigation badge. */
    fun pendingOffers(snapshots: List<TransferSnapshot>): Int =
        snapshots.count { it.direction == TransferDirection.RECEIVE && it.state == TransferStatus.OFFERED }

    /**
     * [relocate] maps a stored location to where the file really is: the mover may have given
     * a landed file another name than the one promised to the core (see `Relocations`).
     */
    private fun row(
        s: TransferSnapshot,
        label: String,
        offer: OfferInfo?,
        relocate: (String, String) -> String,
    ): TransferRow {
        val receive = s.direction == TransferDirection.RECEIVE
        val bytes = s.bytes.toLong()
        val total = s.total.toLong().takeIf { it > 0 } ?: offer?.totalBytes ?: 0L
        val rate = s.bytesPerSec.toLong()
        val active = s.state == TransferStatus.ACTIVE
        val finished = s.state == TransferStatus.COMPLETE ||
            s.state == TransferStatus.FAILED ||
            s.state == TransferStatus.CANCELLED
        val offered = s.state == TransferStatus.OFFERED
        val fileCount = s.fileCount.toLong().takeIf { it > 0 } ?: offer?.fileCount ?: 0L
        val filesDone = s.filesDone.toLong()
        val names = offer?.names?.take(NAMES_SHOWN).orEmpty()
        val delivered = if (receive && s.state == TransferStatus.COMPLETE) s.delivered else emptyList()
        return TransferRow(
            id = s.id,
            receive = receive,
            peerLabel = label,
            state = s.state,
            bytes = bytes,
            total = total,
            bytesPerSec = rate,
            fraction = Format.fraction(bytes, total),
            etaSecs = if (active) Format.etaSeconds(total - bytes, rate) else null,
            fileCount = fileCount,
            filesDone = filesDone,
            showFilesDone = active && Format.showFilesDone(filesDone, fileCount),
            offeredNames = names,
            moreNames = (fileCount - names.size).coerceAtLeast(0).takeIf { names.isNotEmpty() } ?: 0,
            note = s.note ?: offer?.note,
            canCancel = !finished && !(receive && offered),
            canRespond = receive && offered,
            openUri = delivered.firstOrNull()?.let { relocate(s.id, it.location) },
            openName = delivered.firstOrNull()?.path,
            showFolder = delivered.size > 1,
        )
    }
}
