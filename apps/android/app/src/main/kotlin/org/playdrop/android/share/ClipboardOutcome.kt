package org.playdrop.android.share

/**
 * What a manual clipboard push achieved, reduced to the one message the user sees. Pure, so
 * the wording rules are unit-tested.
 */
internal sealed class ClipboardOutcome {
    /** Sent to [count] devices. */
    data class Sent(val count: Int) : ClipboardOutcome()

    /** The echo rule skipped it: this clip came from a peer or was just sent. */
    data object AlreadySent : ClipboardOutcome()

    /** No paired device at all. */
    data object NoPairedDevice : ClipboardOutcome()

    /** Devices are online but none has the clipboard-out grant. */
    data object NoGrant : ClipboardOutcome()

    /** Nobody eligible is online. */
    data object NoDeviceOnline : ClipboardOutcome()

    companion object {
        /** Picks the outcome from the core's `ClipboardPush` counters. */
        fun of(
            sentTo: Int,
            skippedEcho: Boolean,
            skippedOffline: Int,
            skippedNoGrant: Int,
            pairedCount: Int,
        ): ClipboardOutcome = when {
            sentTo > 0 -> Sent(sentTo)
            skippedEcho -> AlreadySent
            pairedCount == 0 -> NoPairedDevice
            skippedNoGrant > 0 && skippedOffline == 0 -> NoGrant
            else -> NoDeviceOnline
        }
    }
}
