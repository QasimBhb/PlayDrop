package org.playdrop.android.ui.device

import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import org.playdrop.core.HostEvent
import org.playdrop.core.TransferDirection
import org.playdrop.core.TransferFailure

/** Where a send from the device page stands. */
sealed interface SendPhase {
    /** Still going (or waiting for the device to come back). */
    data object Sending : SendPhase

    /** Every file arrived. */
    data object Sent : SendPhase

    /** It stopped; [code] and [detail] (the core's cleaned sentence for `OTHER`) say why. */
    data class Failed(val code: TransferFailure, val detail: String?) : SendPhase

    /** It stopped but PlayDrop keeps it and carries on when the device is back; same as the history's "resumable". */
    data class FailedResumable(val code: TransferFailure, val detail: String?) : SendPhase

    /** The person cancelled it. */
    data object Cancelled : SendPhase
}

/**
 * The card shown after a pick: [count] files for [label], sent as transfer [transferId], at
 * [phase]. [offline] adds the "keeps trying" line while it is still sending.
 */
data class SendNotice(
    val count: Int,
    val label: String,
    val offline: Boolean,
    val transferId: String = "",
    val phase: SendPhase = SendPhase.Sending,
)

/**
 * Pure state of the device page's send cards, one per device, kept in the process (not in a
 * screen) so an outcome that arrives while the page is closed still shows when it opens.
 * A card follows its transfer: a later end replaces an earlier one (a failed send that
 * resumes and completes turns to "sent"). An end that comes before its card was shown is
 * remembered (the newest [MAX_ENDED]) and applied when the card begins.
 */
data class SendNoticeBook(
    /** The card of each device, by hex Device ID. */
    val notices: Map<String, SendNotice> = emptyMap(),
    /** Recent ends by transfer ID, oldest first. */
    val ended: Map<String, SendPhase> = emptyMap(),
) {
    /** Shows [notice] for [peer], already at its outcome when that was seen first. */
    fun begin(peer: String, notice: SendNotice): SendNoticeBook {
        val phase = ended[notice.transferId] ?: notice.phase
        return copy(notices = notices + (peer to notice.copy(phase = phase)))
    }

    /** Transfer [id] reached [phase]: its card, if any, follows. */
    fun end(id: String, phase: SendPhase): SendNoticeBook {
        val kept = (ended - id) + (id to phase)
        val bounded = if (kept.size > MAX_ENDED) kept.entries.drop(kept.size - MAX_ENDED).associate { it.toPair() } else kept
        return copy(
            notices = notices.mapValues { (_, n) -> if (n.transferId == id) n.copy(phase = phase) else n },
            ended = bounded,
        )
    }

    /** Hides [peer]'s card. */
    fun dismiss(peer: String): SendNoticeBook = copy(notices = notices - peer)

    /**
     * Stops cards (and remembered ends) claiming "will carry on" for a send the core no longer
     * lists as resumable ([listed], null when the core cannot be asked): the card then reads as
     * a plain failure, as the history entry does (`SendHistory.reconcile`).
     */
    fun reconcile(listed: Set<String>?): SendNoticeBook {
        if (listed == null) return this
        fun fix(id: String, p: SendPhase): SendPhase =
            if (p is SendPhase.FailedResumable && id !in listed) SendPhase.Failed(p.code, p.detail) else p
        return copy(
            notices = notices.mapValues { (_, n) -> n.copy(phase = fix(n.transferId, n.phase)) },
            ended = ended.mapValues { (id, p) -> fix(id, p) },
        )
    }

    companion object {
        /** How many ends are remembered for a card that is shown later. */
        const val MAX_ENDED = 32

        /** The phase an event moves a send to, or null for events that are not the end of a send. */
        fun phaseOf(ev: HostEvent, resumableOf: (String) -> Boolean, detailOf: (String) -> String?): Pair<String, SendPhase>? = when {
            ev is HostEvent.TransferComplete && ev.direction == TransferDirection.SEND -> ev.id to SendPhase.Sent
            ev is HostEvent.TransferFailed && ev.direction == TransferDirection.SEND ->
                ev.id to failedPhase(ev.code, ev.reason, resumableOf(ev.id), detailOf)
            else -> null
        }

        /** The phase of a send that stopped with [code]: cancelled, failed, or failed and kept to resume. */
        fun failedPhase(code: TransferFailure, reason: String, resumable: Boolean, detailOf: (String) -> String?): SendPhase {
            if (code == TransferFailure.CANCELLED_BY_YOU) return SendPhase.Cancelled
            val detail = if (code == TransferFailure.OTHER) detailOf(reason) else null
            return if (resumable) SendPhase.FailedResumable(code, detail) else SendPhase.Failed(code, detail)
        }
    }
}

/** The process-wide [SendNoticeBook]. */
object SendNotices {
    private val state = MutableStateFlow(SendNoticeBook())

    /** The book; screens read their device's card from it. */
    val book: StateFlow<SendNoticeBook> = state

    /** Shows the card for a send that started. */
    fun begin(peer: String, notice: SendNotice) = state.update { it.begin(peer, notice) }

    /** Hides [peer]'s card. */
    fun dismiss(peer: String) = state.update { it.dismiss(peer) }

    /** Moves the card of the send that [ev] ends, if [ev] ends one. */
    fun onEvent(ev: HostEvent, resumableOf: (String) -> Boolean, detailOf: (String) -> String?) {
        val (id, phase) = SendNoticeBook.phaseOf(ev, resumableOf, detailOf) ?: return
        state.update { it.end(id, phase) }
    }

    /** Applies [SendNoticeBook.reconcile]. */
    fun reconcile(listed: Set<String>?) = state.update { it.reconcile(listed) }

    private inline fun MutableStateFlow<SendNoticeBook>.update(f: (SendNoticeBook) -> SendNoticeBook) {
        while (true) {
            val before = value
            if (compareAndSet(before, f(before))) return
        }
    }
}
