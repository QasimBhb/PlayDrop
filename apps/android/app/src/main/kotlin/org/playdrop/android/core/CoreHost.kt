package org.playdrop.android.core

import android.content.Context
import android.net.Uri
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import org.playdrop.core.AddDeviceCandidate
import org.playdrop.core.HostEvent
import org.playdrop.core.MediaState
import org.playdrop.core.PairingPrompt
import org.playdrop.core.PeerSummary
import org.playdrop.core.PlayDropHost
import org.playdrop.core.TransferSnapshot

/**
 * The one process-wide handle on the core. The foreground service starts the core through
 * [start] and stops it through [stop]; every screen, receiver and activity reads state from
 * the flows and calls the core through [withHost]. Nothing else touches [PlayDropHost].
 *
 * Contract between the service agent (implements this file) and the UI agent (consumes it).
 * Keep the signatures; add members only with a doc comment.
 */
object CoreHost {
    /** True once [start] returned and the listener saw `Listening`. */
    val running: StateFlow<Boolean> get() = CoreHostImpl.running

    /** Paired peers, refreshed on every peer event. */
    val peers: StateFlow<List<PeerSummary>> get() = CoreHostImpl.peers

    /** Live and recent transfers, refreshed on every transfer event. */
    val transfers: StateFlow<List<TransferSnapshot>> get() = CoreHostImpl.transfers

    /** Devices seen in their "Add device" window, one row per address. */
    val addDeviceCandidates: StateFlow<List<AddDeviceCandidate>> get() = CoreHostImpl.addDeviceCandidates

    /** The pairing prompt waiting for an answer, if any. */
    val pairingPrompt: StateFlow<PendingPairing?> get() = CoreHostImpl.pairingPrompt

    /** Battery and presence last reported by each peer, keyed by hex Device ID. */
    val peerStatus: StateFlow<Map<String, PeerStatus>> get() = CoreHostImpl.peerStatus

    /**
     * Media players each peer reports, keyed by hex Device ID, in the order they first
     * appeared. A player that stops and lists no action is removed, and a peer's players are
     * forgotten when it goes offline; a peer with no players has no key.
     */
    val media: StateFlow<Map<String, List<MediaState>>> get() = CoreHostImpl.media

    /**
     * The real location of a delivered file: [location] from `TransferSnapshot.delivered`,
     * unless the Downloads mover had to give the file another name (see [Relocations]). Every
     * reader that shows or opens a delivered location goes through this.
     */
    fun relocated(transferId: String, location: String): String = CoreHostImpl.relocated(transferId, location)

    /** Counts up whenever [relocated] may answer differently, so rows can be derived again. */
    val relocationChanges: StateFlow<Int> get() = CoreHostImpl.relocationChanges

    /** Every event, in order, for screens that need more than the flows above. */
    val events: SharedFlow<HostEvent> get() = CoreHostImpl.events

    /** Our own identity once listening. */
    val identity: StateFlow<OwnIdentity?> get() = CoreHostImpl.identity

    /**
     * Starts the core if it is not running. Called by the foreground service only. Blocks
     * for the start-up; call from a background dispatcher.
     */
    fun start(context: Context) = CoreHostImpl.start(context)

    /** Stops the core. Called by the foreground service only. */
    suspend fun stop() = CoreHostImpl.stop()

    /**
     * Runs [block] against the live host. Throws [IllegalStateException] when the core is
     * not running; callers that may run with the service down (the share activity) must
     * call [ensureServiceRunning] first and wait for [running].
     */
    suspend fun <T> withHost(block: suspend (PlayDropHost) -> T): T = CoreHostImpl.withHost(block)

    /** Starts the foreground service if needed (no-op when it already runs). */
    fun ensureServiceRunning(context: Context) = CoreHostImpl.ensureServiceRunning(context)

    /**
     * Starts the service if needed and suspends until [running] is true, at most
     * [timeoutMs]. Returns false on timeout (tell the user; anti-pattern 18).
     */
    suspend fun awaitRunning(context: Context, timeoutMs: Long = 5_000): Boolean =
        CoreHostImpl.awaitRunning(context, timeoutMs)

    /**
     * Re-reads [peers] from the core. Grant and label changes raise no event, so call this
     * after `setGrants`, `setLabel` or `unpair` through [withHost].
     */
    suspend fun refreshPeers() = CoreHostImpl.refreshPeers()

    /**
     * Opens the core's 120 s "Add device" window and holds the Wi-Fi MulticastLock for it.
     * [addDeviceCandidates] fills as devices are seen and is cleared when the window closes
     * (by [closeAddDeviceWindow], a successful pairing, or the core's own timeout).
     * Use this instead of `withHost { it.openAddDeviceWindow() }`.
     */
    suspend fun openAddDeviceWindow() = CoreHostImpl.openAddDeviceWindow()

    /** Closes the "Add device" window, clears the candidates and releases its multicast hold. */
    suspend fun closeAddDeviceWindow() = CoreHostImpl.closeAddDeviceWindow()

    /**
     * Opens the window and returns the one-time QR pairing URI (SR-9: render it with
     * `FLAG_SECURE`, never log it). Same window bookkeeping as [openAddDeviceWindow].
     */
    suspend fun showQr(): String = CoreHostImpl.showQr()

    /** Answers [pairingPrompt] and clears it. Use instead of `withHost { it.pairingRespond(..) }`. */
    suspend fun answerPairing(accept: Boolean) = CoreHostImpl.answerPairing(accept)

    /**
     * Turns one reason for holding the Wi-Fi MulticastLock on or off (PLAN.md
     * anti-pattern 10: never hold it permanently). [DiscoveryReason.FOREGROUND] is managed
     * automatically from `MainActivity`'s start and stop, and [DiscoveryReason.ADD_DEVICE] by
     * [openAddDeviceWindow]; the UI only needs this for its own extra discovery screens.
     */
    fun setDiscoveryWindow(reason: DiscoveryReason, on: Boolean) = CoreHostImpl.setDiscoveryWindow(reason, on)

    /**
     * Sends shared or picked [uris] (`content://` or `file://`) to [peer] (hex Device ID).
     * Reads names and sizes, keeps each item readable until the transfer ends, and returns
     * the transfer ID. Throws `PlayDropException` (for example `Io` naming an unreadable
     * item) or [IllegalStateException] when the core is not running.
     */
    suspend fun sendUris(context: Context, peer: String, uris: List<Uri>): String =
        CoreHostImpl.sendUris(context, peer, uris)

    /** The local label of a paired peer, else its Short ID, else a truncated Device ID. */
    fun labelOf(peer: String): String = CoreHostImpl.labelOf(peer)
}

/** Why the Wi-Fi MulticastLock is wanted; the lock is held while any reason is on. */
enum class DiscoveryReason {
    /** The "Add device" window is open. */
    ADD_DEVICE,

    /** `MainActivity` is in the foreground. */
    FOREGROUND,

    /** The first 60 s after the core starts or the network changes (service-managed). */
    NETWORK,
}

/** A pairing prompt the user must answer (SR-8: show the Short ID next to the name). */
data class PendingPairing(
    val peer: String,
    val shortId: String,
    val name: String,
    val prompt: PairingPrompt,
)

/** The last battery and presence a peer reported. */
data class PeerStatus(
    val batteryLevel: Int? = null,
    val charging: Boolean = false,
    val batteryLow: Boolean = false,
    /** `active`, `idle`, `locked` or `asleep`. */
    val presence: String? = null,
)

/** This device as the core reports it. */
data class OwnIdentity(
    val deviceId: String,
    val shortId: String,
    val addr: String,
)
