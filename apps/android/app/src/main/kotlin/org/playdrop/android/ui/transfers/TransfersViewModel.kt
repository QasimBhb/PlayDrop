package org.playdrop.android.ui.transfers

import android.app.Application
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.playdrop.android.core.CoreHost
import org.playdrop.android.ui.common.CoreViewModel
import org.playdrop.core.HostEvent

/**
 * State and actions of the Transfers tab. Created at activity scope so it keeps the file
 * names of incoming offers (from `TransferOffer` events) while other tabs are shown.
 */
class TransfersViewModel(application: Application) : CoreViewModel(application) {
    private val offers = MutableStateFlow<Map<String, OfferInfo>>(emptyMap())

    /** Rows, newest first, offers waiting for an answer on top. */
    val rows: StateFlow<List<TransferRow>> = combine(
        CoreHost.transfers,
        CoreHost.peers,
        offers,
        CoreHost.relocationChanges,
    ) { t, p, o, _ ->
        // The last flow only says "ask the relocation table again": the mover may rename a
        // landed file after the transfer's row was already derived.
        TransferRows.derive(t, p, o, CoreHost::relocated)
    }.stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), emptyList())

    /** Incoming offers waiting for Accept or Decline, for the navigation badge. */
    val pendingOffers: StateFlow<Int> = CoreHost.transfers.map { TransferRows.pendingOffers(it) }
        .stateIn(viewModelScope, SharingStarted.Eagerly, 0)

    init {
        viewModelScope.launch {
            CoreHost.events.collect { e ->
                if (e is HostEvent.TransferOffer) {
                    val info = OfferInfo(
                        names = e.files.take(TransferRows.NAMES_SHOWN).map { it.path },
                        fileCount = e.fileCount.toLong(),
                        totalBytes = e.totalBytes.toLong(),
                        note = e.note,
                    )
                    offers.update { (it + (e.id to info)).entries.toList().takeLast(MAX_OFFERS).associate { x -> x.toPair() } }
                }
            }
        }
    }

    /** Cancels a transfer in either direction. */
    fun cancel(id: String) = launchCore({ it.cancelTransfer(id) })

    /** Accepts or declines an incoming offer. */
    fun respond(id: String, accept: Boolean) = launchCore({ it.respondOffer(id, accept) })

    private companion object {
        const val MAX_OFFERS = 64
    }
}
